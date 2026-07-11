//! The [`Tree`] widget (#223, MVP): a hierarchical list with expand/collapse, single-row selection, and
//! keyboard navigation — windowed by a **rebuild-on-expand** virtualized body.
//!
//! The caller builds a nested [`TreeNode`] model; the widget flattens it once into a pre-order arena (a
//! `Send + Sync` `meta` plane consumed by the reactive body + an `Rc` `data` plane consumed only by the
//! per-row renderer). A single-item [`dyn_list`](kagari_core::element) keyed on the current **visible** row
//! list rebuilds the core [`virtualized_list`](kagari_core::virtualized_list) with the fresh visible count
//! whenever `expanded` changes; the Tree owns the body [`ScrollHandle`], so the scroll offset is preserved
//! across a rebuild and only the visible window (± a buffer) is ever built. Selection is controlled via
//! `RwSignal<TreeSelection<K>>` (an opaque type mirroring the [`Table`](crate::Table)'s — multi-select
//! extends it non-breakingly), and `expanded` via `RwSignal<HashSet<K>>`.
//!
//! MVP scope: render + expand/collapse + single-select + keyboard nav. **Node keys must be unique**
//! (`debug_assert`ed at build). Uniform row height is an invariant (the virtualized body requires it).
//! **Follow-ups**: multi-select, drag-reorder (#178), lazy `TreeSource`, `Role::Tree` a11y.

use std::collections::HashSet;
use std::hash::Hash;
use std::rc::Rc;
use std::sync::Arc;

use kagari_base::{Point, SharedString, Size};
use kagari_core::element::{Div, dyn_list};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{
    AnyElement, IntoElement, KeyCode, MouseKind, ScrollHandle, div, text, use_focus_handle,
    virtualized_list as core_virtualized_list,
};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, label_px};

/// The width the tree lays its rows / scroll viewport to (MVP: fixed, no horizontal scroll).
const TREE_WIDTH: f32 = 280.0;

/// A boxed per-node row-content renderer (the caller's `Fn(&T) -> impl IntoElement`, erased to `AnyElement`).
type NodeView<T> = Rc<dyn Fn(&T) -> AnyElement>;

/// The Tree's row selection (#223). Single-select today (a node key or nothing); a future multi-select
/// extends this type non-breakingly, so callers bind `RwSignal<TreeSelection<K>>` rather than a bare
/// `Option<K>`. Mirrors the [`Table`](crate::Table)'s `Selection`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TreeSelection<K>(Option<K>);

impl<K: Copy + PartialEq> TreeSelection<K> {
    /// Nothing selected.
    pub fn none() -> Self {
        TreeSelection(None)
    }

    /// Selects the single node with `key`.
    pub fn single(key: K) -> Self {
        TreeSelection(Some(key))
    }

    /// The selected node key, if any.
    pub fn selected(&self) -> Option<K> {
        self.0
    }

    /// Whether `key` is selected (the internal predicate a multi-select variant generalizes).
    fn is_selected(&self, key: K) -> bool {
        self.0 == Some(key)
    }
}

/// A node in the caller's tree model (nested; the widget flattens it internally). `key` must be unique
/// across the whole tree; `data` is rendered by [`Tree::node`].
pub struct TreeNode<K, T> {
    pub key: K,
    pub data: T,
    pub children: Vec<TreeNode<K, T>>,
}

impl<K, T> TreeNode<K, T> {
    /// A childless node.
    pub fn leaf(key: K, data: T) -> Self {
        TreeNode {
            key,
            data,
            children: Vec::new(),
        }
    }

    /// A node with `children`.
    pub fn branch(key: K, data: T, children: Vec<TreeNode<K, T>>) -> Self {
        TreeNode {
            key,
            data,
            children,
        }
    }
}

/// Per-node structural metadata (no `T`) — `Send + Sync`, so the reactive visible-set closure can hold it.
struct NodeMeta<K> {
    key: K,
    depth: usize,
    has_children: bool,
}

/// A hierarchical tree widget (#223). Build with [`tree`], set the row renderer with [`Tree::node`], and
/// chain `.expanded(..)` / `.selection(..)` / `.size(..)` / `.height(..)`. Returns `impl IntoElement`;
/// `Styled` is **not** on its surface.
pub struct Tree<K, T> {
    roots: Vec<TreeNode<K, T>>,
    node_view: Option<NodeView<T>>,
    expanded: Option<RwSignal<HashSet<K>>>,
    selection: Option<RwSignal<TreeSelection<K>>>,
    size: ControlSize,
    body_height: f32,
}

