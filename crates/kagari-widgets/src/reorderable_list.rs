//! The [`ReorderableList`] widget (#226, live drag #357): a list whose rows are reordered by **dragging** (a row
//! or, with [`.handle`](ReorderableList::handle), a grip) or by the **keyboard** (Alt+Up/Down), bound to a
//! controlled `RwSignal<Vec<T>>` — layer/track order, playlists, list-based inspectors.
//!
//! Reuses the DnD substrate (#178, as [`Dock`](crate::Dock) does). **Live reflow (#357):** while a drag is in
//! progress the list is rendered in a *tentative* order — the dragged row lifts as a ghost (translucency set by
//! [`.ghost_opacity`](ReorderableList::ghost_opacity), #356), its origin slot collapses, and a **placeholder gap**
//! with a colored **insertion line** ([`.insertion_line_color`](ReorderableList::insertion_line_color)) opens at
//! the drop position (siblings shift to make room), marking exactly where the item will land. The
//! order is **not committed** until release: a non-committal `preview_to` (the tentative insert index) + `drag_from`
//! (the dragged origin index, captured in the `drag_source` factory) drive the **single-item rebuild** `dyn_list`
//! (the `tree`/`dropdown` idiom, live via #278); `on_drag_over` updates the preview, drop commits `items`, and
//! drag-leave / cancel reverts it. Keyboard reorder uses a **roving-focus** model — the list is one focus target
//! with an `active` row index (so a full rebuild can't lose per-row focus handles); Arrow keys move the highlight,
//! Alt+Arrow moves the active item (and the highlight follows it). The reflow is instant; smooth animation is #357's
//! follow-up. Dropping in the empty space below the last row reverts (no append) — a trailing drop-zone is a later
//! refinement.

use std::rc::Rc;

use kagari_base::{Point, Size};
use kagari_core::element::{Div, dyn_list};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{
    AnyElement, BoundsHandle, IconId, IntoElement, KeyCode, Role, div, icon, use_focus_handle,
};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, apply_size, label_px};

/// The drag payload: the index of the row being dragged. A private newtype so `drop_target::<RowDrag>` only
/// matches this list's own drags (`DragPayload` via the blanket `impl<T: Any>`).
#[derive(Clone, Copy)]
struct RowDrag(usize);

/// Moves `v[from]` so it sits **before** the position `insert_before` (`0..=len`). Removing `from` shifts
/// everything above it down by one, so the insertion index is decremented when `from < insert_before`. A no-op
/// for an out-of-range `from` or a drop onto the item's own edges.
fn move_item<T>(v: &mut Vec<T>, from: usize, insert_before: usize) {
    if from >= v.len() {
        return;
    }
    let item = v.remove(from);
    let to = if from < insert_before {
        insert_before - 1
    } else {
        insert_before
    };
    v.insert(to.min(v.len()), item);
}

/// The tentative visual order of original indices during a drag (#357): `0..len` with `from` relocated to sit
/// before original position `to` — the same move `move_item` performs on the item vec. The dragged index's slot in
/// this order is where the placeholder gap is rendered.
fn reflow_order(len: usize, from: usize, to: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    move_item(&mut order, from, to);
    order
}

/// The insert-before index a drop/drag-over on original row `oi` resolves to: the pointer's top half of the row
/// picks before it (`oi`), the bottom half after it (`oi + 1`). `bounds` is the row's last painted rect.
fn drop_index(bounds: &BoundsHandle, oi: usize, pointer: Option<Point>) -> usize {
    match pointer {
        Some(p) => {
            let b = bounds.get();
            if p.y < b.origin.y + b.size.h / 2.0 {
                oi
            } else {
                oi + 1
            }
        }
        None => oi,
    }
}

/// Shared, list-scoped drag/reorder state threaded to every row and the gap. All fields are `RwSignal` — cheap
/// `Copy` handles — so this whole context is `Copy` and can be captured into each row's closures without cloning.
/// `items` is the controlled model, `active` the keyboard highlight, and `preview_to`/`drag_from`/`gap_size` the
/// live-reflow state (#357): the tentative insert index, the dragged origin index, and the dragged row's size.
struct DragCtx<T: 'static> {
    items: RwSignal<Vec<T>>,
    active: RwSignal<Option<usize>>,
    preview_to: RwSignal<Option<usize>>,
    drag_from: RwSignal<Option<usize>>,
    gap_size: RwSignal<Size>,
}

// `RwSignal<_>` is `Copy` regardless of its inner type, so `DragCtx` is too — a manual impl avoids the spurious
// `T: Copy` bound that `#[derive(Copy)]` would add (the item type `T` is never itself copied).
impl<T: 'static> Clone for DragCtx<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: 'static> Copy for DragCtx<T> {}

