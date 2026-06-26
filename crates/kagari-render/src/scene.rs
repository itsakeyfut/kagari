//! Scene: one frame's resolved primitives in painter's order, plus the per-type
//! batching foundation (specs §2.2 / §2.7).
//!
//! The renderer receives **resolved** values — linear `Color`, px `Rect`; token
//! resolution is the style/core layer's job. M1 holds only quads; other primitive
//! vectors (Shadow, sprites, paths, underlines) are added to `Scene` as their
//! types land — adding a field is non-breaking.

use std::ops::Range;

use kagari_base::{Color, Corners, Edges, Point, Rect};

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

/// The kind of primitive a batch draws (one pipeline per kind). More kinds are
/// added alongside their primitives (Shadow, sprites, paths, underlines).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrimitiveKind {
    Quad,
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
}

impl Scene {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear all primitives, retaining allocated capacity for next frame.
    pub fn clear(&mut self) {
        self.quads.clear();
    }

    /// Sort primitives into painter's order and return the per-kind batches.
    ///
    /// The sort is stable, so primitives with equal `order` keep insertion order.
    /// With a single primitive kind this yields one `Quad` batch covering the
    /// order-sorted quads; merging multiple per-type vectors and splitting them
    /// into contiguous same-kind runs is the extension once more kinds exist.
    pub fn batches(&mut self) -> Vec<Batch> {
        self.quads.sort_by_key(|q| q.order);
        if self.quads.is_empty() {
            Vec::new()
        } else {
            vec![Batch {
                kind: PrimitiveKind::Quad,
                range: 0..self.quads.len() as u32,
            }]
        }
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

    #[test]
    fn batches_should_sort_quads_into_painter_order() {
        let mut scene = Scene::new();
        scene.quads.push(test_quad(3));
        scene.quads.push(test_quad(1));
        scene.quads.push(test_quad(2));
        let batches = scene.batches();
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
        scene.batches();
        assert_eq!(scene.quads[0].bounds.origin.x, 1.0);
        assert_eq!(scene.quads[1].bounds.origin.x, 2.0);
    }

    #[test]
    fn batches_should_be_empty_for_empty_scene() {
        let mut scene = Scene::new();
        assert!(scene.batches().is_empty());
    }
}
