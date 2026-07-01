//! Path primitive (#58): a kagari [`PathBuilder`] tessellates fills/strokes with `lyon` into a
//! [`PathVertex`](crate::scene::PathVertex) mesh with **feathered per-vertex AA**, plus the pipeline
//! ([`PathRenderer`]) that draws them — indexed, non-instanced — in the offscreen pass.
//!
//! lyon is fully hidden behind `PathBuilder` (design.md §1 — no external types on the public surface).
//!
//! **Feather AA** (single-pass; the fragment computes `coverage = clamp(cov_a - abs(cov_b), 0, 1)`):
//! - **fill**: `FillTessellator` fills the interior at `cov_a = 1`; a `StrokeTessellator` outline of the
//!   same path at `FEATHER_PX` width adds a fringe whose inner side is `cov_a = 1` and outer side
//!   `cov_a = 0`, so coverage ramps `1 → 0` across ~1px. `cov_b = 0`.
//! - **stroke**: `StrokeTessellator` at `width + FEATHER_PX`; each vertex carries `cov_a = width/2 + 0.5`
//!   (flat) and `cov_b = signed distance from the centerline`. Interpolated across the band, coverage is
//!   solid in the core and ramps to 0 over the outer ~1px, centered on the nominal edge.

use kagari_base::{Color, Corners, Point, Rect};
use lyon::math::point as lpoint;
use lyon::path::{Path as LyonPath, Side};
use lyon::tessellation::{
    BuffersBuilder, FillOptions, FillTessellator, FillVertex, FillVertexConstructor, StrokeOptions,
    StrokeTessellator, StrokeVertex, StrokeVertexConstructor, VertexBuffers,
};

use crate::scene::{Batch, PathPrim, PathVertex, RoundedRect, Scene};

/// AA fringe width, in logical px (a 1px feather centered on each edge).
const FEATHER_PX: f32 = 1.0;

// ---------------------------------------------------------------------------
// PathBuilder (public) — records path commands, tessellates on fill()/stroke().
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum PathCmd {
    MoveTo(Point),
    LineTo(Point),
    QuadTo(Point, Point),
    CubicTo(Point, Point, Point),
    Close,
}

/// Builds an arbitrary path (in logical px) and tessellates it into a [`PathPrim`]. Curves are flattened
/// by lyon at tessellation time. Bundled/hidden lyon.
#[derive(Default)]
pub struct PathBuilder {
    cmds: Vec<PathCmd>,
}

impl PathBuilder {
    /// A new, empty path builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a new subpath at `p`.
    pub fn move_to(&mut self, p: Point) -> &mut Self {
        self.cmds.push(PathCmd::MoveTo(p));
        self
    }

    /// Adds a line segment to `p`.
    pub fn line_to(&mut self, p: Point) -> &mut Self {
        self.cmds.push(PathCmd::LineTo(p));
        self
    }

    /// Adds a quadratic Bézier to `p` with control point `ctrl`.
    pub fn quad_to(&mut self, ctrl: Point, p: Point) -> &mut Self {
        self.cmds.push(PathCmd::QuadTo(ctrl, p));
        self
    }

    /// Adds a cubic Bézier to `p` with control points `c1`, `c2`.
    pub fn cubic_to(&mut self, c1: Point, c2: Point, p: Point) -> &mut Self {
        self.cmds.push(PathCmd::CubicTo(c1, c2, p));
        self
    }

    /// Closes the current subpath (connects back to its start).
    pub fn close(&mut self) -> &mut Self {
        self.cmds.push(PathCmd::Close);
        self
    }

    /// Tessellates the path as a **filled** shape with the given premultiplied-linear `color`, content
    /// mask, and painter order.
    ///
    /// The interior is filled winding-independently (non-zero rule), but the **1px AA fringe assumes a
    /// clockwise outline** (in y-down logical space, matching a natural top-left → clockwise trace). A
    /// counter-clockwise contour inverts the fringe ramp — the edge loses its AA and the shape grows
    /// ~0.5px with a hard outer edge. Trace fill outlines clockwise; winding-independent fringing is a
    /// planned refinement.
    pub fn fill(&self, color: Color, content_mask: RoundedRect, order: u32) -> PathPrim {
        tessellate_fill(&self.build(), color, content_mask, order)
    }

    /// Tessellates the path as a **stroked** outline of the given `width` (logical px).
    pub fn stroke(
        &self,
        width: f32,
        color: Color,
        content_mask: RoundedRect,
        order: u32,
    ) -> PathPrim {
        tessellate_stroke(&self.build(), width, color, content_mask, order)
    }

