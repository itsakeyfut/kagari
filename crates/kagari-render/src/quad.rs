//! The Quad primitive: GPU instance layout, packing from the resolved `Quad`,
//! and the cached pipeline + growable instance buffer.
//!
//! Phase #12 renders a solid fill. The 160-byte `InstanceQuad` layout and the
//! full vertex-attribute wiring are established now (agreed in the spec), so the
//! SDF rounded rect / per-edge border / AA (#13), gradient (#14), and content-mask
//! clip (#15) only add shader logic — no layout churn.

use bytemuck::{Pod, Zeroable};
use kagari_base::{Color, Corners, Edges, Rect};

use crate::scene::{Background, Batch, Quad, Scene};

/// GPU instance for a quad — byte-matched to `quad.wgsl`'s `InstanceQuad`
/// (160 bytes, 16-byte aligned, no padding gaps so it is `Pod`).
///
/// The fields are consumed by the vertex shader through the vertex layout, not
/// read in Rust, so dead-code analysis is suppressed for the whole struct.
#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct InstanceQuad {
    bounds: [f32; 4],
    corner_radii: [f32; 4],
    bg_color: [f32; 4],
    bg_grad_color: [f32; 4],
    bg_grad_dir: [f32; 4],
    border_widths: [f32; 4],
    border_color: [f32; 4],
    mask_bounds: [f32; 4],
    mask_radii: [f32; 4],
    flags: u32,
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

fn edges_arr(e: Edges) -> [f32; 4] {
    [e.top, e.right, e.bottom, e.left]
}

impl InstanceQuad {
    fn from_quad(q: &Quad) -> Self {
        let (bg_color, bg_grad_color, bg_grad_dir, flags) = match q.bg {
            Background::Solid(c) => (color_arr(c), [0.0; 4], [0.0; 4], 0),
            Background::LinearGradient {
                start,
                end,
                start_point,
                end_point,
            } => (
                color_arr(start),
                color_arr(end),
                [start_point.x, start_point.y, end_point.x, end_point.y],
                1,
            ),
        };
        Self {
            bounds: rect_arr(q.bounds),
            corner_radii: corners_arr(q.corner_radii),
            bg_color,
            bg_grad_color,
            bg_grad_dir,
            border_widths: edges_arr(q.border.widths),
            border_color: color_arr(q.border.color),
            mask_bounds: rect_arr(q.content_mask.rect),
            mask_radii: corners_arr(q.content_mask.radii),
            flags,
            _pad: [0; 3],
        }
    }
}

/// Frame globals (uniform): physical viewport size + logical→physical scale.
/// `PartialEq` lets `prepare` skip the upload when the viewport/scale are unchanged.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Pod, Zeroable)]
struct Globals {
    viewport: [f32; 2],
    scale: f32,
    _pad: f32,
}

/// The Quad shader: the shared SDF/AA prelude composed ahead of the quad shader
/// (wgsl.md §1). `concat!` keeps the composed source available to the build/dev
/// Naga validation test so it validates the exact bytes shipped to the GPU.
const QUAD_SHADER_SRC: &str = concat!(
    include_str!("shaders/sdf.wgsl"),
    "\n",
    include_str!("shaders/quad.wgsl"),
);

const INITIAL_INSTANCE_CAP: u32 = 64;

/// The Quad pipeline plus its globals + growable instance buffer.
pub(crate) struct QuadRenderer {
    pipeline: wgpu::RenderPipeline,
    globals_buffer: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    instances: Vec<InstanceQuad>,
    instance_buffer: wgpu::Buffer,
    instance_cap: u32,
    /// Last-uploaded globals; the uniform is re-written only when it changes
    /// (viewport/scale change on resize, not every frame). `None` until the first
    /// upload and after device-loss recreation (forces a fresh write).
    last_globals: Option<Globals>,
}

