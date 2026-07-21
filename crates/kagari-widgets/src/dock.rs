//! The [`Dock`] widget (#239): a dockable workspace that renders the #222 dock substrate — a normalized
//! [`DockTree`] of split / tabbed / leaf regions — into live [`Splitter`](crate::Splitter)s,
//! [`Tabs`](crate::Tabs), and [`Panel`]s, with drag-to-dock re-arrangement. The app owns the
//! `RwSignal<DockTree>` (and its persistence via `PersistenceService`, #68); the widget owns the chrome,
//! the resize/tab bindings, and the drag wiring.
//!
//! ## Reactive model (the crux)
//! The workspace is rendered inside a **single keyed [`dyn_list`]** whose key is an **injective structural
//! projection** of the tree — the root cloned with every weight set to 1 and every `active` set to 0
//! (`structural_key`). So:
//! - a **structural** change (drag-dock tabify/split, remove) changes the key → the subtree rebuilds;
//! - a **resize / tab-switch** changes only weights/active → the key is unchanged → **no rebuild**. This is
//!   required, not just an optimization: the splitter divider drag holds a pointer *capture* on its element
//!   `NodeId`, and a mid-drag rebuild would mint a new `NodeId` and break the in-progress resize.
//!
//! Live resize/tab state lives in per-region signals (a `RwSignal<f32>` per divider, a `RwSignal<usize>`
//! per tab group) seeded from the tree and **written back** to the tree by guarded effects (so persistence
//! sees the change) without triggering a rebuild. The write-back uses the substrate's in-place
//! [`DockTree::set_weight_at`]/[`set_active_at`](DockTree::set_active_at) (no re-normalize). A guard skips
//! the write when the value already matches, so merely rendering the dock does not dirty the tree.
//!
//! **MVP limitation**: replacing `tree` at runtime with a *same-shape* tree whose weights/active differ is
//! not reflected until a structural change (the key excludes weights/active by design). Startup restore and
//! all user interaction are correct.

use std::rc::Rc;

use kagari_base::Axis;
use kagari_core::element::{BoundsHandle, dyn_list};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, create_effect, rx};
use kagari_core::{
    AnyElement, DockNode, DockTarget, DockTree, DropZone, IntoElement, PanelId, div, drop_zone,
};
use kagari_style::{ColorRole, Styled};

use crate::panel::Panel;
use crate::splitter::splitter;
use crate::tabs::tabs;

/// Minimum pane extent (logical px) enforced on every splitter divider in the dock.
const MIN_PANE_PX: f32 = 48.0;

/// A dockable workspace (#239). Build with [`dock`]; returns `impl IntoElement`. The app supplies the
/// controlled `RwSignal<DockTree>` and a `panels` closure mapping each [`PanelId`] to its [`Panel`].
pub struct Dock {
    tree: RwSignal<DockTree>,
    panels: Rc<dyn Fn(PanelId) -> Panel>,
}

/// Creates a dock over `tree`, mapping each docked [`PanelId`] to a [`Panel`] via `panels`. The app owns
/// `tree` (default layout + persistence, #68); resize / tab-switch / drag-dock all write back into it.
pub fn dock(tree: RwSignal<DockTree>, panels: impl Fn(PanelId) -> Panel + 'static) -> Dock {
    Dock {
        tree,
        panels: Rc::new(panels),
    }
}

impl IntoElement for Dock {
    fn into_element(self) -> AnyElement {
        let Dock { tree, panels } = self;
        let list = {
            let panels = panels.clone();
            dyn_list(
                // Subscribe to the tree; the key is the structural projection (weights/active excluded), so
                // only a structural change rebuilds. NOTE: a non-structural write (resize/tab-switch
                // write-back) still re-runs this items_fn — cloning the structural key and waking a gated
                // whole-tree reconcile pass each drag frame — but the key is unchanged so no subtree
                // rebuilds. Bounded for realistic docks; a `structure_version` key that only bumps on
                // structural ops would avoid the per-frame clone + reconcile walk (post-MVP optimization).
                move || vec![tree.with(structural_key)],
                |k: &Option<DockNode>| k.clone(),
                move |_key| render_root(tree, &panels),
            )
            // Fill the workspace: the reactive node must stretch so the region tree below can fill too.
            .grow()
        };
        // The outer container fills the slot the app gives it and is a Root drop target: it catches a drop
        // that no region consumed (an empty workspace seeds its first panel; otherwise a Center append).
        // Regions (deeper hits) win when the pointer is over one.
        div()
            .flex_col()
            .flex_grow(1.0)
            .min_w(0.0)
            .min_h(0.0)
            .drop_target::<PanelId>(move |moving, _cx| {
                tree.update(|t| t.dock(DockTarget::Root, DropZone::Center, moving));
            })
            .child(list)
            .into_element()
    }
}