    /// Replays the recorded commands into a lyon path (a subpath left open is ended un-closed).
    fn build(&self) -> LyonPath {
        let mut b = LyonPath::builder();
        let mut open = false;
        for cmd in &self.cmds {
            match *cmd {
                PathCmd::MoveTo(p) => {
                    if open {
                        b.end(false);
                    }
                    b.begin(lpoint(p.x, p.y));
                    open = true;
                }
                PathCmd::LineTo(p) => {
                    if open {
                        b.line_to(lpoint(p.x, p.y));
                    }
                }
                PathCmd::QuadTo(c, p) => {
                    if open {
                        b.quadratic_bezier_to(lpoint(c.x, c.y), lpoint(p.x, p.y));
                    }
                }
                PathCmd::CubicTo(c1, c2, p) => {
                    if open {
                        b.cubic_bezier_to(lpoint(c1.x, c1.y), lpoint(c2.x, c2.y), lpoint(p.x, p.y));
                    }
                }
                PathCmd::Close => {
                    if open {
                        b.end(true);
                        open = false;
                    }
                }
            }
        }
        if open {
            b.end(false);
        }
        b.build()
    }
}

// ---------------------------------------------------------------------------
// Tessellation → PathVertex, with the feather.
// ---------------------------------------------------------------------------

fn color_arr(c: Color) -> [f32; 4] {
    [c.r, c.g, c.b, c.a]
}

/// The mask attributes baked (flat) into every path vertex. Paths use **absolute** logical-px positions,
/// so `mask_offset = -mask_center` makes the shader's `mp = world + mask_offset = world - mask_center`
/// (mask-relative), matching `sdf.wgsl::mask_coverage`.
fn mask_attrs(mask: RoundedRect) -> ([f32; 2], [f32; 2], [f32; 4]) {
    let m: Rect = mask.rect;
    let center = [m.origin.x + m.size.w * 0.5, m.origin.y + m.size.h * 0.5];
    let radii: Corners = mask.radii;
    (
        [-center[0], -center[1]],
        [m.size.w * 0.5, m.size.h * 0.5],
        [radii.tl, radii.tr, radii.br, radii.bl],
    )
}

/// Fill interior: `cov_a = 1`, `cov_b = 0`.
struct FillCtor {
    color: [f32; 4],
    mask_offset: [f32; 2],
    mask_half: [f32; 2],
    mask_radii: [f32; 4],
}
impl FillVertexConstructor<PathVertex> for FillCtor {
    fn new_vertex(&mut self, v: FillVertex) -> PathVertex {
        let p = v.position();
        PathVertex {
            position: [p.x, p.y],
            cov_a: 1.0,
            cov_b: 0.0,
            color: self.color,
            mask_offset: self.mask_offset,
            mask_half: self.mask_half,
            mask_radii: self.mask_radii,
        }
    }
}

/// Fill fringe (outline stroke): inner side `cov_a = 1`, outer side `cov_a = 0`, `cov_b = 0`. For a
/// clockwise fill outline (in y-down space) the interior is lyon's `Positive` side; a counter-clockwise
/// contour inverts this (see [`PathBuilder::fill`]). Pinned by `path_fill_fringe_should_ramp_inward`.
struct FringeCtor {
    color: [f32; 4],
    mask_offset: [f32; 2],
    mask_half: [f32; 2],
    mask_radii: [f32; 4],
}
impl StrokeVertexConstructor<PathVertex> for FringeCtor {
    fn new_vertex(&mut self, v: StrokeVertex) -> PathVertex {
        let p = v.position();
        let cov_a = match v.side() {
            Side::Negative => 0.0,
            Side::Positive => 1.0,
        };
        PathVertex {
            position: [p.x, p.y],
            cov_a,
            cov_b: 0.0,
            color: self.color,
            mask_offset: self.mask_offset,
            mask_half: self.mask_half,
            mask_radii: self.mask_radii,
        }
    }
}

