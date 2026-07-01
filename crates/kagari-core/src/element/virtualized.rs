//! The [`VirtualizedList`] element (#61): a fixed-height virtualized list that realizes only the rows
//! in the visible range (plus a buffer), reusing [`DynList`](super::DynList)'s keyed reconcile. It
//! reads a [`ScrollHandle`] offset, positions each realized row at `index * item_height - offset`, and
//! clips to a fixed viewport (a standalone virtual viewport — not nested in a #60 `Scroll`).

use kagari_base::{NodeId, Point, Rect, Size};
use kagari_layout::scroll::visible_range;
use kagari_layout::{LayoutStyle, Overflow};

use super::{
    AnyElement, DynList, Element, Event, EventCx, IntoElement, LayoutCx, PaintCx, ScrollHandle,
    div, dyn_list,
};

/// Extra rows realized on each side of the visible window, so a scroll of a fraction of a row does
/// not flash un-built content at the edges.
const BUFFER: usize = 2;

/// A fixed-height virtualized list: only the rows intersecting the viewport (± a buffer) are built,
/// and an offset change incrementally realizes/releases rows by index key (in-range rows keep their
/// `NodeId`). Rows are `item_height` tall and positioned at `index * item_height - offset`.
///
/// Built with [`virtualized_list`]. The offset comes from a [`ScrollHandle`] (shared with a scroll
/// gesture / #60 scrollbar); [`reconcile`](Self::reconcile) applies a scroll's range change (the
/// frame scheduler #36 drives it — tests call it explicitly, like [`DynList`]).
pub struct VirtualizedList {
    list: DynList<usize, usize>,
    item_height: f32,
    offset: ScrollHandle,
    viewport: Size,
    id: Option<NodeId>,
}

/// Creates a fixed-height virtualized list of `count` rows, each `item_height` tall, clipped to
/// `viewport`. `view_fn(index)` builds row `index`; the offset is read from `offset`. Only the rows
/// in the visible range (± a buffer) are ever built.
pub fn virtualized_list<V, F>(
    item_height: f32,
    count: usize,
    viewport: Size,
    offset: &ScrollHandle,
    view_fn: F,
) -> VirtualizedList
where
    F: Fn(usize) -> V + 'static,
    V: IntoElement,
{
    let handle = offset.clone();
    let (vp_w, vp_h) = (viewport.w, viewport.h);
    // The list's items are the visible indices; a scroll re-derives them (the read of the offset
    // signal subscribes the staging effect, so a `scroll_to` flags a reconcile).
    let items_fn = move || {
        visible_range(handle.offset().y, vp_h, item_height, count, BUFFER).collect::<Vec<usize>>()
    };
    // Each row is wrapped in an `item_height` slot so taffy lays its subtree out at the size the
    // manual paint positions it into. Keyed by index → in-range rows are reused across scrolls.
    let view = move |idx: &usize| {
        div()
            .size(Size {
                w: vp_w,
                h: item_height,
            })
            .child(view_fn(*idx))
    };
    let list = dyn_list(items_fn, |idx: &usize| *idx, view);
    VirtualizedList {
        list,
        item_height,
        offset: offset.clone(),
        viewport,
        id: None,
    }
}

impl VirtualizedList {
    /// Whether the visible range changed since the last reconcile (the offset scrolled). The frame
    /// scheduler (#36) polls this; tests call it before [`reconcile`](Self::reconcile).
    pub fn is_dirty(&self) -> bool {
        self.list.is_dirty()
    }

    /// Realizes newly-visible rows and releases scrolled-out ones — delegating to the inner keyed
    /// reconcile, so per-child owner cleanup + `remove_subtree` (RK-005/006) are preserved.
    pub fn reconcile(&mut self, cx: &mut LayoutCx) {
        self.list.reconcile(cx);
    }
}

impl Element for VirtualizedList {
    fn request_layout(&mut self, cx: &mut LayoutCx) -> NodeId {
        // Delegate structure to the inner DynList (builds the node + the initial visible rows).
        let id = self.list.request_layout(cx);
        if self.id.is_none() {
            self.id = Some(id);
            // Fixed viewport: size the node so paint's `bounds` is the clip viewport. `Overflow::Scroll`
            // is semantic; the clip itself is applied in `paint`. Set once (static layout).
            cx.layout.set_style(
                id,
                &LayoutStyle {
                    size: Some(self.viewport),
                    overflow: Overflow::Scroll,
                    ..LayoutStyle::default()
                },
            );
        }
        id
    }

