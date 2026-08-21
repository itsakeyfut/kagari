//! The [`GridView`] widget (#225, MVP): a uniform-tile grid (asset browser / media pool / thumbnail gallery)
//! that wraps a **fixed** set of same-size items into rows and **virtualizes** the body, with single-select on
//! click and 2D arrow-key navigation.
//!
//! It is the third virtualized selectable widget after [`Table`](crate::Table) (#224) and [`Tree`](crate::Tree)
//! (#223), and the simplest: because the item count is fixed (no expand), it builds the core
//! [`virtualized_list`](kagari_core::virtualized_list) element **directly** with a fixed row count (like Table)
//! — no `dyn_list`, no `Send + Sync` items_fn, no two-plane arena. The column count is derived from the viewport
//! width (`cols = ⌊(vp.w + gap) / (item.w + gap)⌋`), items are laid out `cols` per flex row (with a native
//! [`Div::gap`](kagari_core::element::Div::gap) between cells), and the vertical gap is folded into the row
//! height. The widget owns the body [`ScrollHandle`] (wheel + keyboard scroll-to-selected).
//!
//! MVP scope: render + single-select + 2D keyboard nav + focus + row activation. Uniform item size is an
//! invariant (the virtualized body requires it). Activation (#368, mirroring [`Table`](crate::Table)):
//! [`.on_activate`](GridView::on_activate) fires with the item index on Enter/Space/NumpadEnter over the
//! keyboard-selected cell (one-shot per RK-026). **Follow-ups**: multi-select (Ctrl/Shift), resize-reactive
//! column count (the viewport width is fixed at build), `Role::Grid`/`GridCell` a11y.

use std::rc::Rc;

use kagari_base::{Point, Px, Size};
use kagari_core::element::Div;
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{
    AnyElement, IntoElement, KeyCode, MouseKind, ScrollHandle, div, use_focus_handle,
    virtualized_list as core_virtualized_list,
};
use kagari_style::{ColorRole, Styled};

/// The GridView's item selection (#225). Single-select today (an item index or nothing); a future multi-select
/// extends this type non-breakingly, so callers bind `RwSignal<GridSelection>` rather than a bare
/// `Option<usize>`. Grid-owned (not shared with the [`Table`](crate::Table)'s `Selection`): a grid's future
/// multi-select is a 2D rectangle, which diverges from the table's 1D range, so the types evolve independently.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct GridSelection(Option<usize>);

impl GridSelection {
    /// Nothing selected.
    pub fn none() -> Self {
        GridSelection(None)
    }

    /// Selects the single item at index `i` (into the `count` passed to [`grid_view`]).
    pub fn single(i: usize) -> Self {
        GridSelection(Some(i))
    }

    /// The selected item index, if any.
    pub fn selected(&self) -> Option<usize> {
        self.0
    }

    /// Whether item `i` is selected (the internal predicate a multi-select variant generalizes).
    fn is_selected(&self, i: usize) -> bool {
        self.0 == Some(i)
    }
}

/// A virtualized uniform-tile grid (#225). Build with [`grid_view`]; set the viewport with
/// [`.size(..)`](Self::size), the inter-tile spacing with [`.gap(..)`](Self::gap), and wire selection with
/// [`.selection(..)`](Self::selection). Only the rows in view (± a buffer) are ever built. Returns
/// `impl IntoElement`; `Styled` is **not** on its surface.
pub struct GridView {
    count: usize,
    item_size: Size,
    item_fn: Rc<dyn Fn(usize) -> AnyElement>,
    gap: f32,
    viewport: Size,
    selection: Option<RwSignal<GridSelection>>,
    on_activate: Option<Rc<dyn Fn(usize)>>,
}

