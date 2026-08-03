//! The [`VirtualizedList`] element (#61): a fixed-height virtualized list that realizes only the rows
//! in the visible range (plus a buffer), reusing [`DynList`](super::DynList)'s keyed reconcile. It
//! reads a [`ScrollHandle`] offset, positions each realized row by its display slot at
//! `slot * item_height - offset`, and clips to a fixed viewport (a standalone virtual viewport — not
//! nested in a #60 `Scroll`). The inner reconcile keys rows on a caller-supplied **logical id** (default
//! identity = the slot, via [`virtualized_list`]; a re-mapping caller uses [`virtualized_list_keyed`],
//! #325/RK-049), and paint positions by realized order so a re-key repositions rows without stale cells.

use kagari_base::{Color, Corners, Edges, NodeId, Point, Rect, Size};
use kagari_layout::scroll::{thumb, thumb_drag_to_offset, visible_range};
use kagari_layout::{LayoutStyle, Overflow};
use kagari_render::{Border, Quad, RoundedRect};

use super::scroll::{SCROLLBAR_THICKNESS, scrollbar_thumb_bg};
use super::{
    AnyElement, DynList, Element, Event, EventCx, IntoElement, LayoutCx, PaintCx, ScrollHandle,
    div, dyn_list,
};
use crate::event::{HitRegion, InteractFlags, MouseButton, MouseEvent, MouseKind};

/// Extra rows realized on each side of the visible window, so a scroll of a fraction of a row does
/// not flash un-built content at the edges.
const BUFFER: usize = 2;

/// An in-progress overlay-scrollbar-thumb drag on the (vertical-only) virtualized list: the pointer y
/// where the grab began and the scroll offset at that moment. Mirrors `Scroll`'s `ThumbDrag`, but the
/// list scrolls vertically only, so there is one axis. `None` on a bare hover — the guard against a
/// hover scrubbing the offset (a `Move` also delivers to the captured node once a drag is active).
struct ThumbDragV {
    start_mouse: f32,
    start_offset: f32,
}

/// A fixed-height virtualized list: only the rows intersecting the viewport (± a buffer) are built,
/// and an offset change incrementally realizes/releases rows by their **logical-id key** (in-range ids
/// keep their `NodeId`). Rows are `item_height` tall and positioned by their display slot at
/// `slot * item_height - offset` (realized in slot order, so the n-th realized row is the n-th visible
/// slot). The key defaults to the slot itself ([`virtualized_list`], identity); a caller whose slot→row
/// mapping changes uses [`virtualized_list_keyed`] so a re-key repositions rows without stale cells.
///
/// Built with [`virtualized_list`]. The offset comes from a [`ScrollHandle`] (shared with a scroll
/// gesture / #60 scrollbar); [`reconcile`](Self::reconcile) applies a scroll's range change (the
/// frame scheduler #36 drives it — tests call it explicitly, like [`DynList`]).
pub struct VirtualizedList {
    list: DynList<usize, usize>,
    item_height: f32,
    /// Total row count — the content height is `count * item_height` (the list is virtualized, so the
    /// extent can't be measured from laid-out children the way `Scroll` does). Drives the overlay thumb.
    count: usize,
    offset: ScrollHandle,
    viewport: Size,
    id: Option<NodeId>,
    /// The vertical overlay-scrollbar thumb rect from the last paint (absolute coords), retained so a
    /// mouse-down can hit-test against exactly the visible thumb. `None` when content fits (no thumb).
    thumb: Option<Rect>,
    /// The in-progress thumb drag, if any.
    drag: Option<ThumbDragV>,
}