/// Commit a reorder to `items` (`move_item(from, to)`), keep the highlight on the moved item's final index, and
/// clear the live-reflow state (`preview_to` + `drag_from`). Shared by the row and gap drop targets.
fn commit<T: Clone + Send + Sync + 'static>(ctx: DragCtx<T>, from: usize, to: usize) {
    ctx.items.update(|v| move_item(v, from, to));
    let final_idx = if from < to { to.saturating_sub(1) } else { to };
    let len = ctx.items.with_untracked(|v| v.len());
    ctx.active.set(Some(final_idx.min(len.saturating_sub(1))));
    ctx.preview_to.set(None);
    // Reset the dragged-origin index too (state hygiene): it is only meaningful while `preview_to` is `Some`, so a
    // stale value is harmless, but clearing it keeps the "not dragging" state unambiguous. Not cleared in
    // `on_drag_leave` — that fires on every row→row transition, where `drag_from` must persist.
    ctx.drag_from.set(None);
}

/// Attaches the drag source + translucent lift ghost (#356/#357) to `el` (the whole row, or a grip child under
/// [`.handle`](ReorderableList::handle)). The factory runs once at drag start (`begin_drag`): it records the
/// dragged origin index into `drag_from` and the row's painted size into `gap_size` (so the placeholder gap
/// reserves the exact space), then yields the payload. The ghost is the row content at 0.6 opacity, cursor-tracked
/// by the shell.
fn with_drag_source<T: Clone + PartialEq + Send + Sync + 'static>(
    el: Div,
    oi: usize,
    item: T,
    item_fn: Rc<dyn Fn(&T) -> AnyElement>,
    ctx: DragCtx<T>,
    row_bounds: BoundsHandle,
    cfg: ListCfg,
) -> Div {
    el.drag_source(move || {
        ctx.drag_from.set(Some(oi));
        ctx.gap_size.set(row_bounds.get().size);
        RowDrag(oi)
    })
    .drag_image(move || {
        apply_size(div().flex().items_center(), cfg.size)
            .opacity(cfg.ghost_opacity)
            .bg(ColorRole::SurfaceRaised)
            .rounded_md()
            .shadow_sm()
            .child(item_fn(&item))
    })
}

/// One row: the item content, highlighted while it is the `active` (keyboard) row, a drop target that **previews**
/// the reorder on drag-over (`preview_to`) and **commits** it on drop, and — unless [`.handle`](ReorderableList::handle)
/// is set — the whole-row drag source (with `handle`, a grip child is the source instead). `oi` is the item's index
/// in the *original* `items`; drop/preview indices live in that space, independent of the tentative visual position.
fn build_row<T: Clone + PartialEq + Send + Sync + 'static>(
    oi: usize,
    item: T,
    item_fn: Rc<dyn Fn(&T) -> AnyElement>,
    ctx: DragCtx<T>,
    cfg: ListCfg,
) -> Div {
    let bounds = BoundsHandle::new();
    let (b_drop, b_over) = (bounds.clone(), bounds.clone());
    let content = item_fn(&item);

    // The row is the drop target + drag-over/leave preview driver (whether or not it is also the drag source).
    let row = apply_size(div().flex().items_center(), cfg.size)
        .role(Role::ListItem)
        .track_bounds(&bounds)
        .bg(rx(move || {
            if ctx.active.get() == Some(oi) {
                ColorRole::SurfaceRaised
            } else {
                ColorRole::Bg
            }
        }))
        .drop_target::<RowDrag>(move |RowDrag(from), cx| {
            commit(ctx, from, drop_index(&b_drop, oi, cx.drag_pointer()));
        })
        .on_drag_over(move |cx| {
            ctx.preview_to
                .set(Some(drop_index(&b_over, oi, cx.drag_pointer())));
        })
        .on_drag_leave(move |_cx| ctx.preview_to.set(None));

    if cfg.handle {
        // Desktop idiom: a grip child is the sole drag source; the row body is not draggable.
        let grip = with_drag_source(
            div().flex().items_center(),
            oi,
            item,
            item_fn,
            ctx,
            bounds.clone(),
            cfg,
        )
        .child(
            icon(IconId::GripVertical)
                .color_role(ColorRole::TextMuted)
                .size(label_px(cfg.size)),
        );
        row.child(grip).child(content)
    } else {
        // Whole-row drag source (the #226 default).
        with_drag_source(row, oi, item, item_fn, ctx, bounds.clone(), cfg).child(content)
    }
}

/// The placeholder gap (#357): a spacer reserving the dragged row's height at the tentative drop slot (the dragged
/// item's position in the reflowed order), with a thin **insertion line** (`cfg.line_color`) across its top edge
/// marking exactly where the item will land. It is itself a drop target that re-asserts its own insert index `to`
/// on drag-over — so hovering the gap keeps the preview stable (no collapse→reopen flicker) — and a drop onto it
/// commits. Only the *height* is fixed (`min_h`); the width stretches to the list so the line is always full-width.
fn build_gap<T: Clone + Send + Sync + 'static>(
    to: usize,
    gap_size: Size,
    ctx: DragCtx<T>,
    cfg: ListCfg,
) -> Div {
    div()
        .flex_col()
        .min_h(gap_size.h)
        // The drop insertion line: a thin full-width bar at the top of the reserved slot.
        .child(div().h_0p5().bg(cfg.line_color))
        .drop_target::<RowDrag>(move |RowDrag(from), _cx| {
            commit(ctx, from, to);
        })
        .on_drag_over(move |_cx| ctx.preview_to.set(Some(to)))
        .on_drag_leave(move |_cx| ctx.preview_to.set(None))
}