/// Renders the current tree root (read untracked — the `dyn_list` key already subscribed).
fn render_root(tree: RwSignal<DockTree>, panels: &Rc<dyn Fn(PanelId) -> Panel>) -> AnyElement {
    tree.with_untracked(|t| match t.root() {
        Some(node) => render_node(node, &[], tree, panels),
        None => div().flex_col().into_element(),
    })
}

/// Renders one node at `path` (its child-index path from the root, used by the write-back effects).
fn render_node(
    node: &DockNode,
    path: &[usize],
    tree: RwSignal<DockTree>,
    panels: &Rc<dyn Fn(PanelId) -> Panel>,
) -> AnyElement {
    match node {
        DockNode::Leaf(id) => {
            let id = *id;
            // A leaf is the full Panel; its header is the drag handle (drag the title bar to re-dock).
            let panel = (panels)(id).drag_handle(id).into_element();
            region(panel, DockTarget::Panel(id), tree)
        }
        DockNode::Tabs {
            panels: ids,
            active,
        } => {
            let sel = RwSignal::new((*active).min(ids.len().saturating_sub(1)));
            install_active_writeback(sel, path.to_vec(), tree);
            let mut group = tabs(sel);
            for &id in ids {
                let label = (panels)(id).title();
                let panels = panels.clone();
                // Tab content is the panel BODY only — the tab bar is the chrome (no redundant header).
                group = group.tab(label, move || (panels)(id).into_body());
            }
            let anchor = ids.first().copied().unwrap_or(PanelId::from_raw(0));
            region(group.into_element(), DockTarget::Panel(anchor), tree)
        }
        DockNode::Split { axis, children } => {
            // A normalized Split always has ≥ 2 children (normalize collapses empty/single-child splits),
            // so `build_nested`'s `children[i]` is always in range. Guard the degenerate case anyway — a
            // hand-built non-normalized node must render gracefully, not panic (rules/rust.md: no panic
            // outside tests).
            if children.len() < 2 {
                return match children.first() {
                    Some(only) => render_node(&only.node, &child_path(path, 0), tree, panels),
                    None => div().flex_col().into_element(),
                };
            }
            let n = children.len();
            let weights: Vec<f32> = children.iter().map(|c| c.weight).collect();
            // One RwSignal<f32> per divider (n-1), seeded from the weights.
            let ratios: Vec<RwSignal<f32>> = (0..n.saturating_sub(1))
                .map(|i| RwSignal::new(seed_ratio(&weights, i)))
                .collect();
            if !ratios.is_empty() {
                install_weight_writeback(ratios.clone(), path.to_vec(), tree);
            }
            build_nested(*axis, children, &ratios, 0, path, tree, panels)
        }
        // `DockNode` is #[non_exhaustive] (a floating/detached-window variant is post-MVP); render nothing
        // for a future variant until this widget learns to draw it.
        _ => div().flex_col().into_element(),
    }
}

/// Folds an N-child `Split` into right-leaning nested binary [`Splitter`](crate::Splitter)s: divider `i`
/// splits `children[i]` against the group `children[i+1..]`, bound to `ratios[i]`.
fn build_nested(
    axis: Axis,
    children: &[kagari_core::DockChild],
    ratios: &[RwSignal<f32>],
    i: usize,
    path: &[usize],
    tree: RwSignal<DockTree>,
    panels: &Rc<dyn Fn(PanelId) -> Panel>,
) -> AnyElement {
    // The last child fills the tail; every child sits at `path + [child index]` in the flat tree.
    if i + 1 >= children.len() {
        return render_node(&children[i].node, &child_path(path, i), tree, panels);
    }
    let leading = render_node(&children[i].node, &child_path(path, i), tree, panels);
    let trailing = build_nested(axis, children, ratios, i + 1, path, tree, panels);
    let s = splitter(ratios[i]);
    let s = match axis {
        Axis::Horizontal => s.horizontal(),
        Axis::Vertical => s.vertical(),
    };
    s.min(MIN_PANE_PX)
        .pane(leading)
        .pane(trailing)
        .into_element()
}