/// Creates a fixed-height virtualized list of `count` rows, each `item_height` tall, clipped to
/// `viewport`. `view_fn(index)` builds row `index`; the offset is read from `offset`. Only the rows
/// in the visible range (± a buffer) are ever built. Rows are keyed by their display index — correct
/// for a **fixed** dataset (a scroll reuses in-range rows), but if the *mapping* from display slot to
/// underlying row changes (e.g. a re-sort or filter over the same slots), use
/// [`virtualized_list_keyed`] so kept rows refresh (RK-049).
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
    // The default identity key: the logical id of display slot `s` is `s` — behaviour identical to the
    // pre-#325 list (paint by realized order == paint by slot when the key is the slot).
    virtualized_list_keyed(item_height, count, viewport, offset, |slot| slot, view_fn)
}

/// Like [`virtualized_list`], but the inner keyed reconcile identifies rows by a **logical id** rather than the
/// display slot: `key_fn(slot)` maps a display slot to the id of the row shown there, and `view_fn(id)` builds a
/// row **for that id** (from stable, id-addressed data). This fixes the stale-cell trap (RK-049) when the slot→row
/// mapping changes without changing the visible slot set — a header **re-sort** (#325) or a filter: a kept id keeps
/// its (already-correct) node and is repositioned to its new slot, a newly-visible id is built fresh, and a
/// no-longer-visible id is disposed. `key_fn` is read inside the staging effect, so it must be `Send + Sync` and
/// reactive reads inside it (e.g. a sort-order signal) subscribe the list — a re-key then triggers a reconcile.
pub fn virtualized_list_keyed<V, F, K>(
    item_height: f32,
    count: usize,
    viewport: Size,
    offset: &ScrollHandle,
    key_fn: K,
    view_fn: F,
) -> VirtualizedList
where
    F: Fn(usize) -> V + 'static,
    V: IntoElement,
    K: Fn(usize) -> usize + Send + Sync + 'static,
{
    let handle = offset.clone();
    let (vp_w, vp_h) = (viewport.w, viewport.h);
    // The list's items are the **logical ids** of the visible slots, in slot order; a scroll re-derives them (the
    // read of the offset signal subscribes the staging effect), and a re-key does too (via `key_fn`'s reactive reads).
    let items_fn = move || {
        visible_range(handle.offset().y, vp_h, item_height, count, BUFFER)
            .map(&key_fn)
            .collect::<Vec<usize>>()
    };
    // Each row is wrapped in an `item_height` slot so taffy lays its subtree out at the size the manual paint
    // positions it into. Keyed by **logical id** → a re-key reuses the id's node (content already correct) and the
    // paint repositions it by realized order (= its new visible slot).
    let view = move |id: &usize| {
        div()
            .size(Size {
                w: vp_w,
                h: item_height,
            })
            .child(view_fn(*id))
    };
    let list = dyn_list(items_fn, |id: &usize| *id, view);
    VirtualizedList {
        list,
        item_height,
        count,
        offset: offset.clone(),
        viewport,
        id: None,
        thumb: None,
        drag: None,
    }
}

impl VirtualizedList {
    /// Whether the visible range changed since the last reconcile (the offset scrolled). The frame
    /// scheduler polls this; tests poll it before driving [`Element::reconcile`].
    pub fn is_dirty(&self) -> bool {
        self.list.is_dirty()
    }

