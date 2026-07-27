//! The [`ReorderableList`] widget (#226): a list whose rows are reordered by **dragging** a row or by the
//! **keyboard** (Alt+Up/Down), bound to a controlled `RwSignal<Vec<T>>` — layer/track order, playlists,
//! list-based inspectors.
//!
//! Reuses the DnD substrate (#178, as [`Dock`](crate::Dock) does): each row is a `drag_source` carrying its
//! index and a `drop_target` that moves the dragged item on drop; a thin **insertion line** marks where the
//! drop lands (from the pointer's half of the hovered row). The rows are re-rendered on any `items` change via
//! a **single-item rebuild** `dyn_list` (the `tree`/`dropdown` idiom). Keyboard reorder uses a **roving-focus**
//! model — the list is one focus target with an `active` row index (so a full rebuild can't lose per-row focus
//! handles); Arrow keys move the highlight, Alt+Arrow moves the active item (and the highlight follows it).

use std::rc::Rc;

use kagari_core::element::{Div, dyn_list};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{AnyElement, BoundsHandle, IntoElement, KeyCode, Role, div, use_focus_handle};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, apply_size};

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

/// A thin full-width line at insertion index `p`: `Accent` while a drag would drop there (`drop_hint == p`),
/// otherwise the container background (invisible). It also gives rows a small consistent separation.
fn insertion_line(drop_hint: RwSignal<Option<usize>>, p: usize) -> Div {
    div().h_0p5().bg(rx(move || {
        if drop_hint.get() == Some(p) {
            ColorRole::Accent
        } else {
            ColorRole::Bg
        }
    }))
}

/// One row: the item content, highlighted while it is the `active` (keyboard) row, draggable as a whole, and a
/// drop target that moves the dragged item to this row's position (top half → before it, bottom half → after).
fn build_row<T: Clone + PartialEq + Send + Sync + 'static>(
    i: usize,
    content: AnyElement,
    items: RwSignal<Vec<T>>,
    active: RwSignal<Option<usize>>,
    drop_hint: RwSignal<Option<usize>>,
    size: ControlSize,
) -> Div {
    let bounds = BoundsHandle::new();
    let (b_drop, b_over) = (bounds.clone(), bounds.clone());
    apply_size(div().flex().items_center(), size)
        .role(Role::ListItem)
        .track_bounds(&bounds)
        .bg(rx(move || {
            if active.get() == Some(i) {
                ColorRole::SurfaceRaised
            } else {
                ColorRole::Bg
            }
        }))
        .child(content)
        .drag_source(move || RowDrag(i))
        .drop_target::<RowDrag>(move |RowDrag(from), cx| {
            // The pointer's half of the row picks before (i) or after (i+1).
            let to = match cx.drag_pointer() {
                Some(p) => {
                    let b = b_drop.get();
                    if p.y < b.origin.y + b.size.h / 2.0 {
                        i
                    } else {
                        i + 1
                    }
                }
                None => i,
            };
            items.update(|v| move_item(v, from, to));
            // Keep the highlight on the item that just moved (its final index).
            let final_idx = if from < to { to.saturating_sub(1) } else { to };
            let len = items.with_untracked(|v| v.len());
            active.set(Some(final_idx.min(len.saturating_sub(1))));
            drop_hint.set(None);
        })
        .on_drag_over(move |cx| {
            let hint = match cx.drag_pointer() {
                Some(p) => {
                    let b = b_over.get();
                    if p.y < b.origin.y + b.size.h / 2.0 {
                        i
                    } else {
                        i + 1
                    }
                }
                None => i,
            };
            drop_hint.set(Some(hint));
        })
        .on_drag_leave(move |_cx| drop_hint.set(None))
}

/// A reorderable list (#226): rows bound to a controlled `RwSignal<Vec<T>>`, reordered by dragging a row or by
/// the keyboard (Alt+Up/Down while the list is focused). Build with [`reorderable_list`]; chain
/// [`.size`](Self::size). Returns `impl IntoElement`; `Styled` is not on its surface.
pub struct ReorderableList<T> {
    items: RwSignal<Vec<T>>,
    item_fn: Rc<dyn Fn(&T) -> AnyElement>,
    size: ControlSize,
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
    }
}

impl<T: Clone + PartialEq + Send + Sync + 'static> ReorderableList<T> {
    /// Sets the control size (row padding + gap).
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }
}

impl<T: Clone + PartialEq + Send + Sync + 'static> IntoElement for ReorderableList<T> {
    fn into_element(self) -> AnyElement {
        let ReorderableList {
            items,
            item_fn,
            size,
        } = self;
        // Signals created once, so they survive the row rebuilds.
        let active = RwSignal::new(None::<usize>);
        let drop_hint = RwSignal::new(None::<usize>);
        let focus = use_focus_handle();

        // Rows, rebuilt whenever `items` changes (single-item rebuild — the tree/dropdown idiom).
        let list = dyn_list(
            move || vec![items.get()],
            |v: &Vec<T>| v.clone(),
            move |v: &Vec<T>| {
                let item_fn = item_fn.clone();
                let mut col = div().flex_col();
                for (i, item) in v.iter().enumerate() {
                    col = col.child(insertion_line(drop_hint, i)).child(build_row(
                        i,
                        item_fn(item),
                        items,
                        active,
                        drop_hint,
                        size,
                    ));
                }
                col.child(insertion_line(drop_hint, v.len()))
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
                let len = items.with_untracked(|v| v.len());
                if len == 0 {
                    return;
                }
                let cur = active.get_untracked();
                match kev.code {
                    KeyCode::ArrowUp if kev.modifiers.alt => {
                        if let Some(i) = cur {
                            if i > 0 {
                                items.update(|v| move_item(v, i, i - 1));
                                active.set(Some(i - 1));
                            }
                        }
                        cx.stop_propagation();
                    }
                    KeyCode::ArrowDown if kev.modifiers.alt => {
                        if let Some(i) = cur {
                            if i + 1 < len {
                                items.update(|v| move_item(v, i, i + 2));
                                active.set(Some(i + 1));
                            }
                        }
                        cx.stop_propagation();
                    }
                    KeyCode::ArrowUp => {
                        active.set(Some(cur.map_or(len - 1, |i| i.saturating_sub(1))));
                        cx.stop_propagation();
                    }
                    KeyCode::ArrowDown => {
                        active.set(Some(cur.map_or(0, |i| (i + 1).min(len - 1))));
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            })
            .child(list)
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    use kagari_base::{NodeId, Point, Size};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::{
        FocusRegistry, HitTest, KeyEvent, Modifiers, dispatch_drag_start, dispatch_drop,
        dispatch_key,
    };
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
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

    /// The list's row nodes: container → dyn_list → col → [line, row, line, row, …]. Returns the row NodeIds.
    fn rows(h: &Harness) -> Vec<NodeId> {
        // container.children = [dyn_list]; dyn_list.children = [col]; col.children = [line0, row0, line1, row1, …].
        let list = h.arena.get(h.root_id).unwrap().children[0];
        let col = h.arena.get(list).unwrap().children[0];
        let kids = &h.arena.get(col).unwrap().children;
        // Rows are the odd-indexed children (lines are even); drop the trailing line.
        kids.iter().skip(1).step_by(2).copied().collect()
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
