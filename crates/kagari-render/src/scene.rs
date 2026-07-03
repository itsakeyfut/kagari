//! Scene: one frame's resolved primitives in painter's order, plus the per-type
//! batching foundation (specs §2.2 / §2.7).
//!
//! The renderer receives **resolved** values — linear `Color`, px `Rect`; token
//! resolution is the style/core layer's job. M1 holds only quads; other primitive
//! vectors (Shadow, sprites, paths, underlines) are added to `Scene` as their
//! types land — adding a field is non-breaking.

use std::ops::Range;

use bytemuck::{Pod, Zeroable};
use kagari_base::{Color, Corners, Edges, Point, Rect, Transform};

use crate::atlas::AtlasCoord;

/// A rounded rectangle, used as a content-mask clip region.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RoundedRect {
    pub rect: Rect,
    pub radii: Corners,
}

/// A quad's background fill.
#[derive(Clone, Copy, PartialEq, Debug)]
#[non_exhaustive]
pub enum Background {
    /// A single linear premultiplied color.
    Solid(Color),
    /// A two-stop linear gradient interpolated in linear space; `start_point` and
    /// `end_point` are in `[0, 1]` quad-local space (multi-stop/angle is post-MVP).
    LinearGradient {
        start: Color,
        end: Color,
        start_point: Point,
        end_point: Point,
    },
}

/// Per-edge border widths plus a single border color.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Border {
    pub widths: Edges,
    pub color: Color,
}

/// A resolved quad primitive (rounded rect + per-edge border + solid/gradient
/// background + rounded content-mask clip).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Quad {
    pub bounds: Rect,
    pub corner_radii: Corners,
    pub bg: Background,
    pub border: Border,
    pub content_mask: RoundedRect,
    /// Painter's-order key (monotonic in tree order; assigned by paint/core).
    /// CPU-side only — not uploaded to the GPU.
    pub order: u32,
}

/// A resolved drop-shadow primitive (#155): an analytic blurred rounded rectangle drawn **behind**
/// the casting box. `bounds`/`corner_radii` are the casting box (typically the element's quad); the
/// shadow shape is that box translated by `offset` and inflated by `spread`, with a Gaussian blur of
/// radius `blur` (all logical px). `color` is linear premultiplied. Inner/inset shadow is post-MVP
/// (specs §2.5/§2.8).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Shadow {
    pub bounds: Rect,
    pub corner_radii: Corners,
    pub offset: Point,
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
    pub content_mask: RoundedRect,
    /// Painter's-order key (CPU-side only — not uploaded to the GPU).
    pub order: u32,
}

/// A monochrome sprite: an alpha-coverage tile from the R8 atlas (#18) multiplied by
/// a color (glyphs, coverage masks). `bounds` is integer-snapped by the producer.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MonochromeSprite {
    pub bounds: Rect,
    pub tex: AtlasCoord,
    pub color: Color,
    pub content_mask: RoundedRect,
    /// Painter's-order key (CPU-side only — not uploaded to the GPU).
    pub order: u32,
}

/// A polychrome sprite (#55): a colored-image tile from the RGBA atlas (photos/icons),
/// multiplied by a `tint`. The tile is stored straight-alpha sRGB; the shader decodes to
/// linear, premultiplies, then multiplies by `tint` (premultiplied linear; `Color::WHITE`
/// = draw unmodified) and the content mask. `bounds` is the destination rect (logical px).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PolychromeSprite {
    pub bounds: Rect,
    pub tex: AtlasCoord,
    pub tint: Color,
    pub content_mask: RoundedRect,
    /// Painter's-order key (CPU-side only — not uploaded to the GPU).
    pub order: u32,
}

/// How an underline band is filled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum UnderlineStyle {
    /// A continuous filled band.
    Solid,
    /// Periodic rectangular segments along the band's long axis (IME preedit, etc).
    Dotted,
}