    /// Paints the vertical overlay-scrollbar thumb (right edge), sized by the viewport/content ratio and
    /// positioned by the offset — above the rows (highest painter order), recording an interactive hit
    /// region so a mouse-down grabs it. `None` (no thumb, no hit region) when content fits. The thumb rect
    /// is retained in `self.thumb` for the event-time hit-test. Mirrors `Scroll::paint_scrollbars`, but the
    /// list is vertical-only. `content_h` is `count * item_height`; `offset_y` is clamped inside [`thumb`].
    fn paint_scrollbar(&mut self, bounds: Rect, content_h: f32, offset_y: f32, cx: &mut PaintCx) {
        // If an active clip fully culls the viewport, emit no thumb (mirrors `Scroll`'s empty-mask skip).
        let mask_rect = cx.clip_rect(bounds);
        if cx.clip().is_some() && mask_rect.is_empty() {
            self.thumb = None;
            return;
        }
        let thumb_rect =
            thumb(bounds.size.h, content_h, offset_y, bounds.size.h).map(|(pos, len)| {
                Rect::from_xywh(
                    bounds.origin.x + bounds.size.w - SCROLLBAR_THICKNESS,
                    bounds.origin.y + pos,
                    SCROLLBAR_THICKNESS,
                    len,
                )
            });
        if let Some(rect) = thumb_rect {
            let order = cx.next_order();
            cx.scene.quads.push(Quad {
                bounds: rect,
                corner_radii: Corners::default(),
                bg: scrollbar_thumb_bg(),
                border: Border {
                    widths: Edges::default(),
                    color: Color::TRANSPARENT,
                },
                content_mask: RoundedRect {
                    rect: mask_rect,
                    radii: Corners::default(),
                },
                order,
            });
            // The thumb paints last, so its hit `order` is the highest — an edge press grabs the thumb
            // over the row beneath it (RK-016).
            if let Some(id) = self.id {
                cx.record_hit(HitRegion {
                    node: id,
                    bounds: rect,
                    clip: mask_rect,
                    order,
                    flags: InteractFlags {
                        mouse: true,
                        ..InteractFlags::default()
                    },
                });
            }
        }
        self.thumb = thumb_rect;
    }