/// Wraps a region element with a drop target (drag-to-dock) + a coarse hover affordance (the region border
/// tints while a compatible drag hovers) + a [`BoundsHandle`] so the drop handler can resolve its zone.
fn region(inner: AnyElement, target: DockTarget, tree: RwSignal<DockTree>) -> AnyElement {
    let bounds = BoundsHandle::new();
    let hover: RwSignal<Option<DropZone>> = RwSignal::new(None);
    let (b_drop, b_over) = (bounds.clone(), bounds.clone());
    div()
        .flex_col()
        .flex_grow(1.0)
        .min_w(0.0)
        .min_h(0.0)
        // Confine the panel's content to this region so a small pane's text doesn't bleed over its
        // neighbours (regions are transparent, so overflow would otherwise draw across panels).
        .clip()
        .track_bounds(&bounds)
        .border_w_1()
        .border_color(rx(move || hover.get().map(|_| ColorRole::Accent)))
        .drop_target::<PanelId>(move |moving, cx| {
            if let Some(p) = cx.drag_pointer() {
                let zone = drop_zone(b_drop.get(), p);
                tree.update(|t| t.dock(target, zone, moving));
            }
            hover.set(None);
        })
        .on_drag_over(move |cx| {
            if let Some(p) = cx.drag_pointer() {
                hover.set(Some(drop_zone(b_over.get(), p)));
            }
        })
        .on_drag_leave(move |_cx| hover.set(None))
        .child(inner)
        .into_element()
}

/// Installs the guarded weight write-back for a split at `path`: whenever any divider ratio changes, derive
/// the full relative-weight vector and write it into the tree in place — but skip when it already matches
/// (so the effect's eager first run on mount does not dirty the tree).
fn install_weight_writeback(
    ratios: Vec<RwSignal<f32>>,
    path: Vec<usize>,
    tree: RwSignal<DockTree>,
) {
    create_effect(move || {
        let rs: Vec<f32> = ratios.iter().map(|r| sane(r.get())).collect();
        let fracs = ratios_to_weights(&rs);
        let write = tree.with_untracked(|t| {
            weights_at(t.root(), &path)
                .map(|cur| !weights_approx_eq(&normalize_weights(&cur), &fracs))
                .unwrap_or(false)
        });
        if write {
            tree.update(|t| t.set_weight_at(&path, &fracs));
        }
    });
}

/// Installs the guarded active-tab write-back for a tab group at `path` (see [`install_weight_writeback`]).
fn install_active_writeback(sel: RwSignal<usize>, path: Vec<usize>, tree: RwSignal<DockTree>) {
    create_effect(move || {
        let a = sel.get();
        let write = tree.with_untracked(|t| active_at(t.root(), &path) != Some(a));
        if write {
            tree.update(|t| t.set_active_at(&path, a));
        }
    });
}

// --- Pure helpers (structure projection, path walking, weight math) ---

/// The injective structural key: the root cloned with every weight set to 1 and every `active` set to 0, so
/// two trees compare equal iff their *shape* (regions, panel ids, tab order, axes) matches — weight/active
/// differences do not change the key (and so do not rebuild).
fn structural_key(t: &DockTree) -> Option<DockNode> {
    t.root().map(strip)
}

fn strip(node: &DockNode) -> DockNode {
    match node {
        DockNode::Leaf(id) => DockNode::Leaf(*id),
        DockNode::Tabs { panels, .. } => DockNode::Tabs {
            panels: panels.clone(),
            active: 0,
        },
        DockNode::Split { axis, children } => DockNode::Split {
            axis: *axis,
            children: children
                .iter()
                .map(|c| kagari_core::DockChild {
                    node: strip(&c.node),
                    weight: 1.0,
                })
                .collect(),
        },
        // Future (non_exhaustive) variant: keep it whole so any change to it is conservatively a rebuild.
        other => other.clone(),
    }
}

/// The node at `path` (child-index path from `root`), or `None` if the path leaves a non-`Split`.
fn node_at<'a>(root: &'a DockNode, path: &[usize]) -> Option<&'a DockNode> {
    let mut node = root;
    for &i in path {
        match node {
            DockNode::Split { children, .. } => node = &children.get(i)?.node,
            _ => return None,
        }
    }
    Some(node)
}

/// The child weights of the `Split` at `path`, if it is a split.
fn weights_at(root: Option<&DockNode>, path: &[usize]) -> Option<Vec<f32>> {
    match node_at(root?, path)? {
        DockNode::Split { children, .. } => Some(children.iter().map(|c| c.weight).collect()),
        _ => None,
    }
}

/// The active tab index of the `Tabs` at `path`, if it is a tab group.
fn active_at(root: Option<&DockNode>, path: &[usize]) -> Option<usize> {
    match node_at(root?, path)? {
        DockNode::Tabs { active, .. } => Some(*active),
        _ => None,
    }
}