/// A resolved underline band (used for text underlines and IME preedit segments).
/// `rect` is the band to fill; `thickness` derives the dotted segment period.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Underline {
    pub rect: Rect,
    pub color: Color,
    pub style: UnderlineStyle,
    pub thickness: f32,
    pub content_mask: RoundedRect,
    /// Painter's-order key (CPU-side only — not uploaded to the GPU).
    pub order: u32,
}

/// A tessellated path vertex (#58): position + a feather AA pair + a flat premultiplied color and
/// content-mask. Byte-matched to `path.wgsl`'s `PathVertex` (16×f32 = 64 bytes, no padding → `Pod`).
///
/// **Feather AA**: the fragment computes `coverage = clamp(cov_a - abs(cov_b), 0, 1)`, unifying fill and
/// stroke. A fill sets `cov_a = 1` (interior) / ramps `1→0` across a 1px outline fringe with `cov_b = 0`;
/// a stroke sets `cov_a = half_width + 0.5` (flat) and `cov_b = signed distance from the centerline`, so
/// the coverage is solid to the nominal edge and ramps to 0 over the outer ~1px. `color`/mask are flat
/// (identical on every vertex of a path). All values are logical px / linear premultiplied.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, Pod, Zeroable)]
pub(crate) struct PathVertex {
    pub(crate) position: [f32; 2],
    pub(crate) cov_a: f32,
    pub(crate) cov_b: f32,
    pub(crate) color: [f32; 4],
    pub(crate) mask_offset: [f32; 2],
    pub(crate) mask_half: [f32; 2],
    pub(crate) mask_radii: [f32; 4],
}

/// A resolved path primitive (#58): a tessellated triangle mesh (fill or stroke) with feathered AA, built
/// by [`PathBuilder`](crate::PathBuilder). `indices` reference `vertices`; both pack into the frame's
/// shared path vertex/index buffers by the renderer.
#[derive(Clone, PartialEq, Debug)]
pub struct PathPrim {
    pub(crate) vertices: Vec<PathVertex>,
    pub(crate) indices: Vec<u32>,
    /// Painter's-order key (CPU-side only — not uploaded to the GPU).
    pub(crate) order: u32,
}

impl PathPrim {
    /// Builds a path prim from tessellated geometry (used by `PathBuilder`).
    pub(crate) fn new(vertices: Vec<PathVertex>, indices: Vec<u32>, order: u32) -> Self {
        Self {
            vertices,
            indices,
            order,
        }
    }
}

/// Scales rounded-corner radii by a uniform factor (#221 transform).
fn scale_corners(c: Corners, s: f32) -> Corners {
    Corners {
        tl: c.tl * s,
        tr: c.tr * s,
        br: c.br * s,
        bl: c.bl * s,
    }
}

/// Maps a rounded-rect content mask under `t` (#221): its rect by `map_rect`, its radii by `scale`.
fn map_mask(mask: RoundedRect, t: &Transform) -> RoundedRect {
    RoundedRect {
        rect: t.map_rect(mask.rect),
        radii: scale_corners(mask.radii, t.scale),
    }
}

impl Quad {
    /// Maps this quad into window space under a paint transform (#221): bounds + content mask by `t`,
    /// corner radii + border widths by `scale`. The gradient stops (quad-local `[0,1]`) are unaffected.
    pub fn apply_transform(&mut self, t: &Transform) {
        self.bounds = t.map_rect(self.bounds);
        self.content_mask = map_mask(self.content_mask, t);
        self.corner_radii = scale_corners(self.corner_radii, t.scale);
        let w = &mut self.border.widths;
        *w = Edges {
            top: w.top * t.scale,
            right: w.right * t.scale,
            bottom: w.bottom * t.scale,
            left: w.left * t.scale,
        };
    }
}

impl Shadow {
    /// Maps this shadow under a paint transform (#221): the casting box + content mask by `t`, and the
    /// corner radii / blur / spread / offset by `scale`.
    pub fn apply_transform(&mut self, t: &Transform) {
        self.bounds = t.map_rect(self.bounds);
        self.content_mask = map_mask(self.content_mask, t);
        self.corner_radii = scale_corners(self.corner_radii, t.scale);
        self.offset = self.offset * t.scale;
        self.blur *= t.scale;
        self.spread *= t.scale;
    }
}

