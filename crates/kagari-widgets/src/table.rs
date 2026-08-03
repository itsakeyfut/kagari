//! The [`Table`] widget (#224, MVP): columns with a sticky header and a **virtualized** body, bound to
//! per-cell renderers, with single-row selection on click.
//!
//! It composes the virtualization foundation — the core [`virtualized_list`](kagari_core::virtualized_list)
//! element (#61) whose realized rows are interactive (#320) — into a tabular layout: a fixed-height sticky
//! header row above a scrolling body that builds only the visible rows. Each column has an explicit width
//! (the layout API has no flex-grow; fixed widths keep the header and every recycled body row aligned) and a
//! `cell` renderer `Fn(&R) -> impl IntoElement`. Selection is **controlled** via `RwSignal<Selection>` (D2);
//! a clicked row's **logical index** becomes the selection and its row is tinted.
//!
//! Sortable columns (#325): a column added with [`.column_sortable`](Table::column_sortable) gets a clickable
//! header that cycles `Asc → Desc → none`, reordering the rows by that column's key (with a `▲`/`▼` indicator).
//! Sort is keyed on the **logical row id** through the core's
//! [`virtualized_list_keyed`](kagari_core::virtualized_list_keyed), so re-sorting never leaves stale cells and
//! selection tracks the logical row across a sort.
//!
//! MVP scope: render + single-select + single-column sort. **Follow-ups**: keyboard row nav, resizable columns,
//! multi-column (Shift-click) sort, multi-select, DataGrid editable cells (a cell can already *be* a `text_input`
//! bound to a `RwSignal` field of `R`), reactive/streaming rows, horizontal scroll. The Table builds the core
//! element and owns the body `ScrollHandle`, so keyboard-nav / scroll-restore can land additively.

use std::cmp::Ordering;
use std::rc::Rc;

use kagari_base::{Point, SharedString, Size};
use kagari_core::element::Div;
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{
    AnyElement, IntoElement, MouseKind, ScrollHandle, div, text,
    virtualized_list_keyed as core_virtualized_list_keyed,
};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, label_px};

/// The Table's row selection (#224). Single-select today (a logical row index or nothing); a future
/// multi-select extends this type non-breakingly, so callers bind `RwSignal<Selection>` rather than a bare
/// `Option<usize>`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Selection(Option<usize>);

impl Selection {
    /// Nothing selected.
    pub fn none() -> Self {
        Selection(None)
    }

    /// Selects the single row at `row` (a **logical** row index into the `rows` passed to [`table`]).
    pub fn single(row: usize) -> Self {
        Selection(Some(row))
    }

    /// The selected logical row index, if any.
    pub fn selected(&self) -> Option<usize> {
        self.0
    }

    /// Whether `row` is selected (the internal predicate a multi-select variant generalizes).
    fn is_selected(&self, row: usize) -> bool {
        self.0 == Some(row)
    }
}

/// A boxed row comparator: a sortable column's `key` extractor is erased into this (#325).
type RowCmp<R> = Box<dyn Fn(&R, &R) -> Ordering>;

/// One table column: a header label, a fixed width, a per-row cell renderer, and (when sortable, #325) a
/// comparator used to order the rows when this column's header is clicked.
struct Column<R> {
    header: SharedString,
    width: f32,
    cell: Box<dyn Fn(&R) -> AnyElement>,
    sort: Option<RowCmp<R>>,
}

/// A sort direction (#325). A sortable header cycles `Asc → Desc → none` on successive clicks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SortDir {
    Asc,
    Desc,
}

/// The display→logical row permutation for `sort` (#325): `(0..count)` stably ordered by the active column's
/// comparator (reversed for `Desc`). Identity when nothing is sorted or the column carries no comparator. Stable,
/// so equal keys keep their input order and the `None` state restores the original row order.
fn sort_order<R>(
    sort: Option<(usize, SortDir)>,
    rows: &[R],
    columns: &[Column<R>],
    count: usize,
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..count).collect();
    if let Some((col, dir)) = sort {
        if let Some(cmp) = columns.get(col).and_then(|c| c.sort.as_ref()) {
            order.sort_by(|&a, &b| {
                let o = cmp(&rows[a], &rows[b]);
                if dir == SortDir::Desc { o.reverse() } else { o }
            });
        }
    }
    order
}