impl QuadRenderer {
    pub(crate) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kagari.quad.shader"),
            source: wgpu::ShaderSource::Wgsl(QUAD_SHADER_SRC.into()),
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kagari.quad.globals_layout"),
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
            label: Some("kagari.quad.globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kagari.quad.globals_bind"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kagari.quad.pipeline_layout"),
            bind_group_layouts: &[Some(&globals_layout)],
            immediate_size: 0,
        });

        // All 10 instance attributes (9 vec4 + 1 u32); the `_pad` tail is covered by
        // `array_stride` so the GPU stride matches `size_of::<InstanceQuad>()`.
        const ATTRS: [wgpu::VertexAttribute; 10] = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4,
            5 => Float32x4, 6 => Float32x4, 7 => Float32x4, 8 => Float32x4, 9 => Uint32,
        ];
        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceQuad>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kagari.quad.pipeline"),
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
            label: Some("kagari.quad.instances"),
            size: u64::from(INITIAL_INSTANCE_CAP) * std::mem::size_of::<InstanceQuad>() as u64,
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

    /// Pack the scene's quads (already order-sorted by `Scene::batches`, which the
    /// renderer calls once across all kinds), grow the instance buffer if needed, and
    /// upload globals + instances.
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
            .extend(scene.quads.iter().map(InstanceQuad::from_quad));

        let needed = self.instances.len() as u32;
        if needed > self.instance_cap {
            let new_cap = needed.next_power_of_two();
            self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kagari.quad.instances"),
                size: u64::from(new_cap) * std::mem::size_of::<InstanceQuad>() as u64,
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
        // Re-upload globals only when viewport/scale changed (typically just on resize).
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

    /// Draw one quad batch (a `range` of instances) as `range.len()` instanced
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
    use crate::scene::{Border, RoundedRect};
    use kagari_base::Point;

    fn solid_quad() -> Quad {
        Quad {
            bounds: Rect::from_xywh(10.0, 20.0, 100.0, 50.0),
            corner_radii: Corners {
                tl: 1.0,
                tr: 2.0,
                br: 3.0,
                bl: 4.0,
            },
            bg: Background::Solid(Color::new(1.0, 0.0, 0.0, 1.0)),
            border: Border {
                widths: Edges {
                    top: 1.0,
                    right: 2.0,
                    bottom: 3.0,
                    left: 4.0,
                },
                color: Color::new(0.0, 1.0, 0.0, 1.0),
            },
            content_mask: RoundedRect {
                rect: Rect::from_xywh(0.0, 0.0, 200.0, 100.0),
                // Distinct non-zero values so a tl/tr/br/bl swap or a
                // corner_radii/mask_radii mix-up is caught by the packing test.
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
    fn quad_shader_should_pass_naga_validation() {
        // Parse + validate the composed shader (prelude + quad) without a GPU, so
        // WGSL errors fail `cargo test` in CI rather than surfacing at device init.
        // `wgpu::naga` is re-exported, so its version always matches wgpu's.
        let module =
            wgpu::naga::front::wgsl::parse_str(QUAD_SHADER_SRC).expect("quad WGSL should parse");
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .expect("quad WGSL should validate");
    }

    #[test]
    fn instance_quad_should_be_160_bytes() {
        assert_eq!(std::mem::size_of::<InstanceQuad>(), 160);
        let inst = InstanceQuad::from_quad(&solid_quad());
        assert_eq!(bytemuck::bytes_of(&inst).len(), 160);
    }

    #[test]
    fn quad_should_pack_solid_into_instance() {
        let inst = InstanceQuad::from_quad(&solid_quad());
        assert_eq!(inst.bounds, [10.0, 20.0, 100.0, 50.0]);
        assert_eq!(inst.corner_radii, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(inst.bg_color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(inst.border_widths, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(inst.border_color, [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(inst.mask_bounds, [0.0, 0.0, 200.0, 100.0]);
        assert_eq!(inst.mask_radii, [5.0, 6.0, 7.0, 8.0]);
        assert_eq!(inst.flags, 0);
        // Solid contract: the gradient fields are zeroed.
        assert_eq!(inst.bg_grad_color, [0.0; 4]);
        assert_eq!(inst.bg_grad_dir, [0.0; 4]);
    }

    #[test]
    fn quad_should_pack_gradient_into_instance() {
        let mut q = solid_quad();
        q.bg = Background::LinearGradient {
            start: Color::new(1.0, 0.0, 0.0, 1.0),
            end: Color::new(0.0, 0.0, 1.0, 1.0),
            start_point: Point::new(0.0, 0.0),
            end_point: Point::new(1.0, 1.0),
        };
        let inst = InstanceQuad::from_quad(&q);
        assert_eq!(inst.flags & 1, 1);
        assert_eq!(inst.bg_color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(inst.bg_grad_color, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(inst.bg_grad_dir, [0.0, 0.0, 1.0, 1.0]);
    }
}