/// Creates a tree over `roots` (nested [`TreeNode`]s). Set the row content with [`Tree::node`]; wire
/// expand/collapse and selection with [`Tree::expanded`] / [`Tree::selection`].
pub fn tree<K, T>(roots: Vec<TreeNode<K, T>>) -> Tree<K, T>
where
    K: Copy + Eq + Hash + Send + Sync + 'static,
    T: 'static,
{
    Tree {
        roots,
        node_view: None,
        expanded: None,
        selection: None,
        size: ControlSize::Md,
        body_height: 320.0,
    }
}

impl<K, T> Tree<K, T>
where
    K: Copy + Eq + Hash + Send + Sync + 'static,
    T: 'static,
{
    /// Sets the per-node row content renderer. (`E: IntoElement` is a named generic — a nested
    /// `impl Fn(&T) -> impl IntoElement` is E0562 — boxed to `AnyElement` internally.)
    pub fn node<V: IntoElement>(mut self, view: impl Fn(&T) -> V + 'static) -> Self {
        self.node_view = Some(Rc::new(move |t| view(t).into_element()));
        self
    }

    /// Binds the controlled expand/collapse set (which node keys are open). Unset ⇒ an internal set (all
    /// collapsed) is used so the disclosure carets still work.
    pub fn expanded(mut self, expanded: RwSignal<HashSet<K>>) -> Self {
        self.expanded = Some(expanded);
        self
    }

    /// Binds the controlled selection (D2). A clicked row (or keyboard move) sets it; the selected row tints
    /// `Accent`. Unset ⇒ rows render inert (no selection, no keyboard nav).
    pub fn selection(mut self, selection: RwSignal<TreeSelection<K>>) -> Self {
        self.selection = Some(selection);
        self
    }

    /// Sets the control size (D3) — drives the row height, indent step, and caret font.
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Sets the scrolling body's viewport height (logical px).
    pub fn height(mut self, height: f32) -> Self {
        self.body_height = height;
        self
    }
}

/// Row / indent metrics for `size`.
fn tree_row_height(size: ControlSize) -> f32 {
    match size {
        ControlSize::Sm => 22.0,
        ControlSize::Md => 26.0,
        ControlSize::Lg => 30.0,
    }
}
fn tree_indent(size: ControlSize) -> f32 {
    match size {
        ControlSize::Sm => 14.0,
        ControlSize::Md => 16.0,
        ControlSize::Lg => 20.0,
    }
}

/// Flattens the nested `roots` into a pre-order `(meta, data)` pair (parallel vectors). `debug_assert`s all
/// keys are unique (duplicates break the `HashSet<K>` / `dyn_list` identity model).
fn flatten<K: Copy + Eq + Hash, T>(roots: Vec<TreeNode<K, T>>) -> (Vec<NodeMeta<K>>, Vec<T>) {
    fn walk<K: Copy + Eq + Hash, T>(
        nodes: Vec<TreeNode<K, T>>,
        depth: usize,
        meta: &mut Vec<NodeMeta<K>>,
        data: &mut Vec<T>,
        seen: &mut HashSet<K>,
    ) {
        for node in nodes {
            debug_assert!(
                seen.insert(node.key),
                "kagari Tree: node keys must be unique across the whole tree"
            );
            meta.push(NodeMeta {
                key: node.key,
                depth,
                has_children: !node.children.is_empty(),
            });
            data.push(node.data);
            walk(node.children, depth + 1, meta, data, seen);
        }
    }
    let mut meta = Vec::new();
    let mut data = Vec::new();
    walk(roots, 0, &mut meta, &mut data, &mut HashSet::new());
    (meta, data)
}

/// The visible arena indices (pre-order), descending into a node's subtree only if it is expanded. Depth-only
/// walk: on a collapsed branch, skip deeper nodes until the depth returns to that branch's level.
fn visible<K: Eq + Hash>(meta: &[NodeMeta<K>], expanded: &HashSet<K>) -> Vec<usize> {
    let mut vis = Vec::new();
    let mut skip_below: Option<usize> = None;
    for (i, m) in meta.iter().enumerate() {
        if let Some(d) = skip_below {
            if m.depth > d {
                continue;
            }
            skip_below = None;
        }
        vis.push(i);
        if m.has_children && !expanded.contains(&m.key) {
            skip_below = Some(m.depth);
        }
    }
    vis
}

