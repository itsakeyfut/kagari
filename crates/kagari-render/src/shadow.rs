//! The Shadow primitive (#155): GPU instance layout, packing from the resolved
//! `Shadow`, and the cached pipeline + growable instance buffer. Mirrors `quad.rs`;
//! the shader is the analytic erf-Gaussian rounded-box drop shadow (specs §2.5).

use bytemuck::{Pod, Zeroable};
use kagari_base::{Color, Corners, Rect};

use crate::scene::{Batch, Scene, Shadow};

/// GPU instance for a shadow — byte-matched to `shadow.wgsl`'s `InstanceShadow`
/// (96 bytes, 16-byte aligned, no padding gaps so it is `Pod`). Consumed by the vertex
/// shader through the vertex layout, not read in Rust.
#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct InstanceShadow {
    bounds: [f32; 4],
    corner_radii: [f32; 4],
    /// offset.x, offset.y, blur, spread (logical px).
    shadow: [f32; 4],
    color: [f32; 4],
    mask_bounds: [f32; 4],
    mask_radii: [f32; 4],
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

impl InstanceShadow {
    fn from_shadow(s: &Shadow) -> Self {
        Self {
            bounds: rect_arr(s.bounds),
            corner_radii: corners_arr(s.corner_radii),
            shadow: [s.offset.x, s.offset.y, s.blur, s.spread],
            color: color_arr(s.color),
            mask_bounds: rect_arr(s.content_mask.rect),
            mask_radii: corners_arr(s.content_mask.radii),
        }
    }
}

/// Frame globals (uniform): physical viewport size + logical→physical scale. Identical
/// layout to the quad pipeline's globals; `PartialEq` lets `prepare` skip the upload
/// when unchanged.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Pod, Zeroable)]
struct Globals {
    viewport: [f32; 2],
    scale: f32,
    _pad: f32,
}

/// The Shadow shader: the shared SDF/AA prelude composed ahead of the shadow shader
/// (wgsl.md §1). `concat!` keeps the composed source available to the Naga validation
/// test so it validates the exact bytes shipped to the GPU.
const SHADOW_SHADER_SRC: &str = concat!(
    include_str!("shaders/sdf.wgsl"),
    "\n",
    include_str!("shaders/shadow.wgsl"),
);

const INITIAL_INSTANCE_CAP: u32 = 64;

/// The Shadow pipeline plus its globals + growable instance buffer.
pub(crate) struct ShadowRenderer {
    pipeline: wgpu::RenderPipeline,
    globals_buffer: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    instances: Vec<InstanceShadow>,
    instance_buffer: wgpu::Buffer,
    instance_cap: u32,
    last_globals: Option<Globals>,
}

impl ShadowRenderer {
    pub(crate) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kagari.shadow.shader"),
            source: wgpu::ShaderSource::Wgsl(SHADOW_SHADER_SRC.into()),
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kagari.shadow.globals_layout"),
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
            label: Some("kagari.shadow.globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kagari.shadow.globals_bind"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kagari.shadow.pipeline_layout"),
            bind_group_layouts: &[Some(&globals_layout)],
            immediate_size: 0,
        });

        // 6 vec4 instance attributes; `array_stride` matches `size_of::<InstanceShadow>()`.
        const ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4,
            5 => Float32x4,
        ];
        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceShadow>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kagari.shadow.pipeline"),
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
                    // Premultiplied-alpha "over" blend (gpu.md §6).
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
            label: Some("kagari.shadow.instances"),
            size: u64::from(INITIAL_INSTANCE_CAP) * std::mem::size_of::<InstanceShadow>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            globals_buffer,
            globals_bind,
            instances: Vec::new(),
            instance_buffer,
            instance_cap: INITIAL_INSTANCE_CAP,
            last_globals: None,
        }
    }

    /// Pack the scene's shadows (already order-sorted by `Scene::batches_into`), grow the
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
            .extend(scene.shadows.iter().map(InstanceShadow::from_shadow));

        let needed = self.instances.len() as u32;
        if needed > self.instance_cap {
            let new_cap = needed.next_power_of_two();
            self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kagari.shadow.instances"),
                size: u64::from(new_cap) * std::mem::size_of::<InstanceShadow>() as u64,
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
        if self.last_globals != Some(globals) {
            queue.write_buffer(&self.globals_buffer, 0, bytemuck::bytes_of(&globals));
            self.last_globals = Some(globals);
        }
        if !self.instances.is_empty() {
            queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.instances),
            );
        }
    }

    /// Draw one shadow batch (a `range` of instances) as `range.len()` instanced
    /// triangle-strips.
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, batch: &Batch) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.globals_bind, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..4, batch.range.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::RoundedRect;
    use kagari_base::Point;

    fn sample_shadow() -> Shadow {
        Shadow {
            bounds: Rect::from_xywh(10.0, 20.0, 100.0, 50.0),
            corner_radii: Corners {
                tl: 8.0,
                tr: 8.0,
                br: 8.0,
                bl: 8.0,
            },
            offset: Point::new(0.0, 4.0),
            blur: 6.0,
            spread: -1.0,
            color: Color::new(0.0, 0.0, 0.0, 0.25),
            content_mask: RoundedRect {
                rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
                radii: Corners::default(),
            },
            order: 0,
        }
    }

    #[test]
    fn shadow_shader_should_pass_naga_validation() {
        // Parse + validate the composed shader (prelude + shadow) without a GPU, so WGSL
        // errors fail `cargo test` in CI rather than surfacing at device init.
        let module = wgpu::naga::front::wgsl::parse_str(SHADOW_SHADER_SRC)
            .expect("shadow WGSL should parse");
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .expect("shadow WGSL should validate");
    }

    #[test]
    fn instance_shadow_should_be_96_bytes() {
        assert_eq!(std::mem::size_of::<InstanceShadow>(), 96);
        let inst = InstanceShadow::from_shadow(&sample_shadow());
        assert_eq!(bytemuck::bytes_of(&inst).len(), 96);
    }

    #[test]
    fn shadow_should_pack_into_instance() {
        let inst = InstanceShadow::from_shadow(&sample_shadow());
        assert_eq!(inst.bounds, [10.0, 20.0, 100.0, 50.0]);
        assert_eq!(inst.corner_radii, [8.0, 8.0, 8.0, 8.0]);
        // offset.x, offset.y, blur, spread.
        assert_eq!(inst.shadow, [0.0, 4.0, 6.0, -1.0]);
        assert_eq!(inst.color, [0.0, 0.0, 0.0, 0.25]);
        assert_eq!(inst.mask_bounds, [0.0, 0.0, 1.0e4, 1.0e4]);
    }
}