/// Creates a grid over `count` items, each `item_size` (logical px), building item `index` with
/// `item(index)`. Set the scroll viewport with [`.size(..)`](GridView::size) — it defaults to a small
/// placeholder, so a real caller sizes it to its slot. The viewport **width** determines the column count.
pub fn grid_view(
    count: usize,
    item_size: Size,
    item: impl Fn(usize) -> AnyElement + 'static,
) -> GridView {
    GridView {
        count,
        item_size,
        item_fn: Rc::new(item),
        gap: 0.0,
        viewport: Size { w: 240.0, h: 320.0 },
        selection: None,
        on_activate: None,
    }
}

impl GridView {
    /// Sets the inter-tile gap (logical px), applied horizontally between cells and vertically between rows.
    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    /// Sets the scroll viewport (the clipped window the grid scrolls within). Mirrors
    /// [`VirtualizedList::size`](crate::VirtualizedList::size); the width drives the column count and the height
    /// the scroll extent.
    pub fn size(mut self, viewport: Size) -> Self {
        self.viewport = viewport;
        self
    }

    /// Binds the controlled selection. A clicked cell sets it to `GridSelection::single(index)` and the selected
    /// cell is tinted; wiring selection also enables focus + 2D arrow-key navigation. When unset, items render
    /// but are not selectable.
    pub fn selection(mut self, selection: RwSignal<GridSelection>) -> Self {
        self.selection = Some(selection);
        self
    }

    /// Sets the cell **activation** callback (#368): invoked with the item index when the user activates the
    /// keyboard-selected cell via Enter/Space/NumpadEnter. Distinct from [`.selection`](Self::selection), which
    /// only moves the cursor. Optional — without it, Enter/Space merely settle the selection. One-shot: a
    /// long-press does not re-fire it (RK-026). Mirrors [`Table::on_activate`](crate::Table::on_activate).
    /// Activation needs a wired [`selection`](Self::selection) cursor and a selected cell; pressing Enter with
    /// nothing selected selects item 0 instead (a second press then activates it).
    pub fn on_activate(mut self, f: impl Fn(usize) + 'static) -> Self {
        self.on_activate = Some(Rc::new(f));
        self
    }
}

/// Builds one grid row `row`: a flex row (with a native `gap`) of the cells `[row*cols .. min(count, ..)]`.
/// A partial last row builds only its real items. Each cell is `item_size`, tinted `Accent` when selected, and
/// carries the caller's item content; the `.bg` / `.on_click` are wired only when a selection signal is present.
fn build_grid_row(
    row: usize,
    cols: usize,
    count: usize,
    item_size: Size,
    gap: f32,
    item_fn: &Rc<dyn Fn(usize) -> AnyElement>,
    selection: Option<RwSignal<GridSelection>>,
) -> Div {
    let start = row * cols;
    let end = (start + cols).min(count);
    let mut r = div().flex().gap(Px(gap));
    for i in start..end {
        let mut cell = div().size(item_size);
        match selection {
            Some(sel) => {
                cell = cell
                    .bg(rx(move || {
                        if sel.get().is_selected(i) {
                            ColorRole::Accent
                        } else {
                            ColorRole::Surface
                        }
                    }))
                    .on_click(move |_, _| sel.set(GridSelection::single(i)));
            }
            None => cell = cell.bg(ColorRole::Surface),
        }
        r = r.child(cell.child((item_fn)(i)));
    }
    r
}

/// Keeps the row at `pos` within the viewport by nudging the scroll offset (mirrors the Tree's helper).
fn scroll_into_view(handle: &ScrollHandle, pos: usize, row_h: f32, body_height: f32) {
    let y = pos as f32 * row_h;
    let cur = handle.offset().y;
    let next = if y < cur {
        y
    } else if y + row_h > cur + body_height {
        y + row_h - body_height
    } else {
        cur
    };
    handle.jump_to(Point::new(0.0, next));
}