impl MonochromeSprite {
    /// Maps this glyph/coverage sprite under a paint transform (#221): bounds + content mask by `t`. The
    /// atlas tile is unchanged, so a zoomed glyph samples its base raster (re-raster at zoom is post-MVP).
    pub fn apply_transform(&mut self, t: &Transform) {
        self.bounds = t.map_rect(self.bounds);
        self.content_mask = map_mask(self.content_mask, t);
    }
}

impl PolychromeSprite {
    /// Maps this image sprite under a paint transform (#221): bounds + content mask by `t`.
    pub fn apply_transform(&mut self, t: &Transform) {
        self.bounds = t.map_rect(self.bounds);
        self.content_mask = map_mask(self.content_mask, t);
    }
}

impl Underline {
    /// Maps this underline band under a paint transform (#221): rect + content mask by `t`, thickness by
    /// `scale`.
    pub fn apply_transform(&mut self, t: &Transform) {
        self.rect = t.map_rect(self.rect);
        self.content_mask = map_mask(self.content_mask, t);
        self.thickness *= t.scale;
    }
}

impl PathPrim {
    /// Maps this tessellated path under a paint transform (#221): every vertex position + its per-vertex
    /// rounded-rect mask (offset by `map_point`, half-extents + radii by `scale`). The feather-AA pair
    /// (`cov_a`/`cov_b`, a screen-space px ramp) is left unscaled — an accepted approximation at zoom.
    pub fn apply_transform(&mut self, t: &Transform) {
        for v in &mut self.vertices {
            let p = t.map_point(Point::new(v.position[0], v.position[1]));
            v.position = [p.x, p.y];
            let m = t.map_point(Point::new(v.mask_offset[0], v.mask_offset[1]));
            v.mask_offset = [m.x, m.y];
            v.mask_half = [v.mask_half[0] * t.scale, v.mask_half[1] * t.scale];
            v.mask_radii = [
                v.mask_radii[0] * t.scale,
                v.mask_radii[1] * t.scale,
                v.mask_radii[2] * t.scale,
                v.mask_radii[3] * t.scale,
            ];
        }
    }
}

/// The kind of primitive a batch draws (one pipeline per kind). More kinds are
/// added alongside their primitives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum PrimitiveKind {
    Shadow,
    Quad,
    /// A tessellated fill/stroke path (#58).
    Path,
    /// A colored-image sprite from the RGBA atlas (#55).
    Image,
    Sprite,
    Underline,
}

/// A contiguous run of one primitive kind: an instance range drawn in one call.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Batch {
    pub kind: PrimitiveKind,
    pub range: Range<u32>,
}

/// One frame's resolved primitives. Buffers are reused via [`Scene::clear`]
/// (capacity retained, perf.md).
#[derive(Default)]
pub struct Scene {
    pub shadows: Vec<Shadow>,
    pub quads: Vec<Quad>,
    pub paths: Vec<PathPrim>,
    pub images: Vec<PolychromeSprite>,
    pub glyphs: Vec<MonochromeSprite>,
    pub underlines: Vec<Underline>,
}

impl Scene {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear all primitives, retaining allocated capacity for next frame.
    pub fn clear(&mut self) {
        self.shadows.clear();
        self.quads.clear();
        self.paths.clear();
        self.images.clear();
        self.glyphs.clear();
        self.underlines.clear();
    }

