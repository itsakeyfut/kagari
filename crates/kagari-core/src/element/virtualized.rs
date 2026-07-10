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
    /// scheduler polls this; tests poll it before driving [`Element::reconcile`].
    pub fn is_dirty(&self) -> bool {
        self.list.is_dirty()
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

    fn handle_event(&mut self, ev: &Event, cx: &mut EventCx) {
        // A container: route the event down to the realized row on the dispatch path (like `Scroll`/`Div`),
        // so a hit on a virtualized row reaches its handlers. Only realized rows are on the path
        // (O(visible)); the row's own `Div::handle_event` fires / filters its listeners. Rows scrolled out
        // of the window are released, so they are structurally off the path.
        let Some(id) = self.id else {
            return;
        };
        let Some(next) = cx.next_child_on_path(id) else {
            return;
        };
        for (_, node_id, element) in self.list.realized_children() {
            if node_id == next {
                element.handle_event(ev, cx);
                return;
            }
        }
    }

    fn reconcile(&mut self, cx: &mut LayoutCx) {
        // A container: realize the inner list's staged range change (scrolled-in/out rows) live, then
        // recurse into the realized rows so nested dyn nodes inside a row reconcile too (#278/#86).
        self.list.apply_staged(cx);
        for (_, _, element) in self.list.realized_children() {
            element.reconcile(cx);
        }
    }
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
    use crate::event::{
        DispatchState, HitTest, Modifiers, MouseButton, MouseEvent, MouseKind, dispatch_mouse,
    };
    use kagari_base::Color;
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};
    use reactive_graph::owner::Owner;
    use std::cell::Cell;
    use std::rc::Rc;
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

    /// Builds a 1000-row list (20px rows, 100px viewport) whose rows are built by `build_row`, lays it out
    /// and paints it (recording hit regions at the current offset positions).
    fn vl_with<V: IntoElement>(
        handle: &ScrollHandle,
        env: &mut Env,
        hit: &mut HitTest,
        build_row: impl Fn(usize) -> V + 'static,
    ) -> (AnyElement, NodeId) {
        let mut root: AnyElement =
            virtualized_list(20.0, 1000, Size { w: 100.0, h: 100.0 }, handle, build_row)
                .into_element();
        let id = root.request_layout(&mut env.cx());
        env.layout.compute(id, Size { w: 100.0, h: 100.0 }).unwrap();
        repaint(&mut root, env, hit, id);
        (root, id)
    }

    /// A `vl_with` whose rows record the last-clicked index into `clicked`.
    fn clickable_vl(
        handle: &ScrollHandle,
        env: &mut Env,
        hit: &mut HitTest,
    ) -> (AnyElement, NodeId, Rc<Cell<Option<usize>>>) {
        let clicked = Rc::new(Cell::new(None));
        let c = clicked.clone();
        let (root, id) = vl_with(handle, env, hit, move |i| {
            let c = c.clone();
            div()
                .size(Size { w: 100.0, h: 20.0 })
                .on_click(move |_, _| c.set(Some(i)))
        });
        (root, id, clicked)
    }

    /// Clears the hit-test and repaints, so hit regions reflect the rows' current painted offset positions.
    fn repaint(root: &mut AnyElement, env: &mut Env, hit: &mut HitTest, id: NodeId) {
        hit.clear();
        let mut scene = Scene::new();
        root.paint(
            env.layout.layout(id),
            &mut PaintCx::new(
                &mut scene,
                &env.layout,
                &mut env.text,
                None,
                Some(hit),
                &env.theme,
            ),
        );
    }

    /// Dispatches a full click (down + up = a synthesized click) at a window point.
    fn click(root: &mut AnyElement, arena: &Arena, hit: &HitTest, x: f32, y: f32) {
        let mut dispatch = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(kind, Point::new(x, y), Modifiers::default());
            dispatch_mouse(root, arena, hit, &ev, &mut dispatch);
        }
    }

    #[test]
    fn virtualized_row_click_should_reach_its_handler() {
        let owner = Owner::new();
        owner.set();
        let handle = ScrollHandle::new();
        let mut env = Env::new();
        let mut hit = HitTest::new();
        let (mut root, _id, clicked) = clickable_vl(&handle, &mut env, &mut hit);

        // At offset 0 the window point (50,50) is over index 2 (its row paints at y ∈ [40,60)).
        click(&mut root, &env.arena, &hit, 50.0, 50.0);
        assert_eq!(
            clicked.get(),
            Some(2),
            "a click on a realized row reaches that row's handler"
        );
        drop(owner);
    }

    #[test]
    fn virtualized_click_should_track_scroll_offset() {
        let owner = Owner::new();
        owner.set();
        let handle = ScrollHandle::new();
        let mut env = Env::new();
        let mut hit = HitTest::new();
        let (mut root, id, clicked) = clickable_vl(&handle, &mut env, &mut hit);

        click(&mut root, &env.arena, &hit, 50.0, 50.0);
        assert_eq!(clicked.get(), Some(2), "offset 0: (50,50) hits index 2");

        // Scroll down 200px (10 rows): realize the shifted window (8..17), re-paint (hit regions follow the
        // new offset), then click the same window point — it now lands on index 12, not the released index 2.
        handle.jump_to(Point::new(0.0, 200.0));
        root.reconcile(&mut env.cx());
        env.layout.compute(id, Size { w: 100.0, h: 100.0 }).unwrap();
        repaint(&mut root, &mut env, &mut hit, id);

        click(&mut root, &env.arena, &hit, 50.0, 50.0);
        assert_eq!(
            clicked.get(),
            Some(12),
            "hit regions follow the painted offset: (50,50) now hits index 12, and the scrolled-out index 2 \
             is no longer on the event path"
        );
        drop(owner);
    }

    #[test]
    fn virtualized_row_hover_should_reach_its_handler() {
        let owner = Owner::new();
        owner.set();
        let handle = ScrollHandle::new();
        let mut env = Env::new();
        let mut hit = HitTest::new();
        let entered = Rc::new(Cell::new(None));
        let e = entered.clone();
        let (mut root, _id) = vl_with(&handle, &mut env, &mut hit, move |i| {
            let e = e.clone();
            div()
                .size(Size { w: 100.0, h: 20.0 })
                .on_mouse_enter(move |_, _| e.set(Some(i)))
        });

        // A pointer Move over index 2 (row at y ∈ [40,60)) fires its enter handler — the same generic descend
        // routes hover (a Move enter/leave delivery) to the realized row, not just clicks.
        let mut dispatch = DispatchState::default();
        let ev = MouseEvent::new(
            MouseKind::Move,
            Point::new(50.0, 50.0),
            Modifiers::default(),
        );
        dispatch_mouse(&mut root, &env.arena, &hit, &ev, &mut dispatch);
        assert_eq!(
            entered.get(),
            Some(2),
            "a hover over a realized row reaches that row's mouse-enter handler"
        );
        drop(owner);
    }
}