/// Builds one body row for arena index `arena_i`: indent spacer + disclosure caret (branches) + node content,
/// tinted `Accent` when selected. Caret clicks toggle `expanded` (and stop propagation so they do not also
/// select); the row body click selects (when a selection signal is wired).
#[allow(clippy::too_many_arguments)]
fn build_row<K, T>(
    arena_i: usize,
    meta: &Arc<Vec<NodeMeta<K>>>,
    data: &Rc<Vec<T>>,
    node_view: &Option<NodeView<T>>,
    expanded: RwSignal<HashSet<K>>,
    selection: Option<RwSignal<TreeSelection<K>>>,
    size: ControlSize,
) -> Div
where
    K: Copy + Eq + Hash + Send + Sync + 'static,
    T: 'static,
{
    let m = &meta[arena_i];
    let (key, depth, has_children) = (m.key, m.depth, m.has_children);
    let row_h = tree_row_height(size);
    let indent = tree_indent(size);
    let font = label_px(size);

    let mut row = div().flex().items_center().size(Size {
        w: TREE_WIDTH,
        h: row_h,
    });
    // Selection tint: reactive when a selection signal is wired, static otherwise (two-path, no dead effect).
    row = match selection {
        Some(sel) => row.bg(rx(move || {
            if sel.get().is_selected(key) {
                ColorRole::Accent
            } else {
                ColorRole::Surface
            }
        })),
        None => row.bg(ColorRole::Surface),
    };
    // Row-body click: toggle a branch's expansion AND select it (the caret handler stops propagation, so a
    // caret click toggles once without also selecting). Attached when the row can do either.
    if has_children || selection.is_some() {
        row = row.on_click(move |_, _| {
            if has_children {
                expanded.update(|s| {
                    if !s.remove(&key) {
                        s.insert(key);
                    }
                });
            }
            if let Some(sel) = selection {
                sel.set(TreeSelection::single(key));
            }
        });
    }

    // Indentation: a leading spacer (no `.pl` for arbitrary px on Div).
    row = row.child(div().size(Size {
        w: depth as f32 * indent,
        h: row_h,
    }));

    // Disclosure caret (branch) or a caret-sized spacer (leaf), keeping content aligned.
    if has_children {
        row = row.child(
            div()
                .size(Size {
                    w: indent,
                    h: row_h,
                })
                .items_center()
                .on_click(move |_, cx| {
                    expanded.update(|s| {
                        if !s.remove(&key) {
                            s.insert(key);
                        }
                    });
                    cx.stop_propagation();
                })
                .child(
                    text(rx(move || {
                        SharedString::from(if expanded.with(|s| s.contains(&key)) {
                            "▾"
                        } else {
                            "▸"
                        })
                    }))
                    .color_role(ColorRole::TextMuted)
                    .size(font),
                ),
        );
    } else {
        row = row.child(div().size(Size {
            w: indent,
            h: row_h,
        }));
    }

    if let Some(view) = node_view {
        row = row.child(view(&data[arena_i]));
    }
    row
}

/// Keeps row `pos` within the viewport by nudging the scroll offset.
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

