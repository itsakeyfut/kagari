//! Hit-test structure (#47, specs §1.5): "what is under the pointer?".
//!
//! **Data-oriented (decision D1)**: a frame's interactive regions as pure geometry — `NodeId` +
//! bounds + clip + painter z + [`InteractFlags`], with **no closures**. Handlers stay on the
//! retained `Div`; dispatch (#48) picks a region here and resolves the handlers from the arena by
//! `NodeId`. Keeping it closure-free makes it headless-testable and mirrors how layout/paint/
//! reactive props are all keyed by `NodeId`.
//!
//! Built during paint (like [`Scene`](kagari_render::Scene)) and rebuilt each painted frame with
//! capacity retained. The paint-time recording (a `PaintCx` recorder that `Div::paint` pushes into)
//! lands with its first consumer in #48 — until handlers/focus/cursor exist there is nothing
//! interactive to record. This module is the structure those issues build on.

use kagari_base::{NodeId, Point, Rect};

/// Which interactions a node opts into. Populated by later issues' setters (mouse handlers #48,
/// `track_focus` #49, cursor #53, drag/drop #52); carried as metadata on a [`HitRegion`].
///
/// [`HitTest::pick`] does **not** filter on these — the paint-time recording gate (#48) only pushes
/// a region for an interactive node, so every recorded region is already interactive. Dispatch and
/// cursor resolution read the flags to decide *which* interaction applies.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct InteractFlags {
    pub mouse: bool,
    pub focusable: bool,
    pub drag_source: bool,
    pub drop_target: bool,
    pub cursor: bool,
}

/// One interactive region recorded during paint: a node's laid-out `bounds`, its `clip` rect, its
/// painter's-order `order` (z), and its [`InteractFlags`]. `bounds`/`clip` are absolute (window)
/// coordinates. The `clip` is axis-aligned; rounded-corner hit precision is post-MVP.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct HitRegion {
    pub node: NodeId,
    pub bounds: Rect,
    pub clip: Rect,
    /// Painter's-order key — the same monotonic value `PaintCx::next_order` assigns to scene
    /// primitives, so hit z matches draw z (front-most = largest `order`).
    pub order: u32,
    pub flags: InteractFlags,
}

impl HitRegion {
    /// Whether `p` falls inside this region: within `bounds` and not clipped away. Uses the
    /// half-open [`Rect::contains`] (top-left inclusive, far edge exclusive) so adjacent regions
    /// never both claim a shared boundary point.
    fn contains(&self, p: Point) -> bool {
        self.bounds.contains(p) && self.clip.contains(p)
    }
}

/// A frame's recorded interactive regions, in record (paint-traversal) order. Pick queries resolve
/// front-to-back via each region's `order`.
#[derive(Default, Debug)]
pub struct HitTest {
    regions: Vec<HitRegion>,
}

impl HitTest {
    /// Creates an empty hit-test.
    pub fn new() -> Self {
        Self::default()
    }

    /// Clears all regions, retaining allocated capacity for the next frame (perf.md), mirroring
    /// [`Scene::clear`](kagari_render::Scene::clear).
    pub fn clear(&mut self) {
        self.regions.clear();
    }

    /// Records a region (called during paint by the recorder #48 wires in).
    pub fn push(&mut self, region: HitRegion) {
        self.regions.push(region);
    }

    /// The topmost node under `p`: the containing region with the largest `order` (front-most), or
    /// `None` when `p` is in no region.
    pub fn pick(&self, p: Point) -> Option<NodeId> {
        self.regions
            .iter()
            .filter(|r| r.contains(p))
            .max_by_key(|r| r.order)
            .map(|r| r.node)
    }

    /// Every node under `p`, front-to-back (descending `order`).
    pub fn pick_all(&self, p: Point) -> Vec<NodeId> {
        let mut hits: Vec<&HitRegion> = self.regions.iter().filter(|r| r.contains(p)).collect();
        hits.sort_by_key(|r| std::cmp::Reverse(r.order));
        hits.into_iter().map(|r| r.node).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kagari_base::NodeId;

    /// A region for `node` at the given xywh, clipped to its own bounds (the common case), with the
    /// given painter order and the `mouse` interaction flag set.
    fn region(node: u64, x: f32, y: f32, w: f32, h: f32, order: u32) -> HitRegion {
        let bounds = Rect::from_xywh(x, y, w, h);
        HitRegion {
            node: NodeId::from_raw(node),
            bounds,
            clip: bounds,
            order,
            flags: InteractFlags {
                mouse: true,
                ..InteractFlags::default()
            },
        }
    }

    #[test]
    fn pick_should_return_topmost_by_order() {
        // Two overlapping regions cover (5,5); the one with the larger order is front-most.
        let mut ht = HitTest::new();
        ht.push(region(1, 0.0, 0.0, 10.0, 10.0, 0));
        ht.push(region(2, 0.0, 0.0, 10.0, 10.0, 1));
        assert_eq!(ht.pick(Point::new(5.0, 5.0)), Some(NodeId::from_raw(2)));
    }

    #[test]
    fn pick_should_miss_outside_clip() {
        // The point is inside `bounds` but outside the (smaller) `clip`, so it must not hit.
        let mut ht = HitTest::new();
        ht.push(HitRegion {
            node: NodeId::from_raw(1),
            bounds: Rect::from_xywh(0.0, 0.0, 100.0, 100.0),
            clip: Rect::from_xywh(0.0, 0.0, 10.0, 10.0),
            order: 0,
            flags: InteractFlags::default(),
        });
        // (50,50) is in bounds but outside the 10x10 clip.
        assert_eq!(ht.pick(Point::new(50.0, 50.0)), None);
        // (5,5) is inside both.
        assert_eq!(ht.pick(Point::new(5.0, 5.0)), Some(NodeId::from_raw(1)));
    }

    #[test]
    fn pick_should_return_none_outside_all() {
        let mut ht = HitTest::new();
        ht.push(region(1, 0.0, 0.0, 10.0, 10.0, 0));
        assert_eq!(ht.pick(Point::new(50.0, 50.0)), None);
    }

    #[test]
    fn pick_all_should_order_front_to_back() {
        // Three stacked regions over (5,5), pushed out of order; pick_all returns descending order.
        let mut ht = HitTest::new();
        ht.push(region(1, 0.0, 0.0, 10.0, 10.0, 0));
        ht.push(region(3, 0.0, 0.0, 10.0, 10.0, 2));
        ht.push(region(2, 0.0, 0.0, 10.0, 10.0, 1));
        assert_eq!(
            ht.pick_all(Point::new(5.0, 5.0)),
            vec![
                NodeId::from_raw(3),
                NodeId::from_raw(2),
                NodeId::from_raw(1),
            ],
        );
    }

    #[test]
    fn clear_should_retain_capacity() {
        let mut ht = HitTest::new();
        ht.push(region(1, 0.0, 0.0, 10.0, 10.0, 0));
        ht.push(region(2, 0.0, 0.0, 10.0, 10.0, 1));
        let cap = ht.regions.capacity();
        ht.clear();
        assert!(ht.pick(Point::new(5.0, 5.0)).is_none());
        assert!(ht.regions.capacity() >= cap, "capacity retained for reuse");
    }
}