    /// Handles a mouse event on the list node itself (the overlay-thumb hit region). Down inside the
    /// retained thumb rect captures the pointer + records the grab; a captured Move maps the drag to an
    /// offset (grab point tracks the cursor) and writes it via `jump_to`; Up releases. A Move with no
    /// active drag (a bare hover) is a no-op — `drag` is the guard. Mirrors `Scroll::handle_thumb`.
    fn handle_thumb(&mut self, me: &MouseEvent, cx: &mut EventCx) {
        match me.kind {
            MouseKind::Down(MouseButton::Left) => {
                if self.thumb.is_some_and(|r| r.contains(me.pos)) {
                    cx.capture_pointer();
                    self.drag = Some(ThumbDragV {
                        start_mouse: me.pos.y,
                        start_offset: self.offset.offset_untracked().y,
                    });
                }
            }
            MouseKind::Move => {
                let Some(d) = self.drag.as_ref() else {
                    return;
                };
                let content_h = self.count as f32 * self.item_height;
                let cur = self.offset.offset_untracked();
                let y = thumb_drag_to_offset(
                    d.start_offset,
                    me.pos.y - d.start_mouse,
                    self.viewport.h,
                    content_h,
                    self.viewport.h,
                );
                self.offset.jump_to(Point::new(cur.x, y));
            }
            MouseKind::Up(MouseButton::Left) if self.drag.take().is_some() => {
                cx.release_pointer();
            }
            _ => {}
        }
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
        // An untracked read: paint must not establish a reactive dependency (the repaint seam is the inner
        // `DynList`'s staging effect, which subscribes to the offset and flags structure-dirty on a scroll).
        let offset = self.offset.offset_untracked();
        let item_height = self.item_height;
        // Position each realized row by its display **slot**, not by its (logical-id) key: the realized rows are in
        // `items_fn` order (= `visible_range` order, `apply_staged` preserves it), so the n-th realized row is the
        // n-th visible slot, `start + n`. Within a frame the paint offset == the last-staged offset (RK-009), so
        // `start` matches the staged window. (Under the identity key the id == slot, so this is unchanged.)
        let start = visible_range(offset.y, self.viewport.h, item_height, self.count, BUFFER)
            .next()
            .unwrap_or(0);
        // Clip to the viewport, then paint each realized row at `slot * item_height - offset` (offset-based, not
        // flex). A row fully outside the viewport is clipped away by Div's mask.
        let saved = cx.push_clip(bounds);
        for (n, (_id, _node_id, element)) in self.list.realized_children().enumerate() {
            let y = bounds.origin.y + (start + n) as f32 * item_height - offset.y;
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

        // The true content height is `count * item_height` (only O(visible) rows exist, so it can't be
        // measured from laid-out children the way `Scroll` does). Publish it on the handle so a host or
        // observer can read the extent via `ScrollHandle::content()` (parity with `Scroll`, which a
        // composing scroll surface can rely on); the list widget itself clamps its wheel from its own
        // `count * item_height`. Then paint the overlay thumb above the rows.
        let content_h = self.count as f32 * item_height;
        self.offset
            .store_content(Size::new(bounds.size.w, content_h));
        self.paint_scrollbar(bounds, content_h, offset.y, cx);
    }

    fn handle_event(&mut self, ev: &Event, cx: &mut EventCx) {
        // A container: route the event down to the realized row on the dispatch path (like `Scroll`/`Div`),
        // so a hit on a virtualized row reaches its handlers. Only realized rows are on the path
        // (O(visible)); the row's own `Div::handle_event` fires / filters its listeners. Rows scrolled out
        // of the window are released, so they are structurally off the path. Falls through (no early return)
        // so the overlay-thumb self-fire below still runs when the thumb itself is the target.
        let Some(id) = self.id else {
            return;
        };
        if let Some(next) = cx.next_child_on_path(id) {
            for (_, node_id, element) in self.list.realized_children() {
                if node_id == next {
                    element.handle_event(ev, cx);
                    break;
                }
            }
        }
        if cx.is_stopped() {
            return;
        }
        // Self-fire: the painted overlay thumb records a hit region on THIS node (highest order), so a
        // mouse-down/drag on it delivers here (or to this captured node during a drag). Drives the offset.
        // Mirrors `Scroll::handle_event`; the offset handle is always bound, so the thumb is always drivable.
        if let Event::Mouse(me) = ev {
            if cx.should_fire(id) {
                cx.set_current(id);
                self.handle_thumb(me, cx);
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
    use crate::reactive::RwSignal;
    use crate::reactive::prelude::*;
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
    fn virtualized_keyed_should_reposition_on_key_remap() {
        // A keyed list (#325 fix a / RK-049): each row's WIDTH encodes its logical id `(id+1)*10`; `order` is the
        // display→logical permutation (a "sort") that `key_fn` reads. A re-sort must move each logical row to its
        // new slot AND keep its correct content — no stale cells.
        let owner = Owner::new();
        owner.set();

        let order = RwSignal::new((0..10).collect::<Vec<usize>>());
        let handle = ScrollHandle::new();
        let mut env = Env::new();
        let key_fn = move |slot: usize| order.with(|o| o[slot]);
        let mut vl = virtualized_list_keyed(
            20.0,
            10,
            Size { w: 200.0, h: 100.0 },
            &handle,
            key_fn,
            |id| {
                div()
                    .size(Size {
                        w: (id + 1) as f32 * 10.0,
                        h: 20.0,
                    })
                    .background(green())
            },
        );
        let vid = vl.request_layout(&mut env.cx());

        // The width of the (green) row painted at slot 0 (viewport-top, y == 0) — its logical id is `w/10 - 1`.
        fn width_at_top(env: &mut Env, vl: &mut VirtualizedList, vid: NodeId) -> f32 {
            env.layout
                .compute(vid, Size { w: 200.0, h: 100.0 })
                .unwrap();
            let mut scene = Scene::new();
            let bounds = env.layout.layout(vid);
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
            scene
                .quads
                .iter()
                .filter(|q| q.bg == green())
                .find(|q| q.bounds.origin.y == 0.0)
                .map(|q| q.bounds.size.w)
                .expect("a row paints at slot 0 (y == 0)")
        }

        assert_eq!(
            width_at_top(&mut env, &mut vl, vid),
            10.0,
            "identity order: slot 0 shows logical id 0 (width 10)"
        );

        // Re-sort: reverse the order so slot 0 now shows logical id 9 (width 100).
        order.set((0..10).rev().collect::<Vec<usize>>());
        assert!(vl.is_dirty(), "a re-key flags the visible range dirty");
        vl.reconcile(&mut env.cx());
        assert_eq!(
            width_at_top(&mut env, &mut vl, vid),
            100.0,
            "after the re-sort slot 0 shows logical id 9 (width 100) — fresh content, not stale"
        );

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
            !scene.quads.iter().any(|q| q.bg == green()),
            "the over-scrolled tail rows are clipped above the viewport"
        );
        // The overlay thumb self-clamps to the track bottom, so it still paints (the scene isn't blank).
        assert!(
            scene.quads.iter().any(|q| q.bg == thumb_bg()),
            "the overlay thumb paints at the track bottom even when over-scrolled"
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

    // --- Overlay scrollbar (#321): a draggable vertical thumb on the virtualized list ---

    /// The overlay-scrollbar thumb fill — the same shared helper the element paints with (in scope via
    /// `use super::*`), so this in-crate test can't silently drift from the production color.
    fn thumb_bg() -> Background {
        scrollbar_thumb_bg()
    }

    /// Builds a `count`-row list (20px rows) in a 100×100 viewport bound to `handle`, lays it out and
    /// paints it with a hit-test — returning the root, id, and scene so a test can inspect the thumb quad
    /// and dispatch mouse events against its recorded hit region.
    fn painted_vl(
        handle: &ScrollHandle,
        env: &mut Env,
        hit: &mut HitTest,
        count: usize,
    ) -> (AnyElement, NodeId, Scene) {
        let mut root: AnyElement =
            virtualized_list(20.0, count, Size { w: 100.0, h: 100.0 }, handle, |_i| {
                div().size(Size { w: 100.0, h: 20.0 }).background(green())
            })
            .into_element();
        let id = root.request_layout(&mut env.cx());
        env.layout.compute(id, Size { w: 100.0, h: 100.0 }).unwrap();
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
        (root, id, scene)
    }

    /// Dispatches a single mouse event at a window point.
    fn send(
        root: &mut AnyElement,
        arena: &Arena,
        hit: &HitTest,
        st: &mut DispatchState,
        k: MouseKind,
        x: f32,
        y: f32,
    ) {
        let ev = MouseEvent::new(k, Point::new(x, y), Modifiers::default());
        dispatch_mouse(root, arena, hit, &ev, st);
    }

    #[test]
    fn virtualized_should_paint_overlay_scrollbar_thumb() {
        let owner = Owner::new();
        owner.set();
        let handle = ScrollHandle::new();
        let mut env = Env::new();
        let mut hit = HitTest::new();
        // 20 rows of 20px (content 400) in a 100 viewport overflows → a vertical thumb, len 100·100/400 = 25.
        let (_root, _id, scene) = painted_vl(&handle, &mut env, &mut hit, 20);
        let thumb = scene
            .quads
            .iter()
            .find(|q| q.bg == thumb_bg())
            .expect("overflowing content paints an overlay scrollbar thumb");
        assert_eq!(
            thumb.bounds.size.w, SCROLLBAR_THICKNESS,
            "the thumb is thin"
        );
        assert_eq!(
            thumb.bounds.size.h, 25.0,
            "the thumb length reflects the viewport/content ratio (100/400 of 100)"
        );
        drop(owner);
    }

    #[test]
    fn virtualized_should_paint_no_thumb_when_content_fits() {
        let owner = Owner::new();
        owner.set();
        let handle = ScrollHandle::new();
        let mut env = Env::new();
        let mut hit = HitTest::new();
        // 3 rows of 20px (content 60) fit the 100 viewport → no overflow → no thumb (no dead zone).
        let (_root, _id, scene) = painted_vl(&handle, &mut env, &mut hit, 3);
        assert!(
            !scene.quads.iter().any(|q| q.bg == thumb_bg()),
            "content that fits paints no scrollbar thumb"
        );
        drop(owner);
    }

    #[test]
    fn virtualized_thumb_drag_should_scroll() {
        let owner = Owner::new();
        owner.set();
        let handle = ScrollHandle::new();
        let mut env = Env::new();
        let mut hit = HitTest::new();
        // 20 rows / content 400 / viewport 100 → thumb len 25 at x∈[94,100], y∈[0,25]; free track 75, max 300.
        let (mut root, _id, _scene) = painted_vl(&handle, &mut env, &mut hit, 20);
        // Grab the thumb at (97,10) and drag down 37.5px: offset = 37.5 · 300/75 = 150 (grab tracks cursor).
        let mut st = DispatchState::default();
        send(
            &mut root,
            &env.arena,
            &hit,
            &mut st,
            MouseKind::Down(MouseButton::Left),
            97.0,
            10.0,
        );
        send(
            &mut root,
            &env.arena,
            &hit,
            &mut st,
            MouseKind::Move,
            97.0,
            47.5,
        );
        assert!(
            (handle.offset().y - 150.0).abs() < 0.01,
            "dragging the thumb scrolls by the inverse-mapped amount (got {})",
            handle.offset().y
        );
        drop(owner);
    }

    #[test]
    fn virtualized_thumb_up_should_release_and_reset() {
        let owner = Owner::new();
        owner.set();
        let handle = ScrollHandle::new();
        let mut env = Env::new();
        let mut hit = HitTest::new();
        // 20 rows / content 400 / viewport 100 → thumb at x∈[94,100], y∈[0,25]; free track 75, max 300.
        let (mut root, _id, _scene) = painted_vl(&handle, &mut env, &mut hit, 20);
        let mut st = DispatchState::default();
        // Down + Move to y=25 → drag delta 15 → offset 15·300/75 = 60; Up releases; a later Move must NOT
        // scrub (the `self.drag.take()` on Up reset the grab). Mirrors Scroll's release-and-reset test.
        send(
            &mut root,
            &env.arena,
            &hit,
            &mut st,
            MouseKind::Down(MouseButton::Left),
            97.0,
            10.0,
        );
        send(
            &mut root,
            &env.arena,
            &hit,
            &mut st,
            MouseKind::Move,
            97.0,
            25.0,
        );
        assert!(
            (handle.offset().y - 60.0).abs() < 0.01,
            "the drag scrolled to 60 (got {})",
            handle.offset().y
        );
        send(
            &mut root,
            &env.arena,
            &hit,
            &mut st,
            MouseKind::Up(MouseButton::Left),
            97.0,
            25.0,
        );
        send(
            &mut root,
            &env.arena,
            &hit,
            &mut st,
            MouseKind::Move,
            97.0,
            60.0,
        );
        assert!(
            (handle.offset().y - 60.0).abs() < 0.01,
            "after release, a hover-move does not continue scrubbing (got {})",
            handle.offset().y
        );
        drop(owner);
    }

    #[test]
    fn virtualized_hover_thumb_should_not_scroll() {
        let owner = Owner::new();
        owner.set();
        let handle = ScrollHandle::new();
        let mut env = Env::new();
        let mut hit = HitTest::new();
        let (mut root, _id, _scene) = painted_vl(&handle, &mut env, &mut hit, 20);
        // A bare Move over the thumb (no prior Down) also fires on_mouse_move — it must not scrub.
        let mut st = DispatchState::default();
        send(
            &mut root,
            &env.arena,
            &hit,
            &mut st,
            MouseKind::Move,
            97.0,
            12.0,
        );
        assert_eq!(
            handle.offset().y,
            0.0,
            "hovering the thumb without a grab does not scroll"
        );
        drop(owner);
    }
}