/// A columnar table with a sticky header and a virtualized body (#224). Build with [`table`], add columns with
/// [`.column`](Self::column), and chain `.selection(..)` / `.size(..)` / `.height(..)`. Returns
/// `impl IntoElement`; `Styled` is **not** on its surface.
pub struct Table<R> {
    rows: Vec<R>,
    columns: Vec<Column<R>>,
    selection: Option<RwSignal<Selection>>,
    size: ControlSize,
    row_height: Option<f32>,
    body_height: f32,
}

/// Creates a table over `rows`. Add columns with [`Table::column`]; wire selection with
/// [`Table::selection`]. The body scrolls (mouse wheel) within [`Table::height`] pixels.
pub fn table<R: 'static>(rows: Vec<R>) -> Table<R> {
    Table {
        rows,
        columns: Vec::new(),
        selection: None,
        size: ControlSize::Md,
        row_height: None,
        body_height: 320.0,
    }
}

impl<R: 'static> Table<R> {
    /// Appends a column: a `header` label, a fixed `width` (logical px), and a `cell` renderer built for each
    /// row. (`E: IntoElement` is a named generic — a nested `impl Fn(&R) -> impl IntoElement` is not valid
    /// Rust, E0562 — and is boxed to `AnyElement` internally.)
    pub fn column<E: IntoElement>(
        mut self,
        header: impl Into<SharedString>,
        width: f32,
        cell: impl Fn(&R) -> E + 'static,
    ) -> Self {
        self.columns.push(Column {
            header: header.into(),
            width,
            cell: Box::new(move |r| cell(r).into_element()),
            sort: None,
        });
        self
    }

    /// Appends a **sortable** column (#325): like [`.column`](Self::column) plus a `key` extractor — clicking this
    /// column's header cycles asc/desc/none and reorders the rows by `key(row)` (`K: Ord`; the key is boxed into a
    /// comparator internally, so columns may sort on different key types). A `▲`/`▼` indicator marks the active
    /// column and direction.
    pub fn column_sortable<E: IntoElement, K: Ord + 'static>(
        mut self,
        header: impl Into<SharedString>,
        width: f32,
        cell: impl Fn(&R) -> E + 'static,
        key: impl Fn(&R) -> K + 'static,
    ) -> Self {
        self.columns.push(Column {
            header: header.into(),
            width,
            cell: Box::new(move |r| cell(r).into_element()),
            sort: Some(Box::new(move |a, b| key(a).cmp(&key(b)))),
        });
        self
    }

    /// Binds the controlled selection (D2). A clicked row sets it to `Selection::single(logical_index)` and
    /// the selected row is tinted. When unset, rows render but are not selectable.
    pub fn selection(mut self, selection: RwSignal<Selection>) -> Self {
        self.selection = Some(selection);
        self
    }

    /// Sets the control size (D3) — drives the header/cell font and horizontal padding (and the default row
    /// height).
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Sets the scrolling body's viewport height (logical px). The body width is the sum of column widths.
    pub fn height(mut self, height: f32) -> Self {
        self.body_height = height;
        self
    }

    /// Overrides the row height (logical px); defaults from [`ControlSize`].
    pub fn row_height(mut self, row_height: f32) -> Self {
        self.row_height = Some(row_height);
        self
    }
}

/// The default row/header height for `size`.
fn default_row_height(size: ControlSize) -> f32 {
    match size {
        ControlSize::Sm => 24.0,
        ControlSize::Md => 28.0,
        ControlSize::Lg => 34.0,
    }
}

/// Applies `size`-scaled horizontal padding to a header/body cell (matches the control spacing scale).
fn hpad(d: Div, size: ControlSize) -> Div {
    match size {
        ControlSize::Sm => d.px_2(),
        ControlSize::Md => d.px_3(),
        ControlSize::Lg => d.px_4(),
    }
}

