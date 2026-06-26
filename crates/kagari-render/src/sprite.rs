//! The MonochromeSprite primitive: GPU instance layout, packing from the resolved
//! `MonochromeSprite`, and the cached pipeline that samples the R8 atlas array.
//!
//! Mirrors `quad.rs`. The fragment shader multiplies the atlas coverage (sampled
//! linearly from a `texture_2d_array<r8>`, decision Q1/Q4) by a premultiplied-linear
//! color and the shared content-mask clip. The atlas bind group (group 1) is owned by
//! the `Renderer` (it depends on the atlas texture view) and passed to `draw`.

use bytemuck::{Pod, Zeroable};
use kagari_base::{Color, Corners, Rect};

use crate::scene::{Batch, MonochromeSprite, Scene};

/// The shared SDF/AA prelude composed ahead of the sprite shader (wgsl.md §1).
const SPRITE_SHADER_SRC: &str = concat!(
    include_str!("shaders/sdf.wgsl"),
    "\n",
    include_str!("shaders/sprite.wgsl"),
);

/// GPU instance for a sprite — byte-matched to `sprite.wgsl`'s `InstanceSprite`
/// (96 bytes, 16-byte aligned, no padding gaps so it is `Pod`). The fields are read
/// by the vertex shader through the vertex layout, not in Rust.
#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct InstanceSprite {
    bounds: [f32; 4],
    uv: [f32; 4],
    color: [f32; 4],
    mask_offset: [f32; 2],
    mask_half: [f32; 2],
    mask_radii: [f32; 4],
    page: u32,
    _pad: [u32; 3],
}

fn color_arr(c: Color) -> [f32; 4] {
    [c.r, c.g, c.b, c.a]
}

fn rect_arr(r: Rect) -> [f32; 4] {
    [r.origin.x, r.origin.y, r.size.w, r.size.h]
}

fn corners_arr(c: Corners) -> [f32; 4] {
    [c.tl, c.tr, c.br, c.bl]
}

impl InstanceSprite {
    fn from_sprite(s: &MonochromeSprite) -> Self {
        let center = [
            s.bounds.origin.x + s.bounds.size.w * 0.5,
            s.bounds.origin.y + s.bounds.size.h * 0.5,
        ];
        let m = s.content_mask.rect;
        let mask_center = [m.origin.x + m.size.w * 0.5, m.origin.y + m.size.h * 0.5];
        Self {
            bounds: rect_arr(s.bounds),
            uv: [s.tex.min[0], s.tex.min[1], s.tex.max[0], s.tex.max[1]],
            color: color_arr(s.color),
            mask_offset: [center[0] - mask_center[0], center[1] - mask_center[1]],
            mask_half: [m.size.w * 0.5, m.size.h * 0.5],
            mask_radii: corners_arr(s.content_mask.radii),
            page: s.tex.page,
            _pad: [0; 3],
        }
    }
}

/// Frame globals (uniform): physical viewport size + logical→physical scale. Mirrors
/// `quad.rs`'s `Globals` (a future refactor could share one group-0 resource).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    viewport: [f32; 2],
    scale: f32,
    _pad: f32,
}

const INITIAL_INSTANCE_CAP: u32 = 64;

/// The sprite pipeline plus its globals, atlas bind-group layout + sampler, and a
/// growable instance buffer.
pub(crate) struct SpriteRenderer {
    pipeline: wgpu::RenderPipeline,
    globals_buffer: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    atlas_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    instances: Vec<InstanceSprite>,
    instance_buffer: wgpu::Buffer,
    instance_cap: u32,
}

impl SpriteRenderer {
    pub(crate) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kagari.sprite.shader"),
            source: wgpu::ShaderSource::Wgsl(SPRITE_SHADER_SRC.into()),
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kagari.sprite.globals_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kagari.sprite.globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kagari.sprite.globals_bind"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        // Group 1: the R8 atlas array + a linear sampler (decision Q1/Q4).
        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kagari.sprite.atlas_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kagari.sprite.atlas_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kagari.sprite.pipeline_layout"),
            bind_group_layouts: &[Some(&globals_layout), Some(&atlas_layout)],
            immediate_size: 0,
        });

        // 7 instance attributes (3 vec4 + 2 vec2 + 1 vec4 + 1 u32); the `_pad` tail is
        // covered by `array_stride` so the GPU stride matches `size_of::<InstanceSprite>()`.
        const ATTRS: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x4,
            3 => Float32x2, 4 => Float32x2, 5 => Float32x4, 6 => Uint32,
        ];
        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceSprite>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kagari.sprite.pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[instance_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    // Premultiplied-alpha "over" blend (gpu.md §6), same as quad.
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kagari.sprite.instances"),
            size: u64::from(INITIAL_INSTANCE_CAP) * std::mem::size_of::<InstanceSprite>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            globals_buffer,
            globals_bind,
            atlas_layout,
            sampler,
            instances: Vec::new(),
            instance_buffer,
            instance_cap: INITIAL_INSTANCE_CAP,
        }
    }

    /// Build the group-1 bind group for the atlas array view + the linear sampler.
    /// Owned by the `Renderer` and rebuilt when the atlas texture is re-created.
    pub(crate) fn make_atlas_bind(
        &self,
        device: &wgpu::Device,
        atlas_view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kagari.sprite.atlas_bind"),
            layout: &self.atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// Pack the scene's glyphs (already order-sorted by `Scene::batches`), grow the
    /// instance buffer if needed, and upload globals + instances.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        viewport: (u32, u32),
        scale: f32,
    ) {
        self.instances.clear();
        self.instances
            .extend(scene.glyphs.iter().map(InstanceSprite::from_sprite));

        let needed = self.instances.len() as u32;
        if needed > self.instance_cap {
            let new_cap = needed.next_power_of_two();
            self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kagari.sprite.instances"),
                size: u64::from(new_cap) * std::mem::size_of::<InstanceSprite>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instance_cap = new_cap;
        }

        let globals = Globals {
            viewport: [viewport.0 as f32, viewport.1 as f32],
            scale,
            _pad: 0.0,
        };
        queue.write_buffer(&self.globals_buffer, 0, bytemuck::bytes_of(&globals));
        if !self.instances.is_empty() {
            queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.instances),
            );
        }
    }

    /// Draw one sprite batch (a `range` of instances) as `range.len()` instanced
    /// triangle-strips, sampling `atlas_bind` (group 1).
    pub(crate) fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        batch: &Batch,
        atlas_bind: &wgpu::BindGroup,
    ) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.globals_bind, &[]);
        pass.set_bind_group(1, atlas_bind, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..4, batch.range.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprite_shader_should_pass_naga_validation() {
        let module = wgpu::naga::front::wgsl::parse_str(SPRITE_SHADER_SRC)
            .expect("sprite WGSL should parse");
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .expect("sprite WGSL should validate");
    }

    #[test]
    fn instance_sprite_should_be_96_bytes() {
        assert_eq!(std::mem::size_of::<InstanceSprite>(), 96);
        assert_eq!(bytemuck::bytes_of(&InstanceSprite::zeroed()).len(), 96);
    }
}