/// Static per-list presentation config threaded to every row/gap builder (all `Copy`), kept separate from the
/// reactive [`DragCtx`] state. `size`/`handle` are the control size and drag-handle idiom; `ghost_opacity` is the
/// lift ghost's translucency (#357); `line_color` is the drop insertion line's color.
#[derive(Clone, Copy)]
struct ListCfg {
    size: ControlSize,
    handle: bool,
    ghost_opacity: f32,
    line_color: ColorRole,
}

/// A reorderable list (#226, live drag #357): rows bound to a controlled `RwSignal<Vec<T>>`, reordered by dragging
/// (a row or, with [`.handle`](Self::handle), a grip) or the keyboard (Alt+Up/Down while focused). The list reflows
/// live during a drag and commits only on release. Build with [`reorderable_list`]; chain [`.size`](Self::size) /
/// [`.handle`](Self::handle) / [`.ghost_opacity`](Self::ghost_opacity) / [`.insertion_line_color`](Self::insertion_line_color).
/// Returns `impl IntoElement`; `Styled` is not on its surface.
pub struct ReorderableList<T> {
    items: RwSignal<Vec<T>>,
    item_fn: Rc<dyn Fn(&T) -> AnyElement>,
    size: ControlSize,
    /// When `true`, only a grip icon on each row is the drag source (desktop idiom); when `false` (default) the
    /// whole row is draggable.
    handle: bool,
    /// The lift ghost's opacity in `[0, 1]` (#357): `0.6` (default) is a semi-transparent carried card; `0.0` a
    /// fully-transparent (invisible) ghost — user preference. Applied via [`Div::opacity`](kagari_core::element::Div::opacity) (#356).
    ghost_opacity: f32,
    /// The drop insertion line's color (#357): a thin horizontal line marks where the dragged item will land.
    /// Defaults to [`ColorRole::Accent`].
    line_color: ColorRole,
}

/// Creates a reorderable list over `items` (controlled, D2); `item_fn` renders each item's row content.
pub fn reorderable_list<T, E>(
    items: RwSignal<Vec<T>>,
    item_fn: impl Fn(&T) -> E + 'static,
) -> ReorderableList<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
    E: IntoElement,
{
    ReorderableList {
        items,
        item_fn: Rc::new(move |t| item_fn(t).into_element()),
        size: ControlSize::Md,
        handle: false,
        ghost_opacity: 0.6,
        line_color: ColorRole::Accent,
    }
}

