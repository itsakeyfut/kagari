//! Scene: one frame's resolved primitives in painter's order, plus the per-type
//! batching foundation (specs §2.2 / §2.7).
//!
//! The renderer receives **resolved** values — linear `Color`, px `Rect`; token
//! resolution is the style/core layer's job. M1 holds only quads; other primitive
//! vectors (Shadow, sprites, paths, underlines) are added to `Scene` as their
//! types land — adding a field is non-breaking.

use std::ops::Range;

use kagari_base::{Color, Corners, Edges, Point, Rect};

use crate::atlas::AtlasCoord;

/// A rounded rectangle, used as a content-mask clip region.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RoundedRect {
    pub rect: Rect,
    pub radii: Corners,
}

/// A quad's background fill.
#[derive(Clone, Copy, PartialEq, Debug)]
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

/// How an underline band is filled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

/// The kind of primitive a batch draws (one pipeline per kind). More kinds are
/// added alongside their primitives (Shadow, paths).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrimitiveKind {
    Quad,
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
    pub quads: Vec<Quad>,
    pub glyphs: Vec<MonochromeSprite>,
    pub underlines: Vec<Underline>,
}

impl Scene {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear all primitives, retaining allocated capacity for next frame.
    pub fn clear(&mut self) {
        self.quads.clear();
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
    /// across kinds draw by kind priority (Quad before Sprite before Underline),
    /// per the painter's order in the renderer design. Consecutive same-kind picks
    /// coalesce into one batch (contiguous indices within a kind).
    pub fn batches_into(&mut self, out: &mut Vec<Batch>) {
        self.quads.sort_by_key(|q| q.order);
        self.glyphs.sort_by_key(|g| g.order);
        self.underlines.sort_by_key(|u| u.order);

        out.clear();
        let (mut qi, mut gi, mut ui) = (0usize, 0usize, 0usize);
        loop {
            // Each kind's next head as (order, kind-priority); pick the minimum. The
            // priorities are distinct, so the minimum is unique — equal `order` falls
            // back to priority (Quad < Sprite < Underline).
            let heads = [
                self.quads.get(qi).map(|q| (q.order, PrimitiveKind::Quad)),
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
                PrimitiveKind::Quad => {
                    let i = qi as u32;
                    qi += 1;
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

/// Painter's-order priority for an equal-`order` tie: Quad first, then Sprite, then
/// Underline (drawn last), per the renderer's frame flow.
fn kind_priority(kind: PrimitiveKind) -> u8 {
    match kind {
        PrimitiveKind::Quad => 0,
        PrimitiveKind::Sprite => 1,
        PrimitiveKind::Underline => 2,
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
}