    /// Sort each primitive vector into painter's order, then **merge** them into one
    /// painter's-order sequence of batches written into `out`. Each `Batch { kind,
    /// range }`'s `range` indexes that kind's instance buffer, which the per-kind
    /// renderer packs in the same (now order-sorted) order — so the ranges line up.
    ///
    /// `out` is cleared first and its capacity is retained, so the caller (the
    /// renderer) reuses one buffer across frames rather than allocating per frame
    /// (perf.md), mirroring how the instance `Vec`s are reused.
    ///
    /// The per-vector sort is stable (equal `order` keeps insertion order); ties
    /// across kinds draw by kind priority (Shadow before Quad before Image before Sprite
    /// before Underline), per the painter's order in the renderer design — so a shadow at
    /// the same `order` as its quad draws behind it, and a text glyph over an image at the
    /// same `order` draws on top. Consecutive same-kind picks coalesce into one batch
    /// (contiguous indices within a kind).
    pub fn batches_into(&mut self, out: &mut Vec<Batch>) {
        self.shadows.sort_by_key(|s| s.order);
        self.quads.sort_by_key(|q| q.order);
        self.paths.sort_by_key(|p| p.order);
        self.images.sort_by_key(|i| i.order);
        self.glyphs.sort_by_key(|g| g.order);
        self.underlines.sort_by_key(|u| u.order);

        out.clear();
        let (mut si, mut qi, mut pi, mut ii, mut gi, mut ui) =
            (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
        loop {
            // Each kind's next head as (order, kind-priority); pick the minimum. The
            // priorities are distinct, so the minimum is unique — equal `order` falls
            // back to priority (Shadow < Quad < Path < Image < Sprite < Underline).
            let heads = [
                self.shadows
                    .get(si)
                    .map(|s| (s.order, PrimitiveKind::Shadow)),
                self.quads.get(qi).map(|q| (q.order, PrimitiveKind::Quad)),
                self.paths.get(pi).map(|p| (p.order, PrimitiveKind::Path)),
                self.images.get(ii).map(|i| (i.order, PrimitiveKind::Image)),
                self.glyphs
                    .get(gi)
                    .map(|g| (g.order, PrimitiveKind::Sprite)),
                self.underlines
                    .get(ui)
                    .map(|u| (u.order, PrimitiveKind::Underline)),
            ];
            let Some((_, kind)) = heads
                .into_iter()
                .flatten()
                .min_by_key(|(order, kind)| (*order, kind_priority(*kind)))
            else {
                break;
            };
            let idx = match kind {
                PrimitiveKind::Shadow => {
                    let i = si as u32;
                    si += 1;
                    i
                }
                PrimitiveKind::Quad => {
                    let i = qi as u32;
                    qi += 1;
                    i
                }
                PrimitiveKind::Path => {
                    let i = pi as u32;
                    pi += 1;
                    i
                }
                PrimitiveKind::Image => {
                    let i = ii as u32;
                    ii += 1;
                    i
                }
                PrimitiveKind::Sprite => {
                    let i = gi as u32;
                    gi += 1;
                    i
                }
                PrimitiveKind::Underline => {
                    let i = ui as u32;
                    ui += 1;
                    i
                }
            };
            // Extend the previous batch if it is the same kind and contiguous,
            // otherwise start a new run.
            match out.last_mut() {
                Some(b) if b.kind == kind && b.range.end == idx => b.range.end = idx + 1,
                _ => out.push(Batch {
                    kind,
                    range: idx..idx + 1,
                }),
            }
        }
    }
}

/// Painter's-order priority for an equal-`order` tie: Shadow first (behind), then Quad, then Path, then
/// Image, then Sprite (glyphs), then Underline (drawn last), per the renderer's frame flow — shapes draw
/// over their background quad, image content over shapes, and text over images.
fn kind_priority(kind: PrimitiveKind) -> u8 {
    match kind {
        PrimitiveKind::Shadow => 0,
        PrimitiveKind::Quad => 1,
        PrimitiveKind::Path => 2,
        PrimitiveKind::Image => 3,
        PrimitiveKind::Sprite => 4,
        PrimitiveKind::Underline => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_quad(order: u32) -> Quad {
        Quad {
            bounds: Rect::from_xywh(0.0, 0.0, 10.0, 10.0),
            corner_radii: Corners::default(),
            bg: Background::Solid(Color::new(1.0, 1.0, 1.0, 1.0)),
            border: Border {
                widths: Edges::default(),
                color: Color::TRANSPARENT,
            },
            content_mask: RoundedRect {
                rect: Rect::from_xywh(0.0, 0.0, 10.0, 10.0),
                radii: Corners::default(),
            },
            order,
        }
    }

    #[test]
    fn scene_clear_should_retain_capacity() {
        let mut scene = Scene::new();
        scene.quads.push(test_quad(0));
        scene.quads.push(test_quad(1));
        let cap = scene.quads.capacity();
        scene.clear();
        assert!(scene.quads.is_empty());
        assert!(scene.quads.capacity() >= cap);
    }

    /// Collect a scene's batches into a fresh buffer (the test-side mirror of how the
    /// renderer reuses one across frames).
    fn collect_batches(scene: &mut Scene) -> Vec<Batch> {
        let mut out = Vec::new();
        scene.batches_into(&mut out);
        out
    }

    #[test]
    fn batches_should_sort_quads_into_painter_order() {
        let mut scene = Scene::new();
        scene.quads.push(test_quad(3));
        scene.quads.push(test_quad(1));
        scene.quads.push(test_quad(2));
        let batches = collect_batches(&mut scene);
        let orders: Vec<u32> = scene.quads.iter().map(|q| q.order).collect();
        assert_eq!(orders, vec![1, 2, 3]);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].range, 0..3);
        assert_eq!(batches[0].kind, PrimitiveKind::Quad);
    }