impl<T: Clone + PartialEq + Send + Sync + 'static> ReorderableList<T> {
    /// Sets the control size (row padding + gap).
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Opts into the drag-**handle** idiom (#357): when `true`, a grip icon on the left of each row is the sole
    /// drag source and the row body is not draggable; when `false` (default) the whole row is the drag source.
    pub fn handle(mut self, handle: bool) -> Self {
        self.handle = handle;
        self
    }

    /// Sets the lift ghost's opacity (#357), clamped to `[0, 1]`. `0.6` (default) carries a semi-transparent copy
    /// of the row; `0.0` makes the ghost fully transparent (invisible) so only the gap + insertion line show the
    /// drop — a user-preference choice between a translucent and a transparent drag.
    pub fn ghost_opacity(mut self, opacity: f32) -> Self {
        self.ghost_opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// Sets the color of the drop **insertion line** (#357) — the thin horizontal line marking where the dragged
    /// item will land. Defaults to [`ColorRole::Accent`].
    pub fn insertion_line_color(mut self, role: ColorRole) -> Self {
        self.line_color = role;
        self
    }
}

impl<T: Clone + PartialEq + Send + Sync + 'static> IntoElement for ReorderableList<T> {
    fn into_element(self) -> AnyElement {
        let ReorderableList {
            items,
            item_fn,
            size,
            handle,
            ghost_opacity,
            line_color,
        } = self;
        let cfg = ListCfg {
            size,
            handle,
            ghost_opacity,
            line_color,
        };
        // Signals created once, so they survive the row rebuilds. `preview_to`/`drag_from`/`gap_size` drive the
        // live reflow (#357): `drag_from` (set in the drag_source factory) is the dragged origin index and
        // `gap_size` its painted size; `preview_to` is the tentative insert index (on_drag_over sets it, drop/leave
        // clear it). `active` is the keyboard highlight.
        let ctx = DragCtx {
            items,
            active: RwSignal::new(None::<usize>),
            preview_to: RwSignal::new(None::<usize>),
            drag_from: RwSignal::new(None::<usize>),
            gap_size: RwSignal::new(Size { w: 0.0, h: 0.0 }),
        };
        let focus = use_focus_handle();

        // Rows, rebuilt on any `items`/preview change (single-item rebuild — the tree/dropdown idiom, live via
        // #278). While a drag is in progress (`preview_to` + `drag_from` set) the list renders the tentative
        // reflowed order with the dragged slot as an empty placeholder gap; otherwise the full list in order.
        // `drag_from` is read **untracked** (it is set at drag start while `preview_to` is still `None`, so a
        // tracked read would force a wasted full rebuild); `preview_to` is the sole rebuild trigger during a drag.
        let list = dyn_list(
            move || {
                vec![(
                    ctx.items.get(),
                    ctx.drag_from.get_untracked(),
                    ctx.preview_to.get(),
                )]
            },
            |v: &(Vec<T>, Option<usize>, Option<usize>)| v.clone(),
            move |(v, from_opt, to_opt): &(Vec<T>, Option<usize>, Option<usize>)| {
                let item_fn = item_fn.clone();
                let len = v.len();
                let mut col = div().flex_col().gap_1();
                match (*from_opt, *to_opt) {
                    (Some(from), Some(to)) if from < len => {
                        let gs = ctx.gap_size.get_untracked();
                        for oi in reflow_order(len, from, to) {
                            col = if oi == from {
                                col.child(build_gap(to, gs, ctx, cfg))
                            } else {
                                col.child(build_row(oi, v[oi].clone(), item_fn.clone(), ctx, cfg))
                            };
                        }
                    }
                    _ => {
                        for (oi, item) in v.iter().enumerate() {
                            col = col.child(build_row(oi, item.clone(), item_fn.clone(), ctx, cfg));
                        }
                    }
                }
                col
            },
        );

        // The container is the single focus target (roving focus): Arrow keys move the highlight, Alt+Arrow
        // moves the active item (and the highlight follows). `Role::List` exposes the list to a screen reader.
        let ring = focus.clone();
        div()
            .flex_col()
            .role(Role::List)
            .track_focus(&focus)
            .border_w_2()
            .border_color(rx(move || {
                ring.is_focus_visible().then_some(ColorRole::FocusRing)
            }))
            .on_key_down(move |kev, cx| {
                if !kev.pressed {
                    return;
                }
                let len = ctx.items.with_untracked(|v| v.len());
                if len == 0 {
                    return;
                }
                let cur = ctx.active.get_untracked();
                match kev.code {
                    KeyCode::ArrowUp if kev.modifiers.alt => {
                        if let Some(i) = cur {
                            if i > 0 {
                                ctx.items.update(|v| move_item(v, i, i - 1));
                                ctx.active.set(Some(i - 1));
                            }
                        }
                        cx.stop_propagation();
                    }
                    KeyCode::ArrowDown if kev.modifiers.alt => {
                        if let Some(i) = cur {
                            if i + 1 < len {
                                ctx.items.update(|v| move_item(v, i, i + 2));
                                ctx.active.set(Some(i + 1));
                            }
                        }
                        cx.stop_propagation();
                    }
                    KeyCode::ArrowUp => {
                        ctx.active
                            .set(Some(cur.map_or(len - 1, |i| i.saturating_sub(1))));
                        cx.stop_propagation();
                    }
                    KeyCode::ArrowDown => {
                        ctx.active
                            .set(Some(cur.map_or(0, |i| (i + 1).min(len - 1))));
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            })
            .child(list)
            .into_element()
    }
}

// The drag ghost (#356): the shell's cursor-tracked painting is not headless-reachable, but the ghost ELEMENT
// is — `dispatch_drag_start` returns it as the second tuple field (an `Option<AnyElement>`). So the ghost's
// opacity/surface/content wiring is asserted by `drag_should_build_translucent_ghost` below (painting it into a
// scene and checking the faded background quad); only the live cursor-follow is left to the example + manual QA.
#[cfg(test)]
mod tests {
    use super::*;
    use std::any::TypeId;
    use std::sync::Arc as StdArc;

    use kagari_base::{NodeId, Point, Size};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::{
        FocusRegistry, HitTest, KeyEvent, Modifiers, dispatch_drag_leave, dispatch_drag_over,
        dispatch_drag_start, dispatch_drop, dispatch_key,
    };
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    use crate::label::label;

    const VIEWPORT: Size = Size { w: 200.0, h: 300.0 };

    #[test]
    fn move_item_should_reorder() {
        let base = || vec!['a', 'b', 'c', 'd'];
        let mut v = base();
        move_item(&mut v, 0, 2); // move 'a' before original index 2 → [b, a, c, d]
        assert_eq!(v, vec!['b', 'a', 'c', 'd']);
        let mut v = base();
        move_item(&mut v, 3, 1); // move 'd' before index 1 → [a, d, b, c]
        assert_eq!(v, vec!['a', 'd', 'b', 'c']);
        let mut v = base();
        move_item(&mut v, 0, 0); // before itself → no-op
        assert_eq!(v, base());
        let mut v = base();
        move_item(&mut v, 0, 1); // just after itself → no-op
        assert_eq!(v, base());
        let mut v = base();
        move_item(&mut v, 0, 4); // to the end → [b, c, d, a]
        assert_eq!(v, vec!['b', 'c', 'd', 'a']);
        let mut v = base();
        move_item(&mut v, 9, 0); // out of range → no-op
        assert_eq!(v, base());
    }

    struct Harness {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        hit: HitTest,
        focus: FocusRegistry,
        damage: StdArc<DamageState>,
        theme: Theme,
        root_id: NodeId,
    }

    fn harness(widget: impl IntoElement) -> Harness {
        let focus = FocusRegistry::new();
        provide_context(focus.clone());
        let mut h = Harness {
            root: widget.into_element(),
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            focus,
            damage: StdArc::new(DamageState::default()),
            theme: Theme::light(),
            root_id: NodeId::from_raw(0),
        };
        h.root_id = frame(&mut h);
        h
    }

    fn frame(h: &mut Harness) -> NodeId {
        let mut scene = Scene::new();
        render_tree(
            &mut h.root,
            &mut h.arena,
            &mut h.layout,
            &mut h.text,
            None,
            Some(&mut h.hit),
            None,
            Some(&mut h.focus),
            None,
            None,
            &mut scene,
            VIEWPORT,
            &h.damage,
            &h.theme,
        )
        .unwrap()
    }

    /// The list's slot nodes: container → dyn_list → col → [slot, slot, …]. In a non-drag frame every slot is a
    /// row; during a drag one slot is the empty placeholder gap (#357). Returns the col's child NodeIds in order.
    fn rows(h: &Harness) -> Vec<NodeId> {
        let list = h.arena.get(h.root_id).unwrap().children[0];
        let col = h.arena.get(list).unwrap().children[0];
        h.arena.get(col).unwrap().children.clone()
    }

    /// Whether `node` is the placeholder gap: it has exactly one child — the thin insertion **line**, a leaf with
    /// no children of its own — whereas a content row's child (the `label` wrapper) has a text grandchild, and a
    /// handle row has two children (grip + content).
    fn is_gap(h: &Harness, node: NodeId) -> bool {
        let kids = &h.arena.get(node).unwrap().children;
        kids.len() == 1 && h.arena.get(kids[0]).unwrap().children.is_empty()
    }

    /// Render the current tree into a fresh scene and return it (for inspecting emitted quads).
    fn frame_scene(h: &mut Harness) -> Scene {
        let mut scene = Scene::new();
        render_tree(
            &mut h.root,
            &mut h.arena,
            &mut h.layout,
            &mut h.text,
            None,
            Some(&mut h.hit),
            None,
            Some(&mut h.focus),
            None,
            None,
            &mut scene,
            VIEWPORT,
            &h.damage,
            &h.theme,
        )
        .unwrap();
        scene
    }

    fn list_of(items: RwSignal<Vec<char>>) -> impl IntoElement {
        reorderable_list(items, |c: &char| label(c.to_string()))
    }

    #[test]
    fn reorderable_list_should_render_rows() {
        let owner = Owner::new();
        owner.set();
        let items = RwSignal::new(vec!['a', 'b', 'c']);
        let h = harness(list_of(items));
        assert_eq!(rows(&h).len(), 3, "three items render three rows");
        drop(owner);
    }

    #[test]
    fn reorderable_list_drag_should_reorder() {
        let owner = Owner::new();
        owner.set();
        let items = RwSignal::new(vec!['a', 'b', 'c']);
        let mut h = harness(list_of(items));
        let row_nodes = rows(&h);
        let (src, dst) = (row_nodes[0], row_nodes[2]);
        // Simulate a DnD (the headless Dock approach): start the drag from row 0, drop it on row 2 with the
        // pointer in that row's TOP half → insert before it. So 'a' lands between 'b' and 'c'.
        let r = h.layout.absolute_layout(dst);
        let ptr = Point::new(r.origin.x + r.size.w / 2.0, r.origin.y + 1.0);
        let (payload, _img) =
            dispatch_drag_start(&mut h.root, &h.arena, src).expect("row is a drag source");
        dispatch_drop(&mut h.root, &h.arena, dst, payload, ptr);
        assert_eq!(
            items.get_untracked(),
            vec!['b', 'a', 'c'],
            "dragging 'a' onto row 2 (top half) moves it before 'c'"
        );
        drop(owner);
    }

    #[test]
    fn reflow_order_should_match_move_item() {
        // The tentative visual order of indices relocates `from` exactly as `move_item` relocates an item.
        assert_eq!(reflow_order(4, 0, 2), vec![1, 0, 2, 3]);
        assert_eq!(reflow_order(3, 2, 0), vec![2, 0, 1]);
        assert_eq!(reflow_order(3, 0, 0), vec![0, 1, 2]); // no-op (before itself)
        assert_eq!(reflow_order(3, 0, 3), vec![1, 2, 0]); // to the end
    }

    /// Absolute top-half pointer of a slot (→ insert before it).
    fn top_half(h: &Harness, node: NodeId) -> Point {
        let r = h.layout.absolute_layout(node);
        Point::new(r.origin.x + r.size.w / 2.0, r.origin.y + 1.0)
    }

    #[test]
    fn drag_over_should_preview_without_committing_items() {
        let owner = Owner::new();
        owner.set();
        let items = RwSignal::new(vec!['a', 'b', 'c']);
        let mut h = harness(list_of(items));
        let slots = rows(&h);
        let (src, over) = (slots[0], slots[2]);
        let ptr = top_half(&h, over); // over row 2 top half → insert before index 2
        dispatch_drag_start(&mut h.root, &h.arena, src).expect("row is a drag source");
        let accepted =
            dispatch_drag_over(&mut h.root, &h.arena, over, TypeId::of::<RowDrag>(), ptr);
        assert!(accepted, "the row accepts a RowDrag");
        frame(&mut h);
        // Non-committal: `items` is untouched until release.
        assert_eq!(
            items.get_untracked(),
            vec!['a', 'b', 'c'],
            "drag-over previews only — it must not commit items"
        );
        // Reflowed order [1, 0, 2]: the empty gap (dragged slot) sits at visual index 1.
        let slots = rows(&h);
        assert_eq!(slots.len(), 3, "still three slots (two rows + the gap)");
        let gaps: Vec<usize> = slots
            .iter()
            .enumerate()
            .filter(|&(_, &n)| is_gap(&h, n))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            gaps,
            vec![1],
            "exactly one placeholder gap, at the reflow position"
        );
        drop(owner);
    }

    #[test]
    fn cancel_should_revert_preview() {
        let owner = Owner::new();
        owner.set();
        let items = RwSignal::new(vec!['a', 'b', 'c']);
        let mut h = harness(list_of(items));
        let slots = rows(&h);
        let (src, over) = (slots[0], slots[2]);
        let ptr = top_half(&h, over);
        dispatch_drag_start(&mut h.root, &h.arena, src).expect("row is a drag source");
        dispatch_drag_over(&mut h.root, &h.arena, over, TypeId::of::<RowDrag>(), ptr);
        // Cancel (Esc / leave): the shell delivers drag-leave to the over-target; nothing commits.
        dispatch_drag_leave(&mut h.root, &h.arena, over);
        frame(&mut h);
        assert_eq!(
            items.get_untracked(),
            vec!['a', 'b', 'c'],
            "cancel reverts — items unchanged"
        );
        let slots = rows(&h);
        assert_eq!(slots.len(), 3, "the original three rows render");
        assert!(
            slots.iter().all(|&n| !is_gap(&h, n)),
            "no placeholder gap after revert"
        );
        drop(owner);
    }

    #[test]
    fn drop_on_gap_should_commit_through_reflow() {
        // The full live lifecycle: drag_start → drag_over (preview → rebuild, NodeIds change) → drop on the
        // placeholder gap. Exercises the gap's own drop_target AND commit-after-rebuild — the shell re-hit-tests
        // at release (app.rs `end_drag`) to find this live gap node, since the cached `over` is now stale.
        let owner = Owner::new();
        owner.set();
        let items = RwSignal::new(vec!['a', 'b', 'c']);
        let mut h = harness(list_of(items));
        let slots = rows(&h);
        let (src, over) = (slots[0], slots[2]);
        let ptr = top_half(&h, over); // drag 'a' over row 2 top-half → gap before 'c'
        let (payload, _img) =
            dispatch_drag_start(&mut h.root, &h.arena, src).expect("src is a drag source");
        dispatch_drag_over(&mut h.root, &h.arena, over, TypeId::of::<RowDrag>(), ptr);
        frame(&mut h); // preview → full rebuild (row/gap NodeIds replaced)
        let gap = rows(&h)
            .into_iter()
            .find(|&n| is_gap(&h, n))
            .expect("a placeholder gap exists during preview");
        let gap_ptr = {
            let r = h.layout.absolute_layout(gap);
            Point::new(r.origin.x + 1.0, r.origin.y + 1.0)
        };
        let dropped = dispatch_drop(&mut h.root, &h.arena, gap, payload, gap_ptr);
        assert!(dropped, "the gap consumes the drop");
        assert_eq!(
            items.get_untracked(),
            vec!['b', 'a', 'c'],
            "drop on the gap commits the previewed order"
        );
        frame(&mut h);
        assert!(
            rows(&h).iter().all(|&n| !is_gap(&h, n)),
            "the preview clears after commit — no gap remains"
        );
        drop(owner);
    }

    #[test]
    fn ghost_opacity_should_set_ghost_alpha() {
        // `.ghost_opacity(o)` controls the lift ghost's translucency (user preference: translucent ↔ transparent).
        let owner = Owner::new();
        owner.set();
        let items = RwSignal::new(vec!['a', 'b', 'c']);
        let mut h =
            harness(reorderable_list(items, |c: &char| label(c.to_string())).ghost_opacity(0.3));
        let src = rows(&h)[0];
        let (_payload, img) =
            dispatch_drag_start(&mut h.root, &h.arena, src).expect("row is a drag source");
        let mut ghost = img.expect("the row attaches a drag image");

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let damage = StdArc::new(DamageState::default());
        render_tree(
            &mut ghost,
            &mut arena,
            &mut layout,
            &mut text,
            None,
            None,
            None,
            None,
            None,
            None,
            &mut scene,
            VIEWPORT,
            &damage,
            &h.theme,
        )
        .unwrap();

        let raised = h.theme.resolve_color(ColorRole::SurfaceRaised);
        assert_eq!(
            scene.quads[0].bg,
            Background::Solid(raised.fade(0.3)),
            "the ghost card fades to the configured 0.3 opacity, not the 0.6 default"
        );
        drop(owner);
    }

    #[test]
    fn insertion_line_should_use_configured_color() {
        // During a drag the gap carries a thin insertion line in the configured color (default Accent, here Border).
        let owner = Owner::new();
        owner.set();
        let items = RwSignal::new(vec!['a', 'b', 'c']);
        let mut h = harness(
            reorderable_list(items, |c: &char| label(c.to_string()))
                .insertion_line_color(ColorRole::Border),
        );
        let slots = rows(&h);
        let (src, over) = (slots[0], slots[2]);
        let ptr = top_half(&h, over);
        dispatch_drag_start(&mut h.root, &h.arena, src).expect("src is a drag source");
        dispatch_drag_over(&mut h.root, &h.arena, over, TypeId::of::<RowDrag>(), ptr);
        let scene = frame_scene(&mut h);
        let line = Background::Solid(h.theme.resolve_color(ColorRole::Border));
        assert!(
            scene.quads.iter().any(|q| q.bg == line),
            "the drop insertion line paints a quad in the configured color"
        );
        drop(owner);
    }

    #[test]
    fn cancel_after_reflow_should_revert_via_live_node() {
        // Cancel after the reflow rebuild: the shell re-hit-tests and delivers drag-leave to a LIVE post-rebuild
        // node (the cached `over` is stale). `preview_to` is a shared signal, so any live target clears it → revert.
        let owner = Owner::new();
        owner.set();
        let items = RwSignal::new(vec!['a', 'b', 'c']);
        let mut h = harness(list_of(items));
        let slots = rows(&h);
        let (src, over) = (slots[0], slots[2]);
        let ptr = top_half(&h, over);
        dispatch_drag_start(&mut h.root, &h.arena, src).expect("src is a drag source");
        dispatch_drag_over(&mut h.root, &h.arena, over, TypeId::of::<RowDrag>(), ptr);
        frame(&mut h); // rebuild
        let live = rows(&h)[0]; // a live node from the rebuilt tree
        dispatch_drag_leave(&mut h.root, &h.arena, live);
        frame(&mut h);
        assert_eq!(
            items.get_untracked(),
            vec!['a', 'b', 'c'],
            "cancel after reflow reverts — items unchanged"
        );
        assert!(
            rows(&h).iter().all(|&n| !is_gap(&h, n)),
            "no gap after revert"
        );
        drop(owner);
    }

    #[test]
    fn drag_over_upward_should_gap_at_reflow_index() {
        // Upward drag exercises the `from > to` branch of reflow_order (distinct from the downward case).
        let owner = Owner::new();
        owner.set();
        let items = RwSignal::new(vec!['a', 'b', 'c']);
        let mut h = harness(list_of(items));
        let slots = rows(&h);
        let (src, over) = (slots[2], slots[0]); // drag 'c' (from=2) over row 0 top-half → insert before 0
        let ptr = top_half(&h, over);
        dispatch_drag_start(&mut h.root, &h.arena, src).expect("src is a drag source");
        dispatch_drag_over(&mut h.root, &h.arena, over, TypeId::of::<RowDrag>(), ptr);
        frame(&mut h);
        // reflow_order(3, 2, 0) = [2, 0, 1] → the dragged slot (orig 2) is the gap at visual index 0.
        let slots = rows(&h);
        let gaps: Vec<usize> = slots
            .iter()
            .enumerate()
            .filter(|&(_, &n)| is_gap(&h, n))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(gaps, vec![0], "upward drag opens the gap at reflow index 0");
        assert_eq!(
            items.get_untracked(),
            vec!['a', 'b', 'c'],
            "still non-committal"
        );
        drop(owner);
    }

    #[test]
    fn second_drag_should_reorder_after_first_commit() {
        // Guards the `drag_from` reset hygiene (commit clears it): a second drag after a committed first must
        // reorder correctly and not be contaminated by the first drag's state.
        let owner = Owner::new();
        owner.set();
        let items = RwSignal::new(vec!['a', 'b', 'c']);
        let mut h = harness(list_of(items));
        // First drag: 'a' → before 'c' → [b, a, c].
        let slots = rows(&h);
        let (s1, d1) = (slots[0], slots[2]);
        let p1 = top_half(&h, d1);
        let (pl1, _) = dispatch_drag_start(&mut h.root, &h.arena, s1).expect("src1");
        dispatch_drop(&mut h.root, &h.arena, d1, pl1, p1);
        assert_eq!(items.get_untracked(), vec!['b', 'a', 'c']);
        frame(&mut h);
        // Second drag on the new order: move row 0 ('b') to before row 2 ('c') → [a, b, c].
        let slots = rows(&h);
        let (s2, d2) = (slots[0], slots[2]);
        let p2 = top_half(&h, d2);
        let (pl2, _) = dispatch_drag_start(&mut h.root, &h.arena, s2).expect("src2");
        dispatch_drop(&mut h.root, &h.arena, d2, pl2, p2);
        assert_eq!(
            items.get_untracked(),
            vec!['a', 'b', 'c'],
            "the second drag reorders correctly (no stale drag_from contamination)"
        );
        drop(owner);
    }

    #[test]
    fn handle_should_gate_drag_source() {
        let owner = Owner::new();
        owner.set();
        // handle(false) (default): the whole row is the drag source.
        let items = RwSignal::new(vec!['a', 'b', 'c']);
        let mut h = harness(reorderable_list(items, |c: &char| label(c.to_string())));
        let row = rows(&h)[0];
        assert!(
            dispatch_drag_start(&mut h.root, &h.arena, row).is_some(),
            "without a handle the row itself is the drag source"
        );
        drop(owner);

        // handle(true): only the grip child is the drag source; the row body is not.
        let owner = Owner::new();
        owner.set();
        let items = RwSignal::new(vec!['a', 'b', 'c']);
        let mut h = harness(reorderable_list(items, |c: &char| label(c.to_string())).handle(true));
        let row = rows(&h)[0];
        let grip = h.arena.get(row).unwrap().children[0]; // [grip, content]
        assert!(
            dispatch_drag_start(&mut h.root, &h.arena, row).is_none(),
            "with a handle the row body is not a drag source"
        );
        assert!(
            dispatch_drag_start(&mut h.root, &h.arena, grip).is_some(),
            "the grip child is the drag source"
        );
        drop(owner);
    }

    #[test]
    fn drag_should_build_translucent_ghost() {
        // The drag ghost element (#356) is the second field of `dispatch_drag_start`. Paint it standalone and
        // confirm its card is a `SurfaceRaised` fill faded to 0.6 opacity — pinning the `.drag_image` +
        // `.opacity(0.6)` wiring (the live cursor-follow is shell-only, left to the example).
        let owner = Owner::new();
        owner.set();
        let items = RwSignal::new(vec!['a', 'b', 'c']);
        let mut h = harness(list_of(items));
        let src = rows(&h)[0];
        let (_payload, img) =
            dispatch_drag_start(&mut h.root, &h.arena, src).expect("row is a drag source");
        let mut ghost = img.expect("the row attaches a drag image");

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let mut scene = Scene::new();
        let damage = StdArc::new(DamageState::default());
        render_tree(
            &mut ghost,
            &mut arena,
            &mut layout,
            &mut text,
            None,
            None,
            None,
            None,
            None,
            None,
            &mut scene,
            VIEWPORT,
            &damage,
            &h.theme,
        )
        .unwrap();

        let raised = h.theme.resolve_color(ColorRole::SurfaceRaised);
        assert!(!scene.quads.is_empty(), "the ghost paints a card quad");
        assert_eq!(
            scene.quads[0].bg,
            Background::Solid(raised.fade(0.6)),
            "the ghost card is a SurfaceRaised fill faded to 0.6"
        );
        drop(owner);
    }

    #[test]
    fn reorderable_list_alt_arrow_should_move_item() {
        let owner = Owner::new();
        owner.set();
        let items = RwSignal::new(vec!['a', 'b', 'c']);
        let mut h = harness(list_of(items));
        let mut alt = Modifiers::default();
        alt.alt = true;
        // A plain ArrowDown sets the active row to 0; Alt+ArrowDown then moves item 0 down one → [b, a, c].
        press(&mut h, KeyCode::ArrowDown, Modifiers::default());
        press(&mut h, KeyCode::ArrowDown, alt);
        assert_eq!(
            items.get_untracked(),
            vec!['b', 'a', 'c'],
            "Alt+ArrowDown moves the active item down one"
        );
        drop(owner);
    }

    /// Dispatches a key to the list container (the roving-focus target = the root).
    fn press(h: &mut Harness, code: KeyCode, mods: Modifiers) {
        let kev = KeyEvent::new(code, mods, true, false);
        dispatch_key(&mut h.root, &h.arena, Some(h.root_id), &kev);
    }
}