/// Stroke band: `cov_a = half_plus` (flat), `cov_b = signed distance from the centerline`.
struct StrokeCtor {
    color: [f32; 4],
    mask_offset: [f32; 2],
    mask_half: [f32; 2],
    mask_radii: [f32; 4],
    half_plus: f32,
}
impl StrokeVertexConstructor<PathVertex> for StrokeCtor {
    fn new_vertex(&mut self, v: StrokeVertex) -> PathVertex {
        let p = v.position();
        let c = v.position_on_path();
        let dist = ((p.x - c.x).powi(2) + (p.y - c.y).powi(2)).sqrt();
        let sign = match v.side() {
            Side::Positive => 1.0,
            Side::Negative => -1.0,
        };
        PathVertex {
            position: [p.x, p.y],
            cov_a: self.half_plus,
            cov_b: sign * dist,
            color: self.color,
            mask_offset: self.mask_offset,
            mask_half: self.mask_half,
            mask_radii: self.mask_radii,
        }
    }
}

fn tessellate_fill(path: &LyonPath, color: Color, mask: RoundedRect, order: u32) -> PathPrim {
    let (mask_offset, mask_half, mask_radii) = mask_attrs(mask);
    let color = color_arr(color);
    let mut buffers: VertexBuffers<PathVertex, u32> = VertexBuffers::new();
    let mut fill = FillTessellator::new();
    let _ = fill.tessellate_path(
        path,
        &FillOptions::default(),
        &mut BuffersBuilder::new(
            &mut buffers,
            FillCtor {
                color,
                mask_offset,
                mask_half,
                mask_radii,
            },
        ),
    );
    // Outline fringe for the AA edge.
    let mut stroke = StrokeTessellator::new();
    let _ = stroke.tessellate_path(
        path,
        &StrokeOptions::default().with_line_width(FEATHER_PX),
        &mut BuffersBuilder::new(
            &mut buffers,
            FringeCtor {
                color,
                mask_offset,
                mask_half,
                mask_radii,
            },
        ),
    );
    PathPrim::new(buffers.vertices, buffers.indices, order)
}

fn tessellate_stroke(
    path: &LyonPath,
    width: f32,
    color: Color,
    mask: RoundedRect,
    order: u32,
) -> PathPrim {
    let (mask_offset, mask_half, mask_radii) = mask_attrs(mask);
    let color = color_arr(color);
    let mut buffers: VertexBuffers<PathVertex, u32> = VertexBuffers::new();
    let mut stroke = StrokeTessellator::new();
    let _ = stroke.tessellate_path(
        path,
        &StrokeOptions::default().with_line_width(width + FEATHER_PX),
        &mut BuffersBuilder::new(
            &mut buffers,
            StrokeCtor {
                color,
                mask_offset,
                mask_half,
                mask_radii,
                half_plus: width * 0.5 + FEATHER_PX * 0.5,
            },
        ),
    );
    PathPrim::new(buffers.vertices, buffers.indices, order)
}

// ---------------------------------------------------------------------------
// PathRenderer — the pipeline (globals group 0, `path.wgsl`) + growable vertex/index buffers.
// ---------------------------------------------------------------------------

/// The shared SDF/AA prelude composed ahead of the path shader (for `mask_coverage`).
const PATH_SHADER_SRC: &str = concat!(
    include_str!("shaders/sdf.wgsl"),
    "\n",
    include_str!("shaders/path.wgsl"),
);

use bytemuck::{Pod, Zeroable};

/// Frame globals (uniform): physical viewport size + logical→physical scale. Mirrors the other
/// primitive renderers.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Pod, Zeroable)]
struct Globals {
    viewport: [f32; 2],
    scale: f32,
    _pad: f32,
}

/// A tessellated path's slice of the concatenated vertex/index buffers.
struct PathMesh {
    index_start: u32,
    index_count: u32,
    base_vertex: i32,
}

const INITIAL_VERTEX_CAP: u64 = 256;
const INITIAL_INDEX_CAP: u64 = 512;

/// The path pipeline plus its globals and growable vertex + index buffers.
pub(crate) struct PathRenderer {
    pipeline: wgpu::RenderPipeline,
    globals_buffer: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    vertices: Vec<PathVertex>,
    indices: Vec<u32>,
    meshes: Vec<PathMesh>,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    vertex_cap: u64,
    index_cap: u64,
    last_globals: Option<Globals>,
}