    fn paint(&mut self, bounds: Rect, cx: &mut PaintCx) {
        let offset = self.offset.offset();
        let item_height = self.item_height;
        // Clip to the viewport, then paint each realized row at `index * item_height - offset`
        // (offset-based, not flex). A row fully outside the viewport is clipped away by Div's mask.
        let saved = cx.push_clip(bounds);
        for (&idx, _node_id, element) in self.list.realized_children() {
            let y = bounds.origin.y + idx as f32 * item_height - offset.y;
            let row_bounds = Rect {
                origin: Point {
                    x: bounds.origin.x,
                    y,
                },
                size: Size {
                    w: bounds.size.w,
                    h: item_height,
                },
            };
            element.paint(row_bounds, cx);
        }
        cx.set_clip(saved);
    }

    fn handle_event(&mut self, _ev: &Event, _cx: &mut EventCx) {}
}

impl IntoElement for VirtualizedList {
    fn into_element(self) -> AnyElement {
        Box::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::Arena;
    use crate::element::DamageSink;
    use kagari_base::Color;
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};
    use reactive_graph::owner::Owner;
    use std::sync::Arc;

    struct NoopDamage;
    impl DamageSink for NoopDamage {
        fn mark_paint_dirty(&self, _id: NodeId) {}
    }

    fn green() -> Background {
        Background::Solid(Color::new(0.0, 1.0, 0.0, 1.0))
    }