/// Handles a key press: Down/Up move the selection over the visible order (falling back to the first visible
/// when the current selection is hidden); Right expands or descends; Left collapses or ascends to the parent;
/// Enter selects. Scrolls to keep the selected row visible.
fn keyboard<K: Copy + Eq + Hash + Send + Sync + 'static>(
    code: KeyCode,
    meta: &[NodeMeta<K>],
    expanded: RwSignal<HashSet<K>>,
    selection: RwSignal<TreeSelection<K>>,
    handle: &ScrollHandle,
    row_h: f32,
    body_height: f32,
) {
    let vis = expanded.with_untracked(|s| visible(meta, s));
    if vis.is_empty() {
        return;
    }
    let cur_pos = selection
        .get_untracked()
        .selected()
        .and_then(|k| vis.iter().position(|&i| meta[i].key == k));

    let select = |pos: usize| {
        selection.set(TreeSelection::single(meta[vis[pos]].key));
        scroll_into_view(handle, pos, row_h, body_height);
    };

    match code {
        KeyCode::ArrowDown => select(cur_pos.map(|p| (p + 1).min(vis.len() - 1)).unwrap_or(0)),
        KeyCode::ArrowUp => select(cur_pos.map(|p| p.saturating_sub(1)).unwrap_or(0)),
        KeyCode::ArrowRight => {
            if let Some(p) = cur_pos {
                let i = vis[p];
                let is_expanded = expanded.with_untracked(|s| s.contains(&meta[i].key));
                if meta[i].has_children && !is_expanded {
                    expanded.update(|s| {
                        s.insert(meta[i].key);
                    });
                } else if p + 1 < vis.len() {
                    select(p + 1);
                }
            }
        }
        KeyCode::ArrowLeft => {
            if let Some(p) = cur_pos {
                let i = vis[p];
                let is_expanded = expanded.with_untracked(|s| s.contains(&meta[i].key));
                if meta[i].has_children && is_expanded {
                    expanded.update(|s| {
                        s.remove(&meta[i].key);
                    });
                } else if let Some(pp) = (0..p).rev().find(|&q| meta[vis[q]].depth < meta[i].depth)
                {
                    select(pp);
                }
            }
        }
        KeyCode::Enter | KeyCode::NumpadEnter if cur_pos.is_none() => select(0),
        _ => {}
    }
}

