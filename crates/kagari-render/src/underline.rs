//! The Underline primitive: GPU instance layout, packing from the resolved
//! `Underline`, and the cached pipeline + growable instance buffer.
//!
//! Mirrors `quad.rs` (group-0 globals only, no atlas). The fragment shader fills a
//! band with ~1px SDF AA on its edges; `Dotted` modulates the band by a periodic
//! along-axis threshold. Linear premultiplied; the content-mask clip is shared via
//! `sdf.wgsl`.

use bytemuck::{Pod, Zeroable};
use kagari_base::{Color, Corners, Rect};

use crate::scene::{Batch, Scene, Underline, UnderlineStyle};

/// The shared SDF/AA prelude composed ahead of the underline shader (wgsl.md §1).
const UNDERLINE_SHADER_SRC: &str = concat!(
    include_str!("shaders/sdf.wgsl"),
    "\n",
    include_str!("shaders/underline.wgsl"),
);

/// GPU instance for an underline — byte-matched to `underline.wgsl`'s
/// `InstanceUnderline` (80 bytes, 16-byte aligned, no padding gaps so it is `Pod`).
/// The fields are read by the vertex shader through the vertex layout, not in Rust.
#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct InstanceUnderline {
    bounds: [f32; 4],
    color: [f32; 4],
    mask_offset: [f32; 2],
    mask_half: [f32; 2],
    mask_radii: [f32; 4],
    flags: u32,
    period: f32,
    _pad: [u32; 2],
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

impl InstanceUnderline {
    fn from_underline(u: &Underline) -> Self {
        let center = [
            u.rect.origin.x + u.rect.size.w * 0.5,
            u.rect.origin.y + u.rect.size.h * 0.5,
        ];
        let m = u.content_mask.rect;
        let mask_center = [m.origin.x + m.size.w * 0.5, m.origin.y + m.size.h * 0.5];
        let (flags, period) = match u.style {
            UnderlineStyle::Solid => (0, 0.0),
            // Square-ish segments spaced one width apart (50% duty in the shader).
            UnderlineStyle::Dotted => (1, u.thickness * 2.0),
        };
        Self {
            bounds: rect_arr(u.rect),
            color: color_arr(u.color),
            mask_offset: [center[0] - mask_center[0], center[1] - mask_center[1]],
            mask_half: [m.size.w * 0.5, m.size.h * 0.5],
            mask_radii: corners_arr(u.content_mask.radii),
            flags,
            period,
            _pad: [0; 2],
        }
    }
}

/// Frame globals (uniform): physical viewport size + logical→physical scale. Mirrors
/// `quad.rs`'s `Globals`. `PartialEq` lets `prepare` skip the upload when unchanged.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Pod, Zeroable)]
struct Globals {
    viewport: [f32; 2],
    scale: f32,
    _pad: f32,
}

const INITIAL_INSTANCE_CAP: u32 = 64;

/// The underline pipeline plus its globals + growable instance buffer.
pub(crate) struct UnderlineRenderer {
    pipeline: wgpu::RenderPipeline,
    globals_buffer: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    instances: Vec<InstanceUnderline>,
    instance_buffer: wgpu::Buffer,
    instance_cap: u32,
    /// Last-uploaded globals; re-written only on change (resize), `None` after
    /// device-loss recreation to force a fresh write.
    last_globals: Option<Globals>,
}

impl UnderlineRenderer {
    pub(crate) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kagari.underline.shader"),
            source: wgpu::ShaderSource::Wgsl(UNDERLINE_SHADER_SRC.into()),
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kagari.underline.globals_layout"),
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
            label: Some("kagari.underline.globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kagari.underline.globals_bind"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kagari.underline.pipeline_layout"),
            bind_group_layouts: &[Some(&globals_layout)],
            immediate_size: 0,
        });

        // 7 instance attributes (2 vec4 + 2 vec2 + 1 vec4 + u32 + f32); the `_pad`
        // tail is covered by `array_stride` so the GPU stride matches the struct size.
        const ATTRS: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x2, 3 => Float32x2,
            4 => Float32x4, 5 => Uint32, 6 => Float32,
        ];
        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceUnderline>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kagari.underline.pipeline"),
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
            label: Some("kagari.underline.instances"),
            size: u64::from(INITIAL_INSTANCE_CAP) * std::mem::size_of::<InstanceUnderline>() as u64,
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

    /// Pack the scene's underlines (already order-sorted by `Scene::batches_into`),
    /// grow the instance buffer if needed, and upload globals + instances.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        viewport: (u32, u32),
        scale: f32,
    ) {
        self.instances.clear();
        self.instances.extend(
            scene
                .underlines
                .iter()
                .map(InstanceUnderline::from_underline),
        );

        let needed = self.instances.len() as u32;
        if needed > self.instance_cap {
            let new_cap = needed.next_power_of_two();
            self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kagari.underline.instances"),
                size: u64::from(new_cap) * std::mem::size_of::<InstanceUnderline>() as u64,
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

    /// Draw one underline batch (a `range` of instances) as `range.len()` instanced
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

    fn underline(style: UnderlineStyle) -> Underline {
        Underline {
            rect: Rect::from_xywh(10.0, 20.0, 40.0, 2.0),
            color: Color::new(0.2, 0.4, 0.6, 1.0),
            style,
            thickness: 2.0,
            content_mask: RoundedRect {
                rect: Rect::from_xywh(0.0, 0.0, 100.0, 100.0),
                radii: Corners {
                    tl: 5.0,
                    tr: 6.0,
                    br: 7.0,
                    bl: 8.0,
                },
            },
            order: 0,
        }
    }

    #[test]
    fn instance_underline_should_be_80_bytes() {
        assert_eq!(std::mem::size_of::<InstanceUnderline>(), 80);
        assert_eq!(bytemuck::bytes_of(&InstanceUnderline::zeroed()).len(), 80);
    }

    #[test]
    fn underline_should_pack_solid_into_instance() {
        let inst = InstanceUnderline::from_underline(&underline(UnderlineStyle::Solid));
        assert_eq!(inst.bounds, [10.0, 20.0, 40.0, 2.0]);
        assert_eq!(inst.color, [0.2, 0.4, 0.6, 1.0]);
        // band_center (30, 21) - mask_center (50, 50).
        assert_eq!(inst.mask_offset, [-20.0, -29.0]);
        assert_eq!(inst.mask_half, [50.0, 50.0]);
        assert_eq!(inst.mask_radii, [5.0, 6.0, 7.0, 8.0]);
        // Solid contract: no dotted modulation.
        assert_eq!(inst.flags, 0);
        assert_eq!(inst.period, 0.0);
    }

    #[test]
    fn underline_should_pack_dotted_into_instance() {
        let inst = InstanceUnderline::from_underline(&underline(UnderlineStyle::Dotted));
        assert_eq!(inst.flags, 1);
        // period = thickness * 2.
        assert_eq!(inst.period, 4.0);
    }

    #[test]
    fn underline_shader_should_pass_naga_validation() {
        let module = wgpu::naga::front::wgsl::parse_str(UNDERLINE_SHADER_SRC)
            .expect("underline WGSL should parse");
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .expect("underline WGSL should validate");
    }
}