    #[test]
    fn batches_should_preserve_insertion_order_for_equal_keys() {
        // Stable sort: two quads with the same `order` keep their insertion order.
        let mut scene = Scene::new();
        let mut a = test_quad(5);
        a.bounds = Rect::from_xywh(1.0, 0.0, 10.0, 10.0);
        let mut b = test_quad(5);
        b.bounds = Rect::from_xywh(2.0, 0.0, 10.0, 10.0);
        scene.quads.push(a);
        scene.quads.push(b);
        collect_batches(&mut scene);
        assert_eq!(scene.quads[0].bounds.origin.x, 1.0);
        assert_eq!(scene.quads[1].bounds.origin.x, 2.0);
    }

    #[test]
    fn batches_should_be_empty_for_empty_scene() {
        let mut scene = Scene::new();
        assert!(collect_batches(&mut scene).is_empty());
    }

    #[test]
    fn batches_into_should_reuse_buffer_capacity() {
        // The renderer reuses one batch buffer across frames: a second fill into the
        // same `out` clears it (no stale batches) and keeps the allocated capacity.
        let mut scene = Scene::new();
        scene.quads.push(test_quad(0));
        scene.quads.push(test_quad(1));
        let mut out = Vec::new();
        scene.batches_into(&mut out);
        let cap = out.capacity();
        scene.clear();
        scene.quads.push(test_quad(0));
        scene.batches_into(&mut out);
        assert_eq!(out.len(), 1, "no stale batches from the previous fill");
        assert!(out.capacity() >= cap, "capacity retained for reuse");
    }

    fn test_sprite(order: u32) -> MonochromeSprite {
        MonochromeSprite {
            bounds: Rect::from_xywh(0.0, 0.0, 10.0, 10.0),
            tex: AtlasCoord {
                page: 0,
                min: [0.0, 0.0],
                max: [1.0, 1.0],
            },
            color: Color::new(1.0, 1.0, 1.0, 1.0),
            content_mask: RoundedRect {
                rect: Rect::from_xywh(0.0, 0.0, 10.0, 10.0),
                radii: Corners::default(),
            },
            order,
        }
    }