/// Handles a key press: 2D grid navigation over the item indices. Right/Left move ±1 within a row (no wrap
/// past the row edges); Down/Up move ±`cols` (staying put when there is no cell directly below/above). All
/// moves are guarded (no `usize` underflow). The first nav press with no selection selects item 0. Scrolls to
/// keep the selected item's row visible.
#[allow(clippy::too_many_arguments)]
fn keyboard(
    code: KeyCode,
    repeat: bool,
    count: usize,
    cols: usize,
    selection: RwSignal<GridSelection>,
    handle: &ScrollHandle,
    row_h: f32,
    body_height: f32,
    on_activate: Option<&dyn Fn(usize)>,
) {
    if count == 0 {
        return;
    }
    let cur = selection.get_untracked().selected();
    // Activation (#368): Enter/Space/NumpadEnter on the selected cell fires `on_activate` with the item index —
    // one-shot (RK-026: guard `repeat`; navigation, below, repeats). With no selection yet, fall through to select
    // the first item (a second press then activates it).
    if matches!(code, KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Space) {
        if let Some(i) = cur {
            if !repeat {
                if let Some(f) = on_activate {
                    f(i);
                }
            }
            return;
        }
    }
    let next = match cur {
        Some(i) => match code {
            KeyCode::ArrowRight => {
                if i % cols != cols - 1 && i + 1 < count {
                    i + 1
                } else {
                    i
                }
            }
            KeyCode::ArrowLeft => {
                if i % cols != 0 {
                    i - 1
                } else {
                    i
                }
            }
            KeyCode::ArrowDown => {
                if i + cols < count {
                    i + cols
                } else {
                    i
                }
            }
            KeyCode::ArrowUp => {
                if i >= cols {
                    i - cols
                } else {
                    i
                }
            }
            _ => return,
        },
        None => match code {
            KeyCode::ArrowRight
            | KeyCode::ArrowLeft
            | KeyCode::ArrowDown
            | KeyCode::ArrowUp
            | KeyCode::Enter
            | KeyCode::NumpadEnter
            | KeyCode::Space => 0,
            _ => return,
        },
    };
    // No-op move (an arrow held at an edge, or Enter with a selection): skip the redundant signal write + scroll —
    // matters now that navigation keys auto-repeat.
    if cur == Some(next) {
        return;
    }
    selection.set(GridSelection::single(next));
    scroll_into_view(handle, next / cols, row_h, body_height);
}