impl<K, T> IntoElement for Tree<K, T>
where
    K: Copy + Eq + Hash + Send + Sync + 'static,
    T: 'static,
{
    fn into_element(self) -> AnyElement {
        let Tree {
            roots,
            node_view,
            expanded,
            selection,
            size,
            body_height,
        } = self;
        let (meta, data) = flatten(roots);
        let meta = Arc::new(meta);
        let data = Rc::new(data);
        let expanded = expanded.unwrap_or_else(|| RwSignal::new(HashSet::new()));
        let row_h = tree_row_height(size);
        let viewport = Size {
            w: TREE_WIDTH,
            h: body_height,
        };
        let handle = ScrollHandle::new();

        // Body: rebuild-on-expand — a single-item dyn_list keyed on the visible list rebuilds the core
        // virtualized_list (with the fresh visible count) on every `expanded` change; the owned handle keeps
        // the scroll offset across rebuilds, and scrolling re-windows inside the virtualized element.
        let meta_items = meta.clone();
        let meta_view = meta.clone();
        let data_view = data.clone();
        let nv = node_view.clone();
        let view_handle = handle.clone();
        let body_list = dyn_list(
            move || expanded.with(|s| vec![visible(&meta_items, s)]),
            |vis: &Vec<usize>| vis.clone(),
            move |vis: &Vec<usize>| {
                let vis = vis.clone();
                let meta_r = meta_view.clone();
                let data_r = data_view.clone();
                let nv = nv.clone();
                core_virtualized_list(row_h, vis.len(), viewport, &view_handle, move |win_i| {
                    build_row(vis[win_i], &meta_r, &data_r, &nv, expanded, selection, size)
                })
            },
        );

        let meta_wheel = meta.clone();
        let wheel_handle = handle.clone();
        let body = div()
            .size(viewport)
            .on_wheel(move |ev, _| {
                if let MouseKind::Wheel { dy, .. } = ev.kind {
                    let count = expanded.with_untracked(|s| visible(&meta_wheel, s).len());
                    let max_y = (count as f32 * row_h - body_height).max(0.0);
                    let next = (wheel_handle.offset().y - dy * row_h).clamp(0.0, max_y);
                    wheel_handle.jump_to(Point::new(0.0, next));
                }
            })
            .child(body_list);

        // Focus + keyboard navigation only when a selection signal is wired (selection is the nav cursor).
        match selection {
            Some(sel) => {
                let focus = use_focus_handle();
                let ring = focus.clone();
                let meta_kb = meta.clone();
                let kb_handle = handle.clone();
                div()
                    .rounded_md()
                    .border_w_1()
                    .border_color(rx(move || {
                        ring.is_focus_visible().then_some(ColorRole::FocusRing)
                    }))
                    .track_focus(&focus)
                    .on_key_down(move |kev, _| {
                        if kev.repeat {
                            return;
                        }
                        keyboard(
                            kev.code,
                            &meta_kb,
                            expanded,
                            sel,
                            &kb_handle,
                            row_h,
                            body_height,
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
    use std::sync::Arc as StdArc;

    use kagari_base::{NodeId, Point, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::FocusRegistry;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{
        DispatchState, HitTest, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseKind,
        dispatch_key, dispatch_mouse,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 400.0, h: 400.0 };
    const ROW_H: f32 = 26.0; // tree_row_height(Md)

    /// keys 0..7: root0(0)[a(1),b(2)], root1(3)[c(4)[d(5)]], root2(6). Pre-order collapsed visible = [0,3,6].
    fn sample() -> Vec<TreeNode<u32, &'static str>> {
        vec![
            TreeNode::branch(
                0,
                "root0",
                vec![TreeNode::leaf(1, "a"), TreeNode::leaf(2, "b")],
            ),
            TreeNode::branch(
                3,
                "root1",
                vec![TreeNode::branch(4, "c", vec![TreeNode::leaf(5, "d")])],
            ),
            TreeNode::leaf(6, "root2"),
        ]
    }

    fn sample_tree(
        expanded: RwSignal<HashSet<u32>>,
        sel: Option<RwSignal<TreeSelection<u32>>>,
    ) -> AnyElement {
        let mut t = tree(sample())
            .node(|n: &&'static str| text(*n))
            .expanded(expanded)
            .size(ControlSize::Md)
            .height(200.0);
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

    /// The core virtualized body node: root → child[0] body div → child[0] dyn_list → child[0] core.
    fn virt_core(h: &Harness) -> NodeId {
        let body = h.arena.get(h.root_id).unwrap().children[0];
        let list = h.arena.get(body).unwrap().children[0];
        h.arena.get(list).unwrap().children[0]
    }

    fn visible_row_count(h: &Harness) -> usize {
        h.arena.get(virt_core(h)).unwrap().children.len()
    }

    fn click_at(h: &mut Harness, x: f32, y: f32) {
        let mut dispatch = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(kind, Point::new(x, y), Modifiers::default());
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut dispatch);
        }
    }

    /// Clicks the row-body (far right, past the caret) of window row `win_i` (offset 0 → y = core.y + i*ROW_H).
    fn click_row_body(h: &mut Harness, win_i: usize) {
        let c = h.layout.absolute_layout(virt_core(h));
        click_at(
            h,
            c.origin.x + 200.0,
            c.origin.y + win_i as f32 * ROW_H + ROW_H / 2.0,
        );
    }

    /// Clicks the disclosure caret of a **depth-0** window row `win_i` (caret cell at x ∈ [0, indent)).
    fn click_root_caret(h: &mut Harness, win_i: usize) {
        let c = h.layout.absolute_layout(virt_core(h));
        click_at(
            h,
            c.origin.x + 8.0,
            c.origin.y + win_i as f32 * ROW_H + ROW_H / 2.0,
        );
    }

    fn press(h: &mut Harness, code: KeyCode) {
        let kev = KeyEvent::new(code, Modifiers::default(), true, false);
        dispatch_key(&mut h.root, &h.arena, Some(h.root_id), &kev);
    }

    #[test]
    fn tree_should_render_roots_and_expand_children() {
        let owner = Owner::new();
        owner.set();
        let expanded = RwSignal::new(HashSet::new());
        let mut h = harness(|| sample_tree(expanded, None));

        assert_eq!(
            visible_row_count(&h),
            3,
            "collapsed: only the 3 roots are visible"
        );
        // Expand root0 → its two children appear (3 → 5 visible).
        expanded.update(|s| {
            s.insert(0);
        });
        frame(&mut h);
        assert_eq!(
            visible_row_count(&h),
            5,
            "expanding root0 reveals its two children"
        );
        drop(owner);
    }

    #[test]
    fn tree_caret_click_should_toggle_expand() {
        let owner = Owner::new();
        owner.set();
        let expanded = RwSignal::new(HashSet::new());
        let mut h = harness(|| sample_tree(expanded, None));

        // Row 0 is root0 (a branch). Clicking its caret expands it.
        click_root_caret(&mut h, 0);
        assert!(
            expanded.with_untracked(|s| s.contains(&0)),
            "clicking a branch caret expands it"
        );
        frame(&mut h);
        click_root_caret(&mut h, 0);
        assert!(
            expanded.with_untracked(|s| !s.contains(&0)),
            "clicking again collapses it"
        );
        drop(owner);
    }

    #[test]
    fn tree_row_click_should_select() {
        let owner = Owner::new();
        owner.set();
        let expanded = RwSignal::new(HashSet::new());
        let selection = RwSignal::new(TreeSelection::none());
        let mut h = harness(|| sample_tree(expanded, Some(selection)));

        // Collapsed visible = [root0(0), root1(3), root2(6)]. Click row 2 → root2 (key 6).
        click_row_body(&mut h, 2);
        assert_eq!(
            selection.get_untracked().selected(),
            Some(6),
            "clicking a row selects its node key"
        );
        drop(owner);
    }

    #[test]
    fn tree_branch_row_click_should_toggle_and_select() {
        let owner = Owner::new();
        owner.set();
        let expanded = RwSignal::new(HashSet::new());
        let selection = RwSignal::new(TreeSelection::none());
        let mut h = harness(|| sample_tree(expanded, Some(selection)));

        // Clicking the BODY (not the caret) of a branch row toggles its expansion and selects it.
        click_row_body(&mut h, 0); // root0 (branch, key 0)
        assert!(
            expanded.with_untracked(|s| s.contains(&0)),
            "clicking a branch row body expands it"
        );
        assert_eq!(
            selection.get_untracked().selected(),
            Some(0),
            "clicking a branch row body also selects it"
        );
        frame(&mut h);
        click_row_body(&mut h, 0);
        assert!(
            expanded.with_untracked(|s| !s.contains(&0)),
            "clicking the branch row body again collapses it"
        );
        drop(owner);
    }

    #[test]
    fn tree_arrow_down_up_should_navigate() {
        let owner = Owner::new();
        owner.set();
        let expanded = RwSignal::new(HashSet::new());
        let selection = RwSignal::new(TreeSelection::none());
        let mut h = harness(|| sample_tree(expanded, Some(selection)));

        // Visible = [0, 3, 6]. Down (no cursor) → first (0); Down → 3; Up → 0.
        press(&mut h, KeyCode::ArrowDown);
        assert_eq!(selection.get_untracked().selected(), Some(0));
        press(&mut h, KeyCode::ArrowDown);
        assert_eq!(selection.get_untracked().selected(), Some(3));
        press(&mut h, KeyCode::ArrowUp);
        assert_eq!(selection.get_untracked().selected(), Some(0));
        drop(owner);
    }

    #[test]
    fn tree_arrow_right_left_should_expand_collapse() {
        let owner = Owner::new();
        owner.set();
        let expanded = RwSignal::new(HashSet::new());
        let selection = RwSignal::new(TreeSelection::single(0)); // root0 (a collapsed branch)
        let mut h = harness(|| sample_tree(expanded, Some(selection)));

        press(&mut h, KeyCode::ArrowRight);
        assert!(
            expanded.with_untracked(|s| s.contains(&0)),
            "Right on a collapsed branch expands it"
        );
        frame(&mut h);
        press(&mut h, KeyCode::ArrowLeft);
        assert!(
            expanded.with_untracked(|s| !s.contains(&0)),
            "Left on an expanded branch collapses it"
        );
        drop(owner);
    }

    #[test]
    fn tree_selected_should_highlight() {
        let owner = Owner::new();
        owner.set();
        let expanded = RwSignal::new(HashSet::new());
        let selection = RwSignal::new(TreeSelection::single(6)); // root2 (visible)
        let mut h = harness(|| sample_tree(expanded, Some(selection)));

        let accent = h.theme.resolve_color(ColorRole::Accent);
        let (_, scene) = frame(&mut h);
        assert!(
            scene
                .quads
                .iter()
                .any(|q| q.bg == kagari_render::Background::Solid(accent)),
            "the selected row paints an Accent background"
        );
        drop(owner);
    }

    #[test]
    fn tree_empty_should_render_nothing() {
        let owner = Owner::new();
        owner.set();
        let expanded = RwSignal::new(HashSet::new());
        let h = harness(|| {
            tree(Vec::<TreeNode<u32, &'static str>>::new())
                .node(|n: &&'static str| text(*n))
                .expanded(expanded)
                .height(200.0)
                .into_element()
        });
        assert_eq!(visible_row_count(&h), 0, "an empty tree renders no rows");
        drop(owner);
    }
}