impl PathRenderer {
    pub(crate) fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kagari.path.shader"),
            source: wgpu::ShaderSource::Wgsl(PATH_SHADER_SRC.into()),
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kagari.path.globals_layout"),
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
            label: Some("kagari.path.globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kagari.path.globals_bind"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kagari.path.pipeline_layout"),
            bind_group_layouts: &[Some(&globals_layout)],
            immediate_size: 0,
        });

        // Per-vertex attributes: position, cov_a, cov_b, color, mask_offset, mask_half, mask_radii.
        const ATTRS: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
            0 => Float32x2, 1 => Float32, 2 => Float32, 3 => Float32x4,
            4 => Float32x2, 5 => Float32x2, 6 => Float32x4,
        ];
        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<PathVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRS,
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kagari.path.pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[vertex_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    // Premultiplied-alpha "over" blend (gpu.md §6), same as the other primitives.
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
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kagari.path.vertices"),
            size: INITIAL_VERTEX_CAP * std::mem::size_of::<PathVertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kagari.path.indices"),
            size: INITIAL_INDEX_CAP * std::mem::size_of::<u32>() as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            globals_buffer,
            globals_bind,
            vertices: Vec::new(),
            indices: Vec::new(),
            meshes: Vec::new(),
            vertex_buffer,
            index_buffer,
            vertex_cap: INITIAL_VERTEX_CAP,
            index_cap: INITIAL_INDEX_CAP,
            last_globals: None,
        }
    }

    /// Concatenate the scene's paths (already order-sorted) into the shared vertex/index buffers,
    /// recording each path's draw range, then upload globals + geometry.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        viewport: (u32, u32),
        scale: f32,
    ) {
        self.vertices.clear();
        self.indices.clear();
        self.meshes.clear();
        for path in &scene.paths {
            let base_vertex = self.vertices.len() as i32;
            let index_start = self.indices.len() as u32;
            self.vertices.extend_from_slice(&path.vertices);
            self.indices.extend_from_slice(&path.indices);
            self.meshes.push(PathMesh {
                index_start,
                index_count: path.indices.len() as u32,
                base_vertex,
            });
        }

        // Grow buffers if needed (power-of-two), then upload.
        let needed_v = self.vertices.len() as u64;
        if needed_v > self.vertex_cap {
            self.vertex_cap = needed_v.next_power_of_two();
            self.vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kagari.path.vertices"),
                size: self.vertex_cap * std::mem::size_of::<PathVertex>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        let needed_i = self.indices.len() as u64;
        if needed_i > self.index_cap {
            self.index_cap = needed_i.next_power_of_two();
            self.index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kagari.path.indices"),
                size: self.index_cap * std::mem::size_of::<u32>() as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
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
        if !self.vertices.is_empty() {
            queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&self.vertices));
            queue.write_buffer(&self.index_buffer, 0, bytemuck::cast_slice(&self.indices));
        }
    }

    /// Draw the paths in `batch.range` — one `draw_indexed` per path (variable geometry, so not
    /// instanced). `batch.range` indexes `self.meshes`, filled in the same order-sorted order.
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, batch: &Batch) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.globals_bind, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        for i in batch.range.clone() {
            let m = &self.meshes[i as usize];
            pass.draw_indexed(
                m.index_start..m.index_start + m.index_count,
                m.base_vertex,
                0..1,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::RoundedRect;

    fn no_clip() -> RoundedRect {
        RoundedRect {
            rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
            radii: Corners::default(),
        }
    }

    fn triangle() -> PathBuilder {
        let mut b = PathBuilder::new();
        b.move_to(Point::new(10.0, 10.0))
            .line_to(Point::new(40.0, 10.0))
            .line_to(Point::new(25.0, 40.0))
            .close();
        b
    }

    #[test]
    fn path_builder_should_tessellate_fill() {
        let prim = triangle().fill(Color::new(1.0, 1.0, 1.0, 1.0), no_clip(), 0);
        assert!(!prim.vertices.is_empty(), "the fill produces vertices");
        assert!(!prim.indices.is_empty(), "the fill produces indices");
        assert!(
            prim.vertices
                .iter()
                .any(|v| v.cov_a == 1.0 && v.cov_b == 0.0),
            "the interior has fully-covered vertices"
        );
    }

    #[test]
    fn path_builder_should_tessellate_stroke() {
        let mut b = PathBuilder::new();
        b.move_to(Point::new(0.0, 20.0))
            .line_to(Point::new(50.0, 20.0));
        let prim = b.stroke(4.0, Color::new(1.0, 1.0, 1.0, 1.0), no_clip(), 0);
        assert!(
            !prim.vertices.is_empty() && !prim.indices.is_empty(),
            "the stroke produces a mesh"
        );
        // half_plus = 4/2 + 0.5 = 2.5; edge vertices sit ~2.5px from the centerline.
        assert!(
            prim.vertices.iter().all(|v| (v.cov_a - 2.5).abs() < 1e-4),
            "stroke cov_a is the flat half+0.5"
        );
        assert!(
            prim.vertices.iter().any(|v| v.cov_b.abs() > 2.0),
            "stroke edges carry a signed centerline distance"
        );
    }

    #[test]
    fn path_fill_should_feather_edge() {
        // The outline fringe contributes vertices with cov_a == 0 (the outer edge of the AA ramp).
        let prim = triangle().fill(Color::new(1.0, 1.0, 1.0, 1.0), no_clip(), 0);
        assert!(
            prim.vertices.iter().any(|v| v.cov_a == 0.0),
            "the fringe has cov_a == 0 vertices (the AA ramp exists)"
        );
    }

    /// The shader coverage: `clamp(cov_a - abs(cov_b), 0, 1)`, reproduced on CPU vertex data so the feather
    /// is pinned deterministically (golden captures — never validates — this).
    fn cov(v: &PathVertex) -> f32 {
        (v.cov_a - v.cov_b.abs()).clamp(0.0, 1.0)
    }

    /// Signed area of triangle (a, b, c); sign tells which side of edge a→b the point c is on.
    fn edge_sign(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
        (px - bx) * (ay - by) - (ax - bx) * (py - by)
    }

    /// Whether `(px, py)` lies inside the triangle A(10,10) B(40,10) C(25,40) (the `triangle()` shape).
    fn inside_triangle(px: f32, py: f32) -> bool {
        let d1 = edge_sign(px, py, 10.0, 10.0, 40.0, 10.0);
        let d2 = edge_sign(px, py, 40.0, 10.0, 25.0, 40.0);
        let d3 = edge_sign(px, py, 25.0, 40.0, 10.0, 10.0);
        let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(has_neg && has_pos)
    }

    #[test]
    fn path_fill_fringe_should_ramp_inward() {
        // The AA ramp must go interior(cov_a=1) → exterior(cov_a=0); a flipped winding assumption would
        // invert it. Pin the orientation geometrically: the cov_a==0 fringe ring lies OUTSIDE the shape
        // (with correct winding); a flip would place it inside.
        let prim = triangle().fill(Color::new(1.0, 1.0, 1.0, 1.0), no_clip(), 0);
        let outer: Vec<_> = prim.vertices.iter().filter(|v| v.cov_a == 0.0).collect();
        assert!(!outer.is_empty(), "the outer fringe ring exists");
        let outside = outer
            .iter()
            .filter(|v| !inside_triangle(v.position[0], v.position[1]))
            .count();
        assert_eq!(
            outside,
            outer.len(),
            "every cov_a==0 fringe vertex lies outside the fill (inward ramp, correct winding)"
        );
    }

    #[test]
    fn path_stroke_should_cover_core_and_feather_edge() {
        // width 4 → half_plus = 2.5; the band spans ±2.5 from the centerline.
        let mut b = PathBuilder::new();
        b.move_to(Point::new(0.0, 20.0))
            .line_to(Point::new(50.0, 20.0));
        let prim = b.stroke(4.0, Color::new(1.0, 1.0, 1.0, 1.0), no_clip(), 0);
        // cov_a >= 1 flat, so the interpolated centerline (cov_b→0) reaches full coverage in the core.
        assert!(
            prim.vertices.iter().all(|v| v.cov_a >= 1.0),
            "cov_a keeps the core fully covered"
        );
        // The outer band vertices sit at |cov_b| ~ half_plus, so coverage feathers to 0 exactly at the edge.
        let max_d = prim
            .vertices
            .iter()
            .map(|v| v.cov_b.abs())
            .fold(0.0f32, f32::max);
        assert!(
            (max_d - 2.5).abs() < 0.05,
            "the outer edge lands at half_plus (a 1px feather centered on the nominal edge)"
        );
        assert!(
            prim.vertices.iter().filter(|v| cov(v) < 0.01).count() > 0,
            "edge vertices feather to ~0 coverage"
        );
        // Signed distance: vertices exist on both sides of the centerline.
        assert!(
            prim.vertices.iter().any(|v| v.cov_b > 2.0)
                && prim.vertices.iter().any(|v| v.cov_b < -2.0),
            "cov_b is a signed centerline distance (both sides present)"
        );
    }

    #[test]
    fn path_shader_should_pass_naga_validation() {
        let module =
            wgpu::naga::front::wgsl::parse_str(PATH_SHADER_SRC).expect("path WGSL should parse");
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .expect("path WGSL should validate");
    }

    #[test]
    fn path_vertex_should_be_64_bytes() {
        assert_eq!(std::mem::size_of::<PathVertex>(), 64);
    }
}