/// `path` extended by child index `i`.
fn child_path(path: &[usize], i: usize) -> Vec<usize> {
    let mut p = path.to_vec();
    p.push(i);
    p
}

/// The seed ratio for divider `i`: `w[i] / sum(w[i..])`, guarded (a zero/degenerate remainder → 0.5).
fn seed_ratio(weights: &[f32], i: usize) -> f32 {
    let remaining: f32 = weights[i..].iter().copied().map(sane_weight).sum();
    if remaining > 0.0 {
        (sane_weight(weights[i]) / remaining).clamp(0.0, 1.0)
    } else {
        0.5
    }
}

/// Converts `n-1` nested-splitter ratios into `n` relative weights summing to 1 (each floored to a tiny
/// positive epsilon so a collapsed pane never round-trips to a degenerate weight).
fn ratios_to_weights(ratios: &[f32]) -> Vec<f32> {
    let mut fracs = Vec::with_capacity(ratios.len() + 1);
    let mut remaining = 1.0_f32;
    for &r in ratios {
        fracs.push((remaining * r).max(1e-4));
        remaining *= 1.0 - r;
    }
    fracs.push(remaining.max(1e-4));
    fracs
}

/// Normalizes weights to sum 1 (a zero/degenerate sum → equal shares) for comparison against derived fracs.
fn normalize_weights(w: &[f32]) -> Vec<f32> {
    let sum: f32 = w.iter().copied().map(sane_weight).sum();
    if sum > 0.0 {
        w.iter().map(|x| sane_weight(*x) / sum).collect()
    } else {
        vec![1.0 / w.len().max(1) as f32; w.len()]
    }
}

/// Whether two weight vectors match within a small tolerance (guards the write-back against no-op writes).
fn weights_approx_eq(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3)
}

/// A ratio coerced to a usable `[0, 1]` value (a non-finite ratio → 0.5); mirrors the splitter's own guard.
fn sane(r: f32) -> f32 {
    if r.is_finite() {
        r.clamp(0.0, 1.0)
    } else {
        0.5
    }
}