    /// A build environment mirroring the DynList tests: an arena/layout/text a `LayoutCx` borrows.
    struct Env {
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        damage: Arc<dyn DamageSink>,
        theme: Theme,
    }
    impl Env {
        fn new() -> Self {
            Self {
                arena: Arena::new(),
                layout: LayoutTree::new(),
                text: TextSystem::new(FontDb::new()),
                damage: Arc::new(NoopDamage),
                theme: Theme::default(),
            }
        }
        fn cx(&mut self) -> LayoutCx<'_> {
            LayoutCx {
                arena: &mut self.arena,
                layout: &mut self.layout,
                text: &mut self.text,
                damage: Arc::clone(&self.damage),
                theme: &self.theme,
                focus: None,
                cursor: None,
            }
        }
    }

    #[test]
    fn virtualized_should_build_only_visible_items() {
        // A 1000-row list in a 100px viewport of 20px rows realizes only the visible window (0..5)
        // plus the ±2 buffer = 7 rows — never all 1000. `RwSignal::new` needs an owner (RK-003).
        let owner = Owner::new();
        owner.set();

        let handle = ScrollHandle::new();
        let mut env = Env::new();
        let mut vl = virtualized_list(20.0, 1000, Size { w: 100.0, h: 100.0 }, &handle, |_i| {
            div().size(Size { w: 100.0, h: 20.0 }).background(green())
        });
        let id = vl.request_layout(&mut env.cx());

        let realized = env.arena.get(id).unwrap().children.len();
        assert_eq!(
            realized, 7,
            "only the visible rows (0..5) + buffer (±2) are built"
        );
        assert!(realized < 1000, "the huge list is not fully realized");

        drop(owner);
    }

    #[test]
    fn virtualized_should_reuse_rows_on_scroll() {
        // Scrolling shifts the window: rows still visible keep their NodeId (reuse-by-index-key),
        // only the boundary rows are added/released. Synchronous effect → hang-free (RK-005).
        let owner = Owner::new();
        owner.set();

        let handle = ScrollHandle::new();
        let mut env = Env::new();
        let mut vl = virtualized_list(20.0, 1000, Size { w: 100.0, h: 100.0 }, &handle, |_i| div());
        let id = vl.request_layout(&mut env.cx());
        let rows0 = env.arena.get(id).unwrap().children.clone(); // indices 0..7
        assert_eq!(rows0.len(), 7);

        // Scroll down 60px (3 rows): visible range 0..7 → 1..10. Between-frames write (RK-009).
        handle.jump_to(Point::new(0.0, 60.0));
        assert!(
            vl.is_dirty(),
            "the offset write flags the visible range dirty"
        );
        vl.reconcile(&mut env.cx());
        let rows1 = env.arena.get(id).unwrap().children.clone(); // indices 1..10

        assert_eq!(rows1.len(), 9);
        assert_eq!(
            rows1[0], rows0[1],
            "index 1 row is reused (same NodeId), not rebuilt"
        );
        assert_eq!(rows1[5], rows0[6], "index 6 row is reused");
        assert!(
            !rows1.contains(&rows0[0]),
            "index 0 scrolled out of the window"
        );
        assert!(
            !env.arena.contains(rows0[0]),
            "the released row's node is freed (remove_subtree, RK-006)"
        );
        assert!(!rows0.contains(&rows1[6]), "index 7 is freshly realized");

        drop(owner);
    }

    #[test]
    fn virtualized_should_position_rows_at_index_times_height_minus_offset() {
        // Pre-scroll so index 3 (3*20 = 60) sits exactly at the viewport top (offset 60). Rows paint
        // at `index * 20 - 60`: index 3 → y 0, index 4 → 20; earlier rows land above and are clipped.
        let owner = Owner::new();
        owner.set();

        let handle = ScrollHandle::new();
        handle.jump_to(Point::new(0.0, 60.0));
        let mut env = Env::new();
        let mut vl = virtualized_list(20.0, 1000, Size { w: 100.0, h: 200.0 }, &handle, |_i| {
            div().size(Size { w: 100.0, h: 20.0 }).background(green())
        });
        let id = vl.request_layout(&mut env.cx());
        env.layout.compute(id, Size { w: 100.0, h: 200.0 }).unwrap();

        let mut scene = Scene::new();
        let bounds = env.layout.layout(id);
        vl.paint(
            bounds,
            &mut PaintCx::new(
                &mut scene,
                &env.layout,
                &mut env.text,
                None,
                None,
                &env.theme,
            ),
        );

        assert!(!scene.quads.is_empty(), "visible rows paint quads");
        assert_eq!(
            scene.quads[0].bounds.origin.y, 0.0,
            "index 3 (3*20) lands at the viewport top for offset 60"
        );
        assert_eq!(
            scene.quads[1].bounds.origin.y, 20.0,
            "the next row is one item_height below"
        );
        assert_eq!(
            scene.quads[0].bounds.size.h, 20.0,
            "rows are item_height tall"
        );
        // Rows above the viewport (indices 1,2 at y -40/-20) are clipped away by Div's mask, not
        // painted above the top — the clip cull is load-bearing for the index-3-at-top result.
        assert!(
            scene.quads.iter().all(|q| q.bounds.origin.y >= 0.0),
            "no row is painted above the viewport top"
        );

        drop(owner);
    }

    #[test]
    fn virtualized_should_clamp_over_scroll() {
        // An offset at the very end of the list (count * item_height = 20000) clamps the window to
        // the tail (998..1000) rather than an out-of-bounds range, and paint does not panic.
        let owner = Owner::new();
        owner.set();

        let handle = ScrollHandle::new();
        handle.jump_to(Point::new(0.0, 20000.0));
        let mut env = Env::new();
        let mut vl = virtualized_list(20.0, 1000, Size { w: 100.0, h: 100.0 }, &handle, |_i| {
            div().size(Size { w: 100.0, h: 20.0 }).background(green())
        });
        let id = vl.request_layout(&mut env.cx());

        let realized = env.arena.get(id).unwrap().children.len();
        assert_eq!(
            realized, 2,
            "the window clamps to the last rows (998, 999), never past count"
        );

        // Paint the over-scrolled list: the tail rows land above the viewport and are clipped — no panic.
        env.layout.compute(id, Size { w: 100.0, h: 100.0 }).unwrap();
        let mut scene = Scene::new();
        vl.paint(
            env.layout.layout(id),
            &mut PaintCx::new(
                &mut scene,
                &env.layout,
                &mut env.text,
                None,
                None,
                &env.theme,
            ),
        );
        assert!(
            scene.quads.is_empty(),
            "the over-scrolled tail is clipped above the viewport"
        );

        drop(owner);
    }

    #[test]
    fn virtualized_should_rebuild_reentered_rows_on_scroll_back() {
        // Index-as-key: a row scrolled fully out is released; scrolling back mints it fresh (its old
        // NodeId is not resurrected). This is the counterpart of the downward-scroll reuse test.
        let owner = Owner::new();
        owner.set();

        let handle = ScrollHandle::new();
        let mut env = Env::new();
        let mut vl = virtualized_list(20.0, 1000, Size { w: 100.0, h: 100.0 }, &handle, |_i| div());
        let id = vl.request_layout(&mut env.cx());
        let index0_node = env.arena.get(id).unwrap().children[0];

        // Scroll down far enough (offset 200 → window 8..17) that index 0 leaves the window.
        handle.jump_to(Point::new(0.0, 200.0));
        vl.reconcile(&mut env.cx());
        assert!(
            !env.arena.contains(index0_node),
            "index 0 is released (node freed) when scrolled out of the window"
        );

        // Scroll back to the top: index 0 re-enters and is rebuilt with a fresh node.
        handle.jump_to(Point::new(0.0, 0.0));
        vl.reconcile(&mut env.cx());
        let reentered = env.arena.get(id).unwrap().children[0];
        assert_ne!(
            reentered, index0_node,
            "the re-entered index 0 gets a fresh NodeId"
        );
        assert!(
            env.arena.contains(reentered),
            "the re-entered row is realized"
        );

        drop(owner);
    }

    #[test]
    fn virtualized_should_build_empty_list_without_panic() {
        // A zero-count list realizes no rows and paints nothing (no panic, no empty-range mishap).
        let owner = Owner::new();
        owner.set();

        let handle = ScrollHandle::new();
        let mut env = Env::new();
        let mut vl = virtualized_list(20.0, 0, Size { w: 100.0, h: 100.0 }, &handle, |_i| {
            div().size(Size { w: 100.0, h: 20.0 }).background(green())
        });
        let id = vl.request_layout(&mut env.cx());
        assert_eq!(
            env.arena.get(id).unwrap().children.len(),
            0,
            "a zero-count list realizes no rows"
        );

        env.layout.compute(id, Size { w: 100.0, h: 100.0 }).unwrap();
        let mut scene = Scene::new();
        vl.paint(
            env.layout.layout(id),
            &mut PaintCx::new(
                &mut scene,
                &env.layout,
                &mut env.text,
                None,
                None,
                &env.theme,
            ),
        );
        assert!(scene.quads.is_empty(), "an empty list paints nothing");

        drop(owner);
    }

    #[test]
    fn virtualized_should_clip_partial_top_row() {
        // A fractional offset (70, not a multiple of 20) makes the top visible row straddle the
        // viewport edge: index 3 paints at y -10 but is masked to its visible 10px (y 0..10).
        let owner = Owner::new();
        owner.set();

        let handle = ScrollHandle::new();
        handle.jump_to(Point::new(0.0, 70.0));
        let mut env = Env::new();
        let mut vl = virtualized_list(20.0, 1000, Size { w: 100.0, h: 200.0 }, &handle, |_i| {
            div().size(Size { w: 100.0, h: 20.0 }).background(green())
        });
        let id = vl.request_layout(&mut env.cx());
        env.layout.compute(id, Size { w: 100.0, h: 200.0 }).unwrap();

        let mut scene = Scene::new();
        vl.paint(
            env.layout.layout(id),
            &mut PaintCx::new(
                &mut scene,
                &env.layout,
                &mut env.text,
                None,
                None,
                &env.theme,
            ),
        );

        // index 3 (60) at offset 70 → y = -10; its child quad keeps that geometry origin but is
        // masked to the visible slice at the viewport top.
        let first = &scene.quads[0];
        assert_eq!(
            first.bounds.origin.y, -10.0,
            "the straddling top row keeps its translated geometry origin"
        );
        assert_eq!(
            first.content_mask.rect.origin.y, 0.0,
            "clipped to the viewport top"
        );
        assert_eq!(
            first.content_mask.rect.size.h, 10.0,
            "the top row is clipped to its visible 10px, not the full 20"
        );

        drop(owner);
    }
}