impl IntoElement for GridView {
    fn into_element(self) -> AnyElement {
        let GridView {
            count,
            item_size,
            item_fn,
            gap,
            viewport,
            selection,
            on_activate,
        } = self;

        // Column count from the viewport width. Guard the denominator: a zero-width item (or w+gap == 0) would
        // divide by zero → `inf as usize` saturates to `usize::MAX`, collapsing every item into one mega-row and
        // defeating virtualization; fall back to a single column instead.
        let cols = if item_size.w + gap <= 0.0 {
            1
        } else {
            ((viewport.w + gap) / (item_size.w + gap)).floor().max(1.0) as usize
        };
        let row_count = count.div_ceil(cols); // count==0 → 0 rows; count%cols==0 → no phantom trailing row
        let row_h = item_size.h + gap; // vertical gap folded into the row slot

        // Virtualized body: build the CORE element so the widget owns the scroll handle (wheel + keyboard
        // scroll-to-selected); re-add the wheel handler (the core element has no `handle_event` of its own).
        let handle = ScrollHandle::new();
        let max_y = (row_count as f32 * row_h - viewport.h).max(0.0);
        let core = core_virtualized_list(row_h, row_count, viewport, &handle, move |row| {
            build_grid_row(row, cols, count, item_size, gap, &item_fn, selection)
        });
        let wheel = handle.clone();
        let body = div()
            .size(viewport)
            .on_wheel(move |ev, _| {
                if let MouseKind::Wheel { dy, .. } = ev.kind {
                    // `dy` is already a logical-pixel scroll distance (the app scales a notch by `WHEEL_LINE_PX`),
                    // so apply it directly — multiplying by `row_h` again over-scrolls by a row per pixel.
                    let next = (wheel.offset().y - dy).clamp(0.0, max_y);
                    wheel.jump_to(Point::new(0.0, next));
                }
            })
            .child(core);

        // Focus + 2D keyboard navigation only when a selection signal is wired (selection is the nav cursor).
        match selection {
            Some(sel) => {
                let focus = use_focus_handle();
                let ring = focus.clone();
                let click_focus = focus.clone();
                let kb_handle = handle.clone();
                let body_h = viewport.h;
                let activate = on_activate.clone();
                div()
                    .rounded_md()
                    .border_w_1()
                    .border_color(rx(move || {
                        ring.is_focus_visible().then_some(ColorRole::FocusRing)
                    }))
                    .track_focus(&focus)
                    // Acquire focus on press so the arrow keys route here — `track_focus` only marks the node
                    // focusable; nothing focuses it on click automatically (RK-051).
                    .on_mouse_down(move |_, _| click_focus.focus())
                    // Navigation keys auto-repeat (hold to keep moving); unlike one-shot activation (RK-026).
                    .on_key_down(move |kev, _| {
                        keyboard(
                            kev.code,
                            kev.repeat,
                            count,
                            cols,
                            sel,
                            &kb_handle,
                            row_h,
                            body_h,
                            activate.as_deref(),
                        );
                    })
                    .child(body)
                    .into_element()
            }
            None => div().child(body).into_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Arc as StdArc;

    use kagari_base::{NodeId, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::FocusRegistry;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{
        DispatchState, HitTest, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseKind,
        dispatch_key, dispatch_mouse, text,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 400.0, h: 400.0 };
    const ITEM: BSize = BSize { w: 100.0, h: 40.0 };
    const ROW_H: f32 = 40.0; // ITEM.h + gap(0)
    const COLS: usize = 3; // ⌊320 / 100⌋

    /// A grid of `n` items (100×40), 3 columns (viewport 320 wide), all rows visible (viewport 400 tall so the
    /// body does not window), optionally wired to `sel`.
    fn sample_grid(n: usize, sel: Option<RwSignal<GridSelection>>) -> AnyElement {
        let mut g = grid_view(n, ITEM, |i| text(format!("#{i}")).into_element())
            .size(BSize { w: 320.0, h: 400.0 });
        if let Some(s) = sel {
            g = g.selection(s);
        }
        g.into_element()
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

    fn harness(build: impl FnOnce() -> AnyElement) -> Harness {
        let focus = FocusRegistry::new();
        provide_context(focus.clone());
        let mut h = Harness {
            root: build(),
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            focus,
            damage: StdArc::new(DamageState::default()),
            theme: Theme::light(),
            root_id: NodeId::from_raw(0),
        };
        h.root_id = frame(&mut h).0;
        h
    }

    fn frame(h: &mut Harness) -> (NodeId, Scene) {
        let mut scene = Scene::new();
        let id = render_tree(
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
        (id, scene)
    }

    /// The core virtualized body node: root → child[0] body div → child[0] core (same path with/without
    /// selection, since the focus container's only child is the body div).
    fn core_node(h: &Harness) -> NodeId {
        let body = h.arena.get(h.root_id).unwrap().children[0];
        h.arena.get(body).unwrap().children[0]
    }

    /// The realized grid-row slot nodes (children of the core node).
    fn body_rows(h: &Harness) -> Vec<NodeId> {
        h.arena.get(core_node(h)).unwrap().children.clone()
    }

    /// The cell nodes of realized row `win_i` (slot → child[0] flex row → its children = the cells).
    fn row_cells(h: &Harness, win_i: usize) -> Vec<NodeId> {
        let flex = h.arena.get(body_rows(h)[win_i]).unwrap().children[0];
        h.arena.get(flex).unwrap().children.clone()
    }

    /// Clicks item index `i` (row `i/COLS`, col `i%COLS`) at offset 0.
    fn click_item(h: &mut Harness, i: usize) {
        let c = h.layout.absolute_layout(core_node(h));
        let x = c.origin.x + (i % COLS) as f32 * ITEM.w + ITEM.w / 2.0;
        let y = c.origin.y + (i / COLS) as f32 * ROW_H + ROW_H / 2.0;
        let p = Point::new(x, y);
        let mut dispatch = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(kind, p, Modifiers::default());
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut dispatch);
        }
    }

    fn press(h: &mut Harness, code: KeyCode) {
        let kev = KeyEvent::new(code, Modifiers::default(), true, false);
        dispatch_key(&mut h.root, &h.arena, Some(h.root_id), &kev);
    }

    #[test]
    fn grid_view_should_render_rows_from_count_and_cols() {
        let owner = Owner::new();
        owner.set();
        // 10 items, 3 cols → 4 rows (div_ceil). Viewport is tall enough to realize all rows.
        let h = harness(|| sample_grid(10, None));
        assert_eq!(body_rows(&h).len(), 4, "10 items / 3 cols → 4 rows");
        assert_eq!(row_cells(&h, 0).len(), 3, "a full row builds 3 cells");
        drop(owner);
    }

    #[test]
    fn grid_view_should_window_large_count() {
        let owner = Owner::new();
        owner.set();
        // 300 items / 3 cols = 100 rows, but a 400px viewport (~10 rows) realizes only a window — GridView
        // passes the derived row_count to the core, which builds only the visible rows (± a buffer).
        let h = harness(|| {
            grid_view(300, ITEM, |i| text(format!("#{i}")).into_element())
                .size(BSize { w: 320.0, h: 400.0 })
                .into_element()
        });
        assert!(
            body_rows(&h).len() < 100,
            "only the visible row window is realized, not all 100 rows"
        );
        drop(owner);
    }

    #[test]
    fn grid_view_partial_last_row_should_build_only_real_items() {
        let owner = Owner::new();
        owner.set();
        // 10 items, 3 cols → last row (row 3) holds only item 9.
        let h = harness(|| sample_grid(10, None));
        assert_eq!(
            row_cells(&h, 3).len(),
            1,
            "the partial last row builds only its 1 real item, no phantom cells"
        );
        drop(owner);
    }

    #[test]
    fn grid_view_click_should_select() {
        let owner = Owner::new();
        owner.set();
        let selection = RwSignal::new(GridSelection::none());
        let mut h = harness(|| sample_grid(10, Some(selection)));

        click_item(&mut h, 4); // row 1, col 1
        assert_eq!(
            selection.get_untracked().selected(),
            Some(4),
            "clicking a cell selects its item index"
        );
        click_item(&mut h, 0);
        assert_eq!(
            selection.get_untracked().selected(),
            Some(0),
            "clicking another cell moves the selection"
        );
        drop(owner);
    }

    #[test]
    fn grid_view_selected_should_highlight() {
        let owner = Owner::new();
        owner.set();
        let selection = RwSignal::new(GridSelection::single(2));
        let mut h = harness(|| sample_grid(10, Some(selection)));

        let accent = h.theme.resolve_color(ColorRole::Accent);
        let (_, scene) = frame(&mut h);
        assert!(
            scene
                .quads
                .iter()
                .any(|q| q.bg == kagari_render::Background::Solid(accent)),
            "the selected cell paints an Accent background"
        );
        drop(owner);
    }

    #[test]
    fn grid_view_arrow_right_down_should_navigate_2d() {
        let owner = Owner::new();
        owner.set();
        let selection = RwSignal::new(GridSelection::none());
        let mut h = harness(|| sample_grid(10, Some(selection)));

        press(&mut h, KeyCode::ArrowRight); // no cursor → item 0
        assert_eq!(selection.get_untracked().selected(), Some(0));
        press(&mut h, KeyCode::ArrowRight); // 0 → 1 (within row)
        assert_eq!(selection.get_untracked().selected(), Some(1));
        press(&mut h, KeyCode::ArrowDown); // 1 → 4 (+cols)
        assert_eq!(selection.get_untracked().selected(), Some(4));
        press(&mut h, KeyCode::ArrowUp); // 4 → 1 (-cols)
        assert_eq!(selection.get_untracked().selected(), Some(1));
        press(&mut h, KeyCode::ArrowLeft); // 1 → 0 (within row)
        assert_eq!(selection.get_untracked().selected(), Some(0));
        drop(owner);
    }

    #[test]
    fn grid_view_arrow_up_left_should_not_underflow() {
        let owner = Owner::new();
        owner.set();
        let selection = RwSignal::new(GridSelection::single(0)); // top-left corner
        let mut h = harness(|| sample_grid(10, Some(selection)));

        press(&mut h, KeyCode::ArrowUp); // no row above → stay (no usize underflow / panic)
        assert_eq!(selection.get_untracked().selected(), Some(0));
        press(&mut h, KeyCode::ArrowLeft); // col 0, no wrap → stay
        assert_eq!(selection.get_untracked().selected(), Some(0));
        // Right at the row edge does not wrap to the next row.
        let sel2 = RwSignal::new(GridSelection::single(2)); // row 0, col 2 (last col)
        let mut h2 = harness(|| sample_grid(10, Some(sel2)));
        press(&mut h2, KeyCode::ArrowRight);
        assert_eq!(
            sel2.get_untracked().selected(),
            Some(2),
            "Right at the last column stays put (no line-wrap)"
        );
        drop(owner);
    }

    #[test]
    fn grid_view_zero_count_should_render_nothing() {
        let owner = Owner::new();
        owner.set();
        let mut h = harness(|| sample_grid(0, None));
        assert_eq!(body_rows(&h).len(), 0, "a zero-item grid realizes no rows");
        frame(&mut h); // a second frame must not panic on the empty range
        drop(owner);
    }

    #[test]
    fn grid_view_zero_item_width_should_not_collapse_cols() {
        let owner = Owner::new();
        owner.set();
        // A zero-width item must fall back to cols=1 (not usize::MAX → one mega-row of every item).
        let h = harness(|| {
            grid_view(6, BSize { w: 0.0, h: 40.0 }, |i| {
                text(format!("#{i}")).into_element()
            })
            .size(BSize { w: 320.0, h: 400.0 })
            .into_element()
        });
        assert_eq!(
            row_cells(&h, 0).len(),
            1,
            "cols=1 fallback: the first row has 1 cell, not all 6 (no usize::MAX collapse)"
        );
        drop(owner);
    }

    #[test]
    fn grid_view_click_should_focus_and_route_arrow_keys() {
        let owner = Owner::new();
        owner.set();
        let selection = RwSignal::new(GridSelection::none());
        let mut h = harness(|| sample_grid(10, Some(selection)));

        assert_eq!(
            h.focus.focused_node(),
            None,
            "the grid is not focused before a click"
        );
        // Clicking an item must select it AND focus the grid, so keys route here (RK-051).
        click_item(&mut h, 0);
        assert_eq!(selection.get_untracked().selected(), Some(0));
        assert_eq!(
            h.focus.focused_node(),
            Some(h.root_id),
            "clicking an item focuses the grid"
        );
        // An arrow routed via the focused node (the real App path) moves the selection.
        let kev = KeyEvent::new(KeyCode::ArrowRight, Modifiers::default(), true, false);
        dispatch_key(&mut h.root, &h.arena, h.focus.focused_node(), &kev);
        assert_eq!(
            selection.get_untracked().selected(),
            Some(1),
            "an arrow key routed through focus moves the selection"
        );
        drop(owner);
    }

    #[test]
    fn grid_view_autorepeat_should_move_selection() {
        let owner = Owner::new();
        owner.set();
        let selection = RwSignal::new(GridSelection::single(0));
        let mut h = harness(|| sample_grid(10, Some(selection)));

        // An auto-repeat ArrowRight (`repeat = true`, a long-press tick) must still advance the selection.
        let kev = KeyEvent::new(KeyCode::ArrowRight, Modifiers::default(), true, true);
        dispatch_key(&mut h.root, &h.arena, Some(h.root_id), &kev);
        assert_eq!(
            selection.get_untracked().selected(),
            Some(1),
            "auto-repeat (long-press) keeps moving the selection"
        );
        drop(owner);
    }

    /// A 10-item grid wired to `sel` with an `.on_activate` that records the fired item index into `fired`.
    fn activatable_grid(
        n: usize,
        sel: RwSignal<GridSelection>,
        fired: Rc<Cell<Option<usize>>>,
    ) -> AnyElement {
        grid_view(n, ITEM, |i| text(format!("#{i}")).into_element())
            .size(BSize { w: 320.0, h: 400.0 })
            .selection(sel)
            .on_activate(move |i| fired.set(Some(i)))
            .into_element()
    }

    #[test]
    fn grid_view_activate_enter_should_fire_on_activate_with_selected_index() {
        let owner = Owner::new();
        owner.set();
        let selection = RwSignal::new(GridSelection::single(4));
        let fired = Rc::new(Cell::new(None));
        let mut h = harness({
            let fired = fired.clone();
            move || activatable_grid(10, selection, fired)
        });

        press(&mut h, KeyCode::Enter);
        assert_eq!(
            fired.get(),
            Some(4),
            "Enter fires on_activate with the selected item index"
        );
        assert_eq!(
            selection.get_untracked().selected(),
            Some(4),
            "activation leaves the selection in place"
        );
        drop(owner);
    }

    #[test]
    fn grid_view_activate_space_should_fire_on_activate() {
        let owner = Owner::new();
        owner.set();
        let selection = RwSignal::new(GridSelection::single(3));
        let fired = Rc::new(Cell::new(None));
        let mut h = harness({
            let fired = fired.clone();
            move || activatable_grid(10, selection, fired)
        });

        press(&mut h, KeyCode::Space);
        assert_eq!(
            fired.get(),
            Some(3),
            "Space also fires on_activate with the selected item index"
        );
        drop(owner);
    }

    #[test]
    fn grid_view_activate_autorepeat_should_not_refire() {
        let owner = Owner::new();
        owner.set();
        let selection = RwSignal::new(GridSelection::single(1));
        let fired = Rc::new(Cell::new(None));
        let mut h = harness({
            let fired = fired.clone();
            move || activatable_grid(10, selection, fired)
        });

        // An auto-repeat Enter (`repeat = true`, a long-press tick) must NOT re-fire activation (one-shot, RK-026).
        let kev = KeyEvent::new(KeyCode::Enter, Modifiers::default(), true, true);
        dispatch_key(&mut h.root, &h.arena, Some(h.root_id), &kev);
        assert_eq!(
            fired.get(),
            None,
            "auto-repeat Enter does not fire activation (one-shot, RK-026)"
        );
        // A genuine (non-repeat) Enter does fire.
        press(&mut h, KeyCode::Enter);
        assert_eq!(fired.get(), Some(1), "a non-repeat Enter fires activation");
        drop(owner);
    }

    #[test]
    fn grid_view_activate_without_callback_should_not_panic() {
        let owner = Owner::new();
        owner.set();
        let selection = RwSignal::new(GridSelection::single(0));
        let mut h = harness(|| sample_grid(10, Some(selection)));

        // Enter with a selection but no `.on_activate` wired: no fire, no panic, selection unchanged.
        press(&mut h, KeyCode::Enter);
        assert_eq!(
            selection.get_untracked().selected(),
            Some(0),
            "Enter with no callback leaves the selection unchanged and does not panic"
        );
        drop(owner);
    }

    #[test]
    fn grid_view_activate_with_no_selection_should_select_first_not_activate() {
        let owner = Owner::new();
        owner.set();
        let selection = RwSignal::new(GridSelection::none());
        let fired = Rc::new(Cell::new(None));
        let mut h = harness({
            let fired = fired.clone();
            move || activatable_grid(10, selection, fired)
        });

        // Enter with nothing selected selects item 0 (does not activate — activation needs a selected cell).
        press(&mut h, KeyCode::Enter);
        assert_eq!(
            selection.get_untracked().selected(),
            Some(0),
            "Enter with no selection selects the first item"
        );
        assert_eq!(
            fired.get(),
            None,
            "and does not fire activation (needs a selected cell)"
        );
        drop(owner);
    }
}