/// A weight coerced to a usable positive value (a non-finite/`<= 0` weight → 1.0).
fn sane_weight(w: f32) -> f32 {
    if w.is_finite() && w > 0.0 { w } else { 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    use kagari_base::{Color, NodeId, Point, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::FocusRegistry;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{
        DispatchState, DockChild, HitTest, Modifiers, MouseButton, MouseEvent, MouseKind,
        dispatch_drop, dispatch_mouse,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    use crate::panel::panel;

    const VIEWPORT: BSize = BSize { w: 400.0, h: 400.0 };

    fn p(n: u64) -> PanelId {
        PanelId::from_raw(n)
    }

    /// Wraps a dock in a viewport-sized flex slot so it fills (as an app would place it), then builds it.
    fn docked(tree: RwSignal<DockTree>) -> AnyElement {
        div()
            .flex()
            .size(VIEWPORT)
            .child(dock(tree, demo_panels()))
            .into_element()
    }

    /// A demo panel whose body is a distinctively colored quad, so the rendered leaf is checkable in scene.
    fn demo_panels() -> impl Fn(PanelId) -> Panel {
        |id: PanelId| {
            let c = 0.1 * id.raw() as f32;
            panel(format!("P{}", id.raw()), move || {
                div()
                    .size(BSize { w: 80.0, h: 40.0 })
                    .background(Background::Solid(Color::new(c, 1.0 - c, 0.5, 1.0)))
            })
        }
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

    fn harness(root: AnyElement) -> Harness {
        let focus = FocusRegistry::new();
        provide_context(focus.clone());
        let mut h = Harness {
            root,
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            focus,
            damage: StdArc::new(DamageState::default()),
            theme: Theme::light(),
            root_id: NodeId::from_raw(0),
        };
        let id = frame(&mut h);
        h.root_id = id;
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

    /// Counts the descendant nodes of `root_id` matching `pred` (a tiny structural probe over the arena).
    fn count_nodes(h: &Harness, pred: impl Fn(&kagari_core::arena::Node) -> bool + Copy) -> usize {
        fn walk(
            arena: &Arena,
            id: NodeId,
            pred: &dyn Fn(&kagari_core::arena::Node) -> bool,
            acc: &mut usize,
        ) {
            if let Some(node) = arena.get(id) {
                if pred(node) {
                    *acc += 1;
                }
                for &c in &node.children {
                    walk(arena, c, pred, acc);
                }
            }
        }
        let mut acc = 0;
        walk(&h.arena, h.root_id, &pred, &mut acc);
        acc
    }

    #[test]
    fn dock_should_render_region_tree() {
        let owner = Owner::new();
        owner.set();
        // Split[ Leaf(1), Tabs(2,3) ] renders without panic and mounts nodes for each region.
        let tree = RwSignal::new(DockTree::new(DockNode::Split {
            axis: Axis::Horizontal,
            children: vec![
                DockChild {
                    node: DockNode::Leaf(p(1)),
                    weight: 1.0,
                },
                DockChild {
                    node: DockNode::Tabs {
                        panels: vec![p(2), p(3)],
                        active: 0,
                    },
                    weight: 1.0,
                },
            ],
        }));
        let h = harness(docked(tree));
        assert!(
            count_nodes(&h, |n| !n.children.is_empty()) > 3,
            "the region tree mounts a non-trivial element tree"
        );
        drop(owner);
    }

    #[test]
    fn dock_empty_should_render_nothing() {
        let owner = Owner::new();
        owner.set();
        let tree = RwSignal::new(DockTree::empty());
        let mut h = harness(docked(tree));
        // No panel bodies (no demo colored quads) when the workspace is empty.
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
        assert!(
            !scene
                .quads
                .iter()
                .any(|q| matches!(q.bg, Background::Solid(c) if c.a > 0.0 && c != Color::new(0.0, 0.0, 0.0, 0.0))),
            "an empty dock renders no panel bodies"
        );
        drop(owner);
    }

    #[test]
    fn dock_leaf_drag_should_redock_panel() {
        let owner = Owner::new();
        owner.set();
        // Two side-by-side leaves; drag panel 2 onto panel 1's CENTER band → they tabify.
        let tree = RwSignal::new(DockTree::new(DockNode::Split {
            axis: Axis::Horizontal,
            children: vec![
                DockChild {
                    node: DockNode::Leaf(p(1)),
                    weight: 1.0,
                },
                DockChild {
                    node: DockNode::Leaf(p(2)),
                    weight: 1.0,
                },
            ],
        }));
        let mut h = harness(docked(tree));
        // Begin a drag from panel 2's header (a drag source), then drop at the center of panel 1's region.
        // Drop PanelId(2) at the CENTER band of panel 1's region (the left half of the viewport), carrying
        // the pointer — the region resolves the zone via drop_zone(bounds, pointer) and docks through #222.
        let center = Point::new(VIEWPORT.w * 0.25, VIEWPORT.h * 0.5);
        let target = h
            .hit
            .pick_where(center, |f| f.drop_target)
            .expect("a region drop target under the pointer");
        let consumed = dispatch_drop(&mut h.root, &h.arena, target, Box::new(p(2)), center);
        assert!(consumed, "the region consumed the PanelId drop");
        assert!(
            matches!(tree.get_untracked().root(), Some(DockNode::Tabs { .. })),
            "a center-band drop tabifies the two leaves: {:?}",
            tree.get_untracked().root()
        );
        drop(owner);
    }

    #[test]
    fn dock_leaf_drag_edge_should_split() {
        let owner = Owner::new();
        owner.set();
        let tree = RwSignal::new(DockTree::leaf(p(1)));
        let mut h = harness(docked(tree));
        // Drop panel 2 on the LEFT edge band of the single leaf → a horizontal split.
        let left_edge = Point::new(VIEWPORT.w * 0.05, VIEWPORT.h * 0.5);
        let target = h
            .hit
            .pick_where(left_edge, |f| f.drop_target)
            .expect("a drop target under the pointer");
        dispatch_drop(&mut h.root, &h.arena, target, Box::new(p(2)), left_edge);
        assert!(
            matches!(tree.get_untracked().root(), Some(DockNode::Split { .. })),
            "an edge-band drop splits the region: {:?}",
            tree.get_untracked().root()
        );
        drop(owner);
    }

    #[test]
    fn dock_splitter_resize_should_write_back_weight_without_rebuild() {
        let owner = Owner::new();
        owner.set();
        let tree = RwSignal::new(DockTree::new(DockNode::Split {
            axis: Axis::Horizontal,
            children: vec![
                DockChild {
                    node: DockNode::Leaf(p(1)),
                    weight: 1.0,
                },
                DockChild {
                    node: DockNode::Leaf(p(2)),
                    weight: 1.0,
                },
            ],
        }));
        let mut h = harness(docked(tree));
        let before = count_nodes(&h, |_| true);
        // The dock built one divider ratio signal seeded at 0.5; the mount write-back must NOT have dirtied
        // the (normalized-equal) weights, and the structure key is unchanged so no rebuild happens.
        // We can't reach the private ratio signal, so drive the substrate directly and confirm the key is
        // stable: set_weight_at is non-structural, so re-rendering keeps the same node count.
        tree.update(|t| t.set_weight_at(&[], &[3.0, 1.0]));
        h.root_id = frame(&mut h);
        let after = count_nodes(&h, |_| true);
        assert_eq!(
            before, after,
            "a weight change does not rebuild the region tree (structural key unchanged)"
        );
        if let Some(DockNode::Split { children, .. }) = tree.get_untracked().root() {
            assert!(
                (children[0].weight - 3.0).abs() < 1e-3,
                "the weight write-back reached the tree"
            );
        } else {
            panic!("still a split");
        }
        drop(owner);
    }

    #[test]
    fn dock_divider_drag_should_resize_via_ratio_and_write_back() {
        let owner = Owner::new();
        owner.set();
        // Two side-by-side leaves (equal weights) filling 400px wide → the divider sits near x=200.
        let tree = RwSignal::new(DockTree::new(DockNode::Split {
            axis: Axis::Horizontal,
            children: vec![
                DockChild {
                    node: DockNode::Leaf(p(1)),
                    weight: 1.0,
                },
                DockChild {
                    node: DockNode::Leaf(p(2)),
                    weight: 1.0,
                },
            ],
        }));
        let mut h = harness(docked(tree));
        // The left pane wrapper: wrapper → dock-outer → dyn_list → splitter-root → [pane_a, divider, pane_b].
        let nth = |h: &Harness, id: NodeId, i: usize| h.arena.get(id).unwrap().children[i];
        let splitter_root = nth(&h, nth(&h, nth(&h, h.root_id, 0), 0), 0);
        let pane_a = nth(&h, splitter_root, 0);
        let w_before = h.layout.absolute_layout(pane_a).size.w;

        // Drag the divider (center of the viewport) to the right; pointer capture routes the moves to it.
        let mut st = DispatchState::default();
        let seq = [
            (MouseKind::Down(MouseButton::Left), Point::new(200.0, 200.0)),
            (MouseKind::Move, Point::new(280.0, 200.0)),
            (MouseKind::Up(MouseButton::Left), Point::new(280.0, 200.0)),
        ];
        for (kind, pos) in seq {
            let ev = MouseEvent::new(kind, pos, Modifiers::default());
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut st);
        }
        // The drag moved the ratio, which the write-back mirrored into the tree: pane 1 now outweighs pane 2.
        if let Some(DockNode::Split { children, .. }) = tree.get_untracked().root() {
            assert!(
                children[0].weight > children[1].weight,
                "dragging the divider right grows the left pane's weight: {:?}",
                children.iter().map(|c| c.weight).collect::<Vec<_>>()
            );
        } else {
            panic!("still a split");
        }
        // The VISUAL resize: re-render (relayout from the new ratio) and confirm the left pane grew wider.
        h.root_id = frame(&mut h);
        let w_after = h.layout.absolute_layout(pane_a).size.w;
        assert!(
            w_after > w_before + 10.0,
            "the left pane visibly widens after the drag ({w_before} -> {w_after})"
        );
        drop(owner);
    }

    #[test]
    fn dock_nested_split_should_fill_after_relayout() {
        let owner = Owner::new();
        owner.set();
        // Split[ Leaf(1) | Split-v[ Leaf(2), Tabs(3,4) ] ] — the nested vertical split must fill its column
        // (Leaf2 region on top, Tabs region well below), and stay filled across a second frame (the
        // request_layout recurse path). Regressed once: the inner panes collapsed to content height.
        let tree = RwSignal::new(DockTree::new(DockNode::Split {
            axis: Axis::Horizontal,
            children: vec![
                DockChild {
                    node: DockNode::Leaf(p(1)),
                    weight: 1.0,
                },
                DockChild {
                    node: DockNode::Split {
                        axis: Axis::Vertical,
                        children: vec![
                            DockChild {
                                node: DockNode::Leaf(p(2)),
                                weight: 1.0,
                            },
                            DockChild {
                                node: DockNode::Tabs {
                                    panels: vec![p(3), p(4)],
                                    active: 0,
                                },
                                weight: 1.0,
                            },
                        ],
                    },
                    weight: 3.0,
                },
            ],
        }));
        let mut h = harness(docked(tree));
        h.root_id = frame(&mut h); // a second frame exercises the request_layout recurse path
        let nth = |h: &Harness, id: NodeId, i: usize| h.arena.get(id).unwrap().children[i];
        // wrapper → dock-outer → dyn_list → outer-splitter → [pane_a, divider, pane_b(inner-splitter)].
        let outer = nth(&h, nth(&h, nth(&h, h.root_id, 0), 0), 0);
        let inner = nth(&h, outer, 2); // pane_b wrapper
        // inner-splitter is the pane_b wrapper's child; its panes are [Leaf2-pane, divider, Tabs-pane].
        let inner_splitter = nth(&h, inner, 0);
        let tabs_pane = nth(&h, inner_splitter, 2);
        let tabs_y = h.layout.absolute_layout(tabs_pane).origin.y;
        assert!(
            tabs_y > 200.0,
            "the nested Tabs region fills the lower half (origin.y={tabs_y}), not collapsed to the top"
        );
        drop(owner);
    }

    #[test]
    fn dock_divider_drag_should_respect_min_pane() {
        let owner = Owner::new();
        owner.set();
        let tree = RwSignal::new(DockTree::new(DockNode::Split {
            axis: Axis::Horizontal,
            children: vec![
                DockChild {
                    node: DockNode::Leaf(p(1)),
                    weight: 1.0,
                },
                DockChild {
                    node: DockNode::Leaf(p(2)),
                    weight: 1.0,
                },
            ],
        }));
        let mut h = harness(docked(tree));
        let nth = |h: &Harness, id: NodeId, i: usize| h.arena.get(id).unwrap().children[i];
        let splitter_root = nth(&h, nth(&h, nth(&h, h.root_id, 0), 0), 0);
        let pane_b = nth(&h, splitter_root, 2);
        // Drag the divider WAY past the right edge (pointer capture delivers moves beyond the window), as
        // a user dragging hard against the edge would — the min clamp must still hold.
        let mut st = DispatchState::default();
        for (kind, pos) in [
            (MouseKind::Down(MouseButton::Left), Point::new(200.0, 200.0)),
            (MouseKind::Move, Point::new(5000.0, 200.0)),
            (MouseKind::Up(MouseButton::Left), Point::new(5000.0, 200.0)),
        ] {
            let ev = MouseEvent::new(kind, pos, Modifiers::default());
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut st);
        }
        h.root_id = frame(&mut h);
        let w = h.layout.absolute_layout(pane_b).size.w;
        assert!(
            w >= MIN_PANE_PX - 2.0,
            "the right pane stays at the min ({MIN_PANE_PX}px) even dragging far past the edge (w={w})"
        );
        drop(owner);
    }

    #[test]
    fn dock_mount_should_not_rewrite_weights() {
        let owner = Owner::new();
        owner.set();
        // Merely building/rendering the dock must NOT dirty the tree: the write-back effect's eager mount
        // run is guarded, so weights [2,1] stay [2,1] (not renormalized to [0.667,0.333]) — else opening a
        // dock would spuriously mark the app's persistence dirty every launch.
        let tree = RwSignal::new(DockTree::new(DockNode::Split {
            axis: Axis::Horizontal,
            children: vec![
                DockChild {
                    node: DockNode::Leaf(p(1)),
                    weight: 2.0,
                },
                DockChild {
                    node: DockNode::Leaf(p(2)),
                    weight: 1.0,
                },
            ],
        }));
        let _h = harness(docked(tree));
        if let Some(DockNode::Split { children, .. }) = tree.get_untracked().root() {
            assert_eq!(
                children[0].weight, 2.0,
                "mount does not rewrite the first weight"
            );
            assert_eq!(
                children[1].weight, 1.0,
                "mount does not rewrite the second weight"
            );
        } else {
            panic!("still a split");
        }
        drop(owner);
    }

    #[test]
    fn dock_tab_click_should_write_back_active() {
        let owner = Owner::new();
        owner.set();
        // A tab group as the root; clicking the second tab must write `active = 1` back into the tree
        // (and mount must leave active at its seed 0 — the active write-back guard).
        let tree = RwSignal::new(DockTree::new(DockNode::Tabs {
            panels: vec![p(1), p(2)],
            active: 0,
        }));
        let mut h = harness(docked(tree));
        assert_eq!(
            active_at(tree.get_untracked().root(), &[]),
            Some(0),
            "mount keeps the seeded active"
        );
        // wrapper → dock-outer → dyn_list → region → tabs-root → strip → tab[1].
        let nth = |h: &Harness, id: NodeId, i: usize| h.arena.get(id).unwrap().children[i];
        let region = nth(&h, nth(&h, nth(&h, h.root_id, 0), 0), 0);
        let tabs_root = nth(&h, region, 0);
        let strip = nth(&h, tabs_root, 0);
        let tab1 = nth(&h, strip, 1);
        // Click the second tab.
        let r = h.layout.absolute_layout(tab1);
        let center = Point::new(r.origin.x + r.size.w / 2.0, r.origin.y + r.size.h / 2.0);
        let mut st = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(kind, center, Modifiers::default());
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut st);
        }
        h.root_id = frame(&mut h);
        assert_eq!(
            active_at(tree.get_untracked().root(), &[]),
            Some(1),
            "clicking the second tab writes active=1 back into the tree"
        );
        // Non-structural: the tab group did not rebuild (still a Tabs, not collapsed/re-shaped).
        assert!(matches!(
            tree.get_untracked().root(),
            Some(DockNode::Tabs { .. })
        ));
        drop(owner);
    }

    #[test]
    fn dock_empty_root_drop_should_seed_first_panel() {
        let owner = Owner::new();
        owner.set();
        let tree = RwSignal::new(DockTree::empty());
        let mut h = harness(docked(tree));
        // With no regions, a drop lands on the outer container's Root drop target and seeds the panel.
        let center = Point::new(VIEWPORT.w * 0.5, VIEWPORT.h * 0.5);
        let target = h
            .hit
            .pick_where(center, |f| f.drop_target)
            .expect("the outer Root drop target");
        dispatch_drop(&mut h.root, &h.arena, target, Box::new(p(9)), center);
        assert_eq!(
            tree.get_untracked().root(),
            Some(&DockNode::Leaf(p(9))),
            "dropping onto an empty dock seeds the first panel"
        );
        drop(owner);
    }

    #[test]
    fn nway_weights_should_round_trip_through_ratios() {
        // A 3-child split: seeding ratios from weights and converting back reproduces the normalized
        // weights (so an N-way resize persists stably). Also covers the pure weight helpers.
        let weights = [1.0f32, 1.0, 2.0];
        let ratios: Vec<f32> = (0..2).map(|i| seed_ratio(&weights, i)).collect();
        let back = ratios_to_weights(&ratios);
        assert!(
            weights_approx_eq(&back, &normalize_weights(&weights)),
            "N-way weights round-trip: {back:?} vs {:?}",
            normalize_weights(&weights)
        );
        // Every derived weight is strictly positive (a collapsed pane floors to epsilon, never 0).
        assert!(ratios_to_weights(&[0.0, 0.0]).iter().all(|&x| x > 0.0));
        // Degenerate/NaN inputs stay finite and in range (RK-045 defensive sanitizing).
        assert!(seed_ratio(&[f32::NAN, 1.0], 0).is_finite());
        assert_eq!(sane(f32::NAN), 0.5);
        assert_eq!(sane_weight(-1.0), 1.0);
    }

    #[test]
    fn dock_layout_should_persist() {
        // The layout persists/restores via the generic PersistenceService (#68): a DockTree round-trips
        // through set/get unchanged (app-owned wiring; the widget only reads/writes the RwSignal).
        let service = kagari_core::PersistenceService::in_memory();
        let tree = DockTree::new(DockNode::Split {
            axis: Axis::Horizontal,
            children: vec![
                DockChild {
                    node: DockNode::Tabs {
                        panels: vec![p(1), p(2)],
                        active: 1,
                    },
                    weight: 2.0,
                },
                DockChild {
                    node: DockNode::Leaf(p(3)),
                    weight: 1.0,
                },
            ],
        });
        service
            .set("dock_layout", &tree)
            .expect("persist the layout");
        let restored: Option<DockTree> = service.get("dock_layout").expect("read the layout");
        assert_eq!(
            restored,
            Some(tree),
            "the dock layout round-trips through the persistence service"
        );
    }

    #[test]
    fn dock_structural_change_should_rebuild() {
        let owner = Owner::new();
        owner.set();
        let tree = RwSignal::new(DockTree::leaf(p(1)));
        let mut h = harness(docked(tree));
        let before = count_nodes(&h, |_| true);
        // A structural dock (tabify a second panel) changes the structural key → the subtree rebuilds bigger.
        tree.update(|t| t.dock(DockTarget::Panel(p(1)), DropZone::Center, p(2)));
        h.root_id = frame(&mut h);
        let after = count_nodes(&h, |_| true);
        assert!(
            after > before,
            "a structural change rebuilds into a larger tree ({before} -> {after})"
        );
        assert!(
            matches!(tree.get_untracked().root(), Some(DockNode::Tabs { .. })),
            "the tree tabified"
        );
        drop(owner);
    }
}