    #[test]
    fn batches_should_sort_sprites_into_painter_order() {
        // Sprites-only: exercises the `(None, Some(_))` pick branch and the coalescing
        // of a consecutive same-kind run into one Sprite batch (range 0..3) — the
        // interleave/tie tests only ever produce single-element Sprite batches.
        let mut scene = Scene::new();
        scene.glyphs.push(test_sprite(3));
        scene.glyphs.push(test_sprite(1));
        scene.glyphs.push(test_sprite(2));
        let batches = collect_batches(&mut scene);
        let orders: Vec<u32> = scene.glyphs.iter().map(|g| g.order).collect();
        assert_eq!(orders, vec![1, 2, 3]);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].range, 0..3);
        assert_eq!(batches[0].kind, PrimitiveKind::Sprite);
    }

    #[test]
    fn batches_should_interleave_quads_and_sprites_by_order() {
        // quads at orders 0 and 2, a sprite at order 1 → Quad, Sprite, Quad. The quad
        // buffer is packed [order 0, order 2] so the two Quad batches index 0..1 / 1..2.
        let mut scene = Scene::new();
        scene.quads.push(test_quad(0));
        scene.quads.push(test_quad(2));
        scene.glyphs.push(test_sprite(1));
        let batches = collect_batches(&mut scene);
        assert_eq!(
            batches,
            vec![
                Batch {
                    kind: PrimitiveKind::Quad,
                    range: 0..1
                },
                Batch {
                    kind: PrimitiveKind::Sprite,
                    range: 0..1
                },
                Batch {
                    kind: PrimitiveKind::Quad,
                    range: 1..2
                },
            ]
        );
    }

    #[test]
    fn batches_should_draw_quad_before_sprite_on_tie() {
        // Equal order → Quad batch precedes the Sprite batch (kind priority).
        let mut scene = Scene::new();
        scene.glyphs.push(test_sprite(5));
        scene.quads.push(test_quad(5));
        let batches = collect_batches(&mut scene);
        assert_eq!(batches[0].kind, PrimitiveKind::Quad);
        assert_eq!(batches[1].kind, PrimitiveKind::Sprite);
    }

    fn test_image(order: u32) -> PolychromeSprite {
        PolychromeSprite {
            bounds: Rect::from_xywh(0.0, 0.0, 10.0, 10.0),
            tex: AtlasCoord {
                page: 0,
                min: [0.0, 0.0],
                max: [1.0, 1.0],
            },
            tint: Color::new(1.0, 1.0, 1.0, 1.0),
            content_mask: RoundedRect {
                rect: Rect::from_xywh(0.0, 0.0, 10.0, 10.0),
                radii: Corners::default(),
            },
            order,
        }
    }

    #[test]
    fn batches_should_sort_images_into_painter_order() {
        let mut scene = Scene::new();
        scene.images.push(test_image(3));
        scene.images.push(test_image(1));
        scene.images.push(test_image(2));
        let batches = collect_batches(&mut scene);
        let orders: Vec<u32> = scene.images.iter().map(|i| i.order).collect();
        assert_eq!(orders, vec![1, 2, 3]);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].range, 0..3);
        assert_eq!(batches[0].kind, PrimitiveKind::Image);
    }

    #[test]
    fn batches_should_draw_image_between_quad_and_glyph_on_tie() {
        // Equal order across quad/image/glyph → Quad, then Image, then Sprite (kind
        // priority): an image draws over its background quad, and a glyph over the image.
        let mut scene = Scene::new();
        scene.glyphs.push(test_sprite(5));
        scene.images.push(test_image(5));
        scene.quads.push(test_quad(5));
        let batches = collect_batches(&mut scene);
        let kinds: Vec<PrimitiveKind> = batches.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            vec![
                PrimitiveKind::Quad,
                PrimitiveKind::Image,
                PrimitiveKind::Sprite,
            ]
        );
    }

    fn test_path(order: u32) -> PathPrim {
        // Batching only reads `order`; the geometry is irrelevant here.
        PathPrim::new(Vec::new(), Vec::new(), order)
    }

    #[test]
    fn batches_should_sort_paths_into_painter_order() {
        let mut scene = Scene::new();
        scene.paths.push(test_path(3));
        scene.paths.push(test_path(1));
        scene.paths.push(test_path(2));
        let batches = collect_batches(&mut scene);
        let orders: Vec<u32> = scene.paths.iter().map(|p| p.order).collect();
        assert_eq!(orders, vec![1, 2, 3]);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].range, 0..3);
        assert_eq!(batches[0].kind, PrimitiveKind::Path);
    }

    #[test]
    fn batches_should_draw_path_between_quad_and_image_on_tie() {
        // Equal order across quad/path/image → Quad, then Path, then Image (kind priority): a shape
        // draws over its background quad, and image content over the shape.
        let mut scene = Scene::new();
        scene.images.push(test_image(5));
        scene.paths.push(test_path(5));
        scene.quads.push(test_quad(5));
        let batches = collect_batches(&mut scene);
        let kinds: Vec<PrimitiveKind> = batches.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            vec![
                PrimitiveKind::Quad,
                PrimitiveKind::Path,
                PrimitiveKind::Image,
            ]
        );
    }

    fn test_underline(order: u32) -> Underline {
        Underline {
            rect: Rect::from_xywh(0.0, 0.0, 20.0, 2.0),
            color: Color::new(1.0, 1.0, 1.0, 1.0),
            style: UnderlineStyle::Solid,
            thickness: 2.0,
            content_mask: RoundedRect {
                rect: Rect::from_xywh(0.0, 0.0, 100.0, 100.0),
                radii: Corners::default(),
            },
            order,
        }
    }

    #[test]
    fn batches_should_sort_underlines_into_painter_order() {
        let mut scene = Scene::new();
        scene.underlines.push(test_underline(3));
        scene.underlines.push(test_underline(1));
        scene.underlines.push(test_underline(2));
        let batches = collect_batches(&mut scene);
        let orders: Vec<u32> = scene.underlines.iter().map(|u| u.order).collect();
        assert_eq!(orders, vec![1, 2, 3]);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].range, 0..3);
        assert_eq!(batches[0].kind, PrimitiveKind::Underline);
    }

    fn test_shadow(order: u32) -> Shadow {
        Shadow {
            bounds: Rect::from_xywh(0.0, 0.0, 10.0, 10.0),
            corner_radii: Corners::default(),
            offset: Point::new(0.0, 2.0),
            blur: 4.0,
            spread: 0.0,
            color: Color::new(0.0, 0.0, 0.0, 0.5),
            content_mask: RoundedRect {
                rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
                radii: Corners::default(),
            },
            order,
        }
    }

    #[test]
    fn batches_should_sort_shadows_into_painter_order() {
        let mut scene = Scene::new();
        scene.shadows.push(test_shadow(3));
        scene.shadows.push(test_shadow(1));
        scene.shadows.push(test_shadow(2));
        let batches = collect_batches(&mut scene);
        let orders: Vec<u32> = scene.shadows.iter().map(|s| s.order).collect();
        assert_eq!(orders, vec![1, 2, 3]);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].range, 0..3);
        assert_eq!(batches[0].kind, PrimitiveKind::Shadow);
    }

    #[test]
    fn batches_should_draw_shadow_before_quad_on_tie() {
        // Equal order → Shadow batch precedes the Quad batch (kind priority), so a drop
        // shadow draws behind the box that casts it.
        let mut scene = Scene::new();
        scene.quads.push(test_quad(5));
        scene.shadows.push(test_shadow(5));
        let batches = collect_batches(&mut scene);
        assert_eq!(batches[0].kind, PrimitiveKind::Shadow);
        assert_eq!(batches[1].kind, PrimitiveKind::Quad);
    }

    #[test]
    fn batches_should_draw_underline_last_on_tie() {
        // Equal order across all three kinds → Quad, then Sprite, then Underline.
        let mut scene = Scene::new();
        scene.underlines.push(test_underline(7));
        scene.glyphs.push(test_sprite(7));
        scene.quads.push(test_quad(7));
        let batches = collect_batches(&mut scene);
        let kinds: Vec<PrimitiveKind> = batches.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            vec![
                PrimitiveKind::Quad,
                PrimitiveKind::Sprite,
                PrimitiveKind::Underline
            ]
        );
    }

    #[test]
    fn apply_transform_should_scale_quad_geometry() {
        // scale ×2 + offset (5,5): bounds origin*2+offset, size*2; radii/border widths *2; content mask
        // maps too (#221).
        let mut q = Quad {
            bounds: Rect::from_xywh(10.0, 10.0, 20.0, 20.0),
            corner_radii: Corners {
                tl: 2.0,
                tr: 2.0,
                br: 2.0,
                bl: 2.0,
            },
            bg: Background::Solid(Color::new(0.0, 0.0, 0.0, 1.0)),
            border: Border {
                widths: Edges {
                    top: 1.0,
                    right: 1.0,
                    bottom: 1.0,
                    left: 1.0,
                },
                color: Color::TRANSPARENT,
            },
            content_mask: RoundedRect {
                rect: Rect::from_xywh(0.0, 0.0, 100.0, 100.0),
                radii: Corners::default(),
            },
            order: 0,
        };
        q.apply_transform(&Transform::new(2.0, Point::new(5.0, 5.0)));
        assert_eq!(q.bounds, Rect::from_xywh(25.0, 25.0, 40.0, 40.0));
        assert_eq!(q.corner_radii.tl, 4.0);
        assert_eq!(q.border.widths.top, 2.0);
        assert_eq!(q.content_mask.rect, Rect::from_xywh(5.0, 5.0, 200.0, 200.0));
    }

    #[test]
    fn apply_transform_should_scale_shadow_extras() {
        let mut s = Shadow {
            bounds: Rect::from_xywh(10.0, 10.0, 20.0, 20.0),
            corner_radii: Corners::default(),
            offset: Point::new(2.0, 3.0),
            blur: 4.0,
            spread: 1.0,
            color: Color::new(0.0, 0.0, 0.0, 1.0),
            content_mask: RoundedRect {
                rect: Rect::from_xywh(0.0, 0.0, 100.0, 100.0),
                radii: Corners::default(),
            },
            order: 0,
        };
        s.apply_transform(&Transform::new(2.0, Point::new(5.0, 5.0)));
        assert_eq!(s.bounds, Rect::from_xywh(25.0, 25.0, 40.0, 40.0));
        assert_eq!(s.offset, Point::new(4.0, 6.0), "shadow offset scales");
        assert_eq!(s.blur, 8.0, "blur scales");
        assert_eq!(s.spread, 2.0, "spread scales");
    }

    #[test]
    fn apply_transform_should_scale_underline_thickness() {
        let mut u = Underline {
            rect: Rect::from_xywh(0.0, 0.0, 10.0, 2.0),
            color: Color::new(0.0, 0.0, 0.0, 1.0),
            style: UnderlineStyle::Solid,
            thickness: 1.5,
            content_mask: RoundedRect {
                rect: Rect::from_xywh(0.0, 0.0, 100.0, 100.0),
                radii: Corners::default(),
            },
            order: 0,
        };
        u.apply_transform(&Transform::new(2.0, Point::new(0.0, 0.0)));
        assert_eq!(u.rect, Rect::from_xywh(0.0, 0.0, 20.0, 4.0));
        assert_eq!(u.thickness, 3.0, "underline thickness scales");
    }

    #[test]
    fn apply_transform_should_map_path_vertices() {
        let v = PathVertex {
            position: [3.0, 4.0],
            cov_a: 1.0,
            cov_b: 0.0,
            color: [0.0, 0.0, 0.0, 1.0],
            mask_offset: [1.0, 1.0],
            mask_half: [5.0, 5.0],
            mask_radii: [0.0, 0.0, 0.0, 0.0],
        };
        let mut p = PathPrim::new(vec![v], vec![0], 0);
        p.apply_transform(&Transform::new(2.0, Point::new(10.0, 10.0)));
        assert_eq!(
            p.vertices[0].position,
            [16.0, 18.0],
            "vertex position mapped (p*2+10)"
        );
        assert_eq!(
            p.vertices[0].mask_half,
            [10.0, 10.0],
            "mask half-extents scale"
        );
        assert_eq!(
            p.vertices[0].cov_a, 1.0,
            "feather-AA coverage left unscaled"
        );
    }
}