/// Builds one body row for logical index `i`: a fixed-width flex row of cells, tinted `Accent` when selected.
/// Cell content is whatever the column's renderer produces (it sets its own font — the header uses the
/// size-derived font, but a cell can be any element, e.g. an input widget for a DataGrid).
fn build_row<R>(
    i: usize,
    rows: &Rc<Vec<R>>,
    columns: &Rc<Vec<Column<R>>>,
    selection: Option<RwSignal<Selection>>,
    total_w: f32,
    row_h: f32,
    size: ControlSize,
) -> Div {
    let mut row = div().flex().size(Size {
        w: total_w,
        h: row_h,
    });
    match selection {
        Some(sel) => {
            row = row
                .bg(rx(move || {
                    if sel.get().is_selected(i) {
                        ColorRole::Accent
                    } else {
                        ColorRole::Surface
                    }
                }))
                .on_click(move |_, _| sel.set(Selection::single(i)));
        }
        None => row = row.bg(ColorRole::Surface),
    }
    for c in columns.iter() {
        let cell = hpad(
            div().size(Size {
                w: c.width,
                h: row_h,
            }),
            size,
        )
        .items_center()
        .child((c.cell)(&rows[i]));
        row = row.child(cell);
    }
    row
}

impl<R: 'static> IntoElement for Table<R> {
    fn into_element(self) -> AnyElement {
        let Table {
            rows,
            columns,
            selection,
            size,
            row_height,
            body_height,
        } = self;
        let font = label_px(size);
        let row_h = row_height.unwrap_or_else(|| default_row_height(size));
        let header_h = row_h;
        let total_w: f32 = columns.iter().map(|c| c.width).sum();
        let count = rows.len();
        let rows = Rc::new(rows);
        let columns = Rc::new(columns);

        // Sort state (#325): `sort_signal` drives the header indicator; `order_signal` is the display→logical row
        // permutation the body's key_fn reads. Created once, before the header (its clicks write both).
        let sort_signal = RwSignal::new(None::<(usize, SortDir)>);
        let order_signal = RwSignal::new((0..count).collect::<Vec<usize>>());

        // Sticky header (non-virtualized): a raised flex row of fixed-width header cells. A sortable column's cell
        // gets a `▲`/`▼` indicator and a click that cycles asc→desc→none and recomputes the row order.
        let mut header = div().flex().bg(ColorRole::SurfaceRaised);
        for (ci, c) in columns.iter().enumerate() {
            let mut cell = hpad(
                div().size(Size {
                    w: c.width,
                    h: header_h,
                }),
                size,
            )
            .flex()
            .items_center()
            .child(
                text(c.header.clone())
                    .size(font)
                    .color_role(ColorRole::Text),
            );
            if c.sort.is_some() {
                cell = cell.child(
                    text(rx(move || match sort_signal.get() {
                        Some((cc, SortDir::Asc)) if cc == ci => SharedString::from(" ▲"),
                        Some((cc, SortDir::Desc)) if cc == ci => SharedString::from(" ▼"),
                        _ => SharedString::from(""),
                    }))
                    .size(font)
                    .color_role(ColorRole::TextMuted),
                );
                // The click cycles this column's 3-state sort and recomputes the order. It captures the row/column
                // `Rc`s to sort by the active comparator — fine, a `Div` handler is not `Send + Sync` (like Menu).
                let rows_h = rows.clone();
                let cols_h = columns.clone();
                cell = cell.on_click(move |_, _| {
                    let next = match sort_signal.get_untracked() {
                        Some((cc, SortDir::Asc)) if cc == ci => Some((ci, SortDir::Desc)),
                        Some((cc, SortDir::Desc)) if cc == ci => None,
                        _ => Some((ci, SortDir::Asc)),
                    };
                    order_signal.set(sort_order(next, &rows_h, &cols_h, count));
                    sort_signal.set(next);
                });
            }
            header = header.child(cell);
        }

        // Virtualized body: build the CORE element so the Table owns the scroll handle (forward-fits
        // keyboard-nav / scroll-restore); re-add the wheel handler (the widget's ~8-line version). The **keyed**
        // core (#325 / RK-049) identifies rows by their **logical id** (`order[slot]`), so a re-sort repositions
        // rows without stale cells; `build_row` is unchanged (it builds `rows[id]` by logical index, so selection
        // tracks the logical row across a sort). `key_fn` is `Send + Sync` (captures only the order signal).
        let handle = ScrollHandle::new();
        let max_y = (count as f32 * row_h - body_height).max(0.0);
        let rows_b = rows.clone();
        let cols_b = columns.clone();
        let key_fn = move |slot: usize| order_signal.with(|o| o[slot]);
        let core = core_virtualized_list_keyed(
            row_h,
            count,
            Size {
                w: total_w,
                h: body_height,
            },
            &handle,
            key_fn,
            move |id| build_row(id, &rows_b, &cols_b, selection, total_w, row_h, size),
        );
        let wheel = handle.clone();
        let body = div()
            .size(Size {
                w: total_w,
                h: body_height,
            })
            .on_wheel(move |ev, _| {
                if let MouseKind::Wheel { dy, .. } = ev.kind {
                    // `dy` is already a logical-pixel scroll distance (the app layer scales a wheel notch by
                    // `WHEEL_LINE_PX`), so apply it directly — multiplying by `row_h` again over-scrolls by a
                    // whole row-height per pixel.
                    let next = (wheel.offset().y - dy).clamp(0.0, max_y);
                    wheel.jump_to(Point::new(0.0, next));
                }
            })
            .child(core);

        div()
            .flex_col()
            .child(header)
            .child(div().h_px().bg(ColorRole::Border)) // header / body divider
            .child(body)
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    use kagari_base::{NodeId, Point, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::Owner;
    use kagari_core::{
        DispatchState, HitTest, Modifiers, MouseButton, MouseEvent, MouseKind, dispatch_mouse,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 400.0, h: 400.0 };

    struct Row {
        name: &'static str,
        age: u32,
    }

    fn sample_rows(n: usize) -> Vec<Row> {
        (0..n)
            .map(|i| Row {
                name: "row",
                age: i as u32,
            })
            .collect()
    }

    /// A 3-column, `n`-row table in a 100px-tall body (so the body virtualizes), optionally wired to `sel`.
    fn sample_table(n: usize, sel: Option<RwSignal<Selection>>) -> AnyElement {
        let mut t = table(sample_rows(n))
            .column("Name", 120.0, |r: &Row| text(r.name))
            .column("Age", 80.0, |r: &Row| text(format!("{}", r.age)))
            .column("Id", 60.0, |r: &Row| text(format!("#{}", r.age)))
            .size(ControlSize::Md)
            .height(100.0);
        if let Some(s) = sel {
            t = t.selection(s);
        }
        t.into_element()
    }

    struct Harness {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        hit: HitTest,
        damage: StdArc<DamageState>,
        theme: Theme,
        root_id: NodeId,
    }

    fn harness(root: AnyElement) -> Harness {
        let mut h = Harness {
            root,
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
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
            None,
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

    /// The core virtualized body node: root (flex_col) → child[2] (body div) → child[0] (core element).
    fn core_node(h: &Harness) -> NodeId {
        let body = h.arena.get(h.root_id).unwrap().children[2];
        h.arena.get(body).unwrap().children[0]
    }

    /// The realized body row nodes (children of the core node).
    fn body_rows(h: &Harness) -> Vec<NodeId> {
        h.arena.get(core_node(h)).unwrap().children.clone()
    }

    /// Clicks the body row at logical `index` (at offset 0 it paints at `core.y + index*row_h`).
    fn click_row(h: &mut Harness, index: usize) {
        let row_h = default_row_height(ControlSize::Md);
        let cb = h.layout.absolute_layout(core_node(h));
        let p = Point::new(
            cb.origin.x + 20.0,
            cb.origin.y + index as f32 * row_h + row_h / 2.0,
        );
        let mut dispatch = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(kind, p, Modifiers::default());
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut dispatch);
        }
    }

    #[test]
    fn table_should_render_columns_and_rows() {
        let owner = Owner::new();
        owner.set();
        let h = harness(sample_table(100, None));

        // Header (root child 0) has one cell per column.
        let header = h.arena.get(h.root_id).unwrap().children[0];
        assert_eq!(
            h.arena.get(header).unwrap().children.len(),
            3,
            "the header renders one cell per column"
        );
        // The body virtualizes: only a window of rows is realized, each with 3 cells.
        let rows = body_rows(&h);
        assert!(
            !rows.is_empty() && rows.len() < 100,
            "the body realizes only the visible window, not all 100 rows"
        );
        // The core wraps each row in an item-height slot; the row (with its 3 cells) is the slot's child.
        let row = h.arena.get(rows[0]).unwrap().children[0];
        assert_eq!(
            h.arena.get(row).unwrap().children.len(),
            3,
            "each body row renders one cell per column"
        );
        drop(owner);
    }

    #[test]
    fn table_row_click_should_select() {
        let owner = Owner::new();
        owner.set();
        let selection = RwSignal::new(Selection::none());
        let mut h = harness(sample_table(100, Some(selection)));

        click_row(&mut h, 2);
        assert_eq!(
            selection.get_untracked().selected(),
            Some(2),
            "clicking a row selects its logical index"
        );
        click_row(&mut h, 0);
        assert_eq!(
            selection.get_untracked().selected(),
            Some(0),
            "clicking another row moves the selection"
        );
        drop(owner);
    }

    #[test]
    fn table_selected_row_should_highlight() {
        let owner = Owner::new();
        owner.set();
        let selection = RwSignal::new(Selection::single(1));
        let mut h = harness(sample_table(100, Some(selection)));

        let accent = h.theme.resolve_color(ColorRole::Accent);
        let (_, scene) = frame(&mut h);
        let has_accent_row = scene
            .quads
            .iter()
            .any(|q| q.bg == kagari_render::Background::Solid(accent));
        assert!(
            has_accent_row,
            "the selected row paints an Accent background"
        );
        drop(owner);
    }

    #[test]
    fn table_without_selection_should_render() {
        let owner = Owner::new();
        owner.set();
        // No `.selection` wired: rows render and a click is inert (no panic, no fabricated signal).
        let mut h = harness(sample_table(50, None));
        assert!(!body_rows(&h).is_empty(), "rows render without a selection");
        click_row(&mut h, 1); // must not panic
        drop(owner);
    }

    #[test]
    fn table_empty_should_render_nothing() {
        let owner = Owner::new();
        owner.set();
        let h = harness(sample_table(0, None));
        assert_eq!(
            body_rows(&h).len(),
            0,
            "a zero-row table renders the header but no body rows"
        );
        drop(owner);
    }

    /// `n` rows whose age is the permutation `age(i) = (i + 1) % n`, so the logical order is neither ascending nor
    /// descending in age — sorting genuinely reorders and asc/desc/none each put a distinct logical row at slot 0.
    fn perm_rows(n: usize) -> Vec<Row> {
        (0..n)
            .map(|i| Row {
                name: "row",
                age: ((i + 1) % n) as u32,
            })
            .collect()
    }

    /// A table over [`perm_rows`] whose column 1 (`Age`) is **sortable** by `age`, in a 100px body (so it
    /// virtualizes), optionally wired to `sel`.
    fn sortable_table(n: usize, sel: Option<RwSignal<Selection>>) -> AnyElement {
        let mut t = table(perm_rows(n))
            .column("Name", 120.0, |r: &Row| text(r.name))
            .column_sortable(
                "Age",
                80.0,
                |r: &Row| text(format!("{}", r.age)),
                |r: &Row| r.age,
            )
            .size(ControlSize::Md)
            .height(100.0);
        if let Some(s) = sel {
            t = t.selection(s);
        }
        t.into_element()
    }

    /// Clicks the header cell for column `col` (its `on_click` cycles that column's sort when sortable).
    fn click_header(h: &mut Harness, col: usize) {
        let header = h.arena.get(h.root_id).unwrap().children[0];
        let cell = h.arena.get(header).unwrap().children[col];
        let cb = h.layout.absolute_layout(cell);
        let p = Point::new(cb.origin.x + cb.size.w / 2.0, cb.origin.y + cb.size.h / 2.0);
        let mut dispatch = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(kind, p, Modifiers::default());
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut dispatch);
        }
    }

    /// Scrolls the body by dispatching a wheel event over it (the body's handler jumps by `dy` logical px, no spring).
    fn wheel(h: &mut Harness, dy: f32) {
        let body = h.arena.get(h.root_id).unwrap().children[2];
        let bb = h.layout.absolute_layout(body);
        let p = Point::new(bb.origin.x + 10.0, bb.origin.y + 10.0);
        let ev = MouseEvent::new(MouseKind::Wheel { dx: 0.0, dy }, p, Modifiers::default());
        let mut dispatch = DispatchState::default();
        dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut dispatch);
    }

    #[test]
    fn table_header_should_sort() {
        let owner = Owner::new();
        owner.set();
        let sel = RwSignal::new(Selection::none());
        let mut h = harness(sortable_table(100, Some(sel)));

        // Unsorted: identity order → slot 0 is logical row 0.
        click_row(&mut h, 0);
        assert_eq!(
            sel.get_untracked().selected(),
            Some(0),
            "unsorted: slot 0 is logical row 0"
        );
        // Ascending by age: the smallest age (logical row 99, age 0) rises to slot 0.
        click_header(&mut h, 1);
        frame(&mut h);
        click_row(&mut h, 0);
        assert_eq!(
            sel.get_untracked().selected(),
            Some(99),
            "ascending by age puts logical row 99 (age 0) at slot 0"
        );
        drop(owner);
    }

    #[test]
    fn table_sort_should_cycle_asc_desc_none() {
        let owner = Owner::new();
        owner.set();
        let sel = RwSignal::new(Selection::none());
        let mut h = harness(sortable_table(100, Some(sel)));

        // Each header click advances Asc → Desc → none; slot 0's logical row is distinct in each state.
        click_header(&mut h, 1);
        frame(&mut h);
        click_row(&mut h, 0);
        assert_eq!(
            sel.get_untracked().selected(),
            Some(99),
            "1st click → ascending (logical 99 at slot 0)"
        );
        click_header(&mut h, 1);
        frame(&mut h);
        click_row(&mut h, 0);
        assert_eq!(
            sel.get_untracked().selected(),
            Some(98),
            "2nd click → descending (logical 98 at slot 0)"
        );
        click_header(&mut h, 1);
        frame(&mut h);
        click_row(&mut h, 0);
        assert_eq!(
            sel.get_untracked().selected(),
            Some(0),
            "3rd click → unsorted (logical 0 at slot 0)"
        );
        drop(owner);
    }

    #[test]
    fn table_sort_should_keep_logical_selection() {
        let owner = Owner::new();
        owner.set();
        let sel = RwSignal::new(Selection::none());
        let mut h = harness(sortable_table(100, Some(sel)));

        // Select logical row 99 while unsorted — far down and NOT realized (virtualized), so no tint yet.
        sel.set(Selection::single(99));
        let accent = h.theme.resolve_color(ColorRole::Accent);
        let (_, scene) = frame(&mut h);
        assert!(
            !scene
                .quads
                .iter()
                .any(|q| q.bg == kagari_render::Background::Solid(accent)),
            "the selected logical row is scrolled out of the window, so nothing tints"
        );
        // Sort ascending → logical 99 (age 0) moves to slot 0, is realized, and still paints its Accent tint.
        click_header(&mut h, 1);
        let (_, scene) = frame(&mut h);
        assert!(
            scene
                .quads
                .iter()
                .any(|q| q.bg == kagari_render::Background::Solid(accent)),
            "the selected logical row rides the sort into view and stays tinted"
        );
        assert_eq!(
            sel.get_untracked().selected(),
            Some(99),
            "sorting does not change the logical selection"
        );
        drop(owner);
    }

    #[test]
    fn table_sort_then_scroll_should_not_stale() {
        let owner = Owner::new();
        owner.set();
        let sel = RwSignal::new(Selection::none());
        let mut h = harness(sortable_table(100, Some(sel)));

        // Ascending → order = [99, 0, 1, 2, 3, 4, …]; then scroll down exactly 5 rows. `dy` is in logical pixels,
        // so 5 rows == `5 * row_h` px (the handler applies `dy` directly).
        click_header(&mut h, 1);
        frame(&mut h);
        wheel(&mut h, -5.0 * default_row_height(ControlSize::Md));
        frame(&mut h);
        // With an exact 5-row offset the top slot paints slot 5 = order[5] = logical row 4. A stale/mis-keyed body
        // would select the wrong logical row here.
        click_row(&mut h, 0);
        assert_eq!(
            sel.get_untracked().selected(),
            Some(4),
            "after sort+scroll the top slot maps to the correct logical row (no stale cell)"
        );
        drop(owner);
    }

    #[test]
    fn table_sort_should_keep_input_order_for_equal_keys() {
        let owner = Owner::new();
        owner.set();
        let sel = RwSignal::new(Selection::none());
        // Two tie groups: odd logical rows have age 0, even rows have age 1. An ascending sort must place the
        // whole age-0 group (the odd indices) first AND keep them in input (logical) order — a *stable* sort.
        let rows: Vec<Row> = (0..100)
            .map(|i| Row {
                name: "row",
                age: if i % 2 == 0 { 1 } else { 0 },
            })
            .collect();
        let mut h = harness(
            table(rows)
                .column("Name", 120.0, |r: &Row| text(r.name))
                .column_sortable(
                    "Age",
                    80.0,
                    |r: &Row| text(format!("{}", r.age)),
                    |r: &Row| r.age,
                )
                .size(ControlSize::Md)
                .height(100.0)
                .selection(sel)
                .into_element(),
        );
        click_header(&mut h, 1);
        frame(&mut h);
        click_row(&mut h, 0);
        assert_eq!(
            sel.get_untracked().selected(),
            Some(1),
            "ascending: the first age-0 tie row is logical 1 (stable → lowest input index first)"
        );
        click_row(&mut h, 1);
        assert_eq!(
            sel.get_untracked().selected(),
            Some(3),
            "ascending: the second age-0 tie row is logical 3 (ties keep input order)"
        );
        drop(owner);
    }

    #[test]
    fn table_nonsortable_header_click_should_not_reorder() {
        let owner = Owner::new();
        owner.set();
        let sel = RwSignal::new(Selection::none());
        let mut h = harness(sortable_table(100, Some(sel)));
        // Column 0 ("Name") is a plain non-sortable column (no comparator, no click handler) → clicking its
        // header is inert and must not reorder the body.
        click_header(&mut h, 0);
        frame(&mut h);
        click_row(&mut h, 0);
        assert_eq!(
            sel.get_untracked().selected(),
            Some(0),
            "clicking a non-sortable header leaves the order unchanged (slot 0 stays logical 0)"
        );
        drop(owner);
    }

    #[test]
    fn table_empty_header_click_should_not_panic() {
        let owner = Owner::new();
        owner.set();
        // A 0-row table still builds a sortable header; clicking it computes `sort_order` over `0..0` and an
        // empty `order` the (never-called) key_fn would read — this must not panic.
        let mut h = harness(sortable_table(0, None));
        click_header(&mut h, 1);
        frame(&mut h);
        assert_eq!(
            body_rows(&h).len(),
            0,
            "an empty table has no body rows after a header click"
        );
        drop(owner);
    }
}
