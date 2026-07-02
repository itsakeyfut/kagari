//! Fine-grained damage tracking (#35): classify dirty nodes as layout vs paint, drive relayout
//! of only the dirty subtrees, and compute a union damage rect.
//!
//! A reactive prop's effect (#31/#29) is a **cheap marker** (ADR 0001 / §1.4): when it fires it
//! flags its node here by damage kind, then the frame loop ([`render_tree`](crate::paint::render_tree))
//! drains the markers under `&mut` access:
//! - **layout-dirty** (size/position) → [`LayoutTree::mark_dirty`], which relays out the node and
//!   its ancestors; clean sibling subtrees keep their cached layout (taffy incremental).
//! - **paint-dirty** (appearance only, e.g. `bg`) → repaint only, no relayout.
//!
//! The **damage rect** (union of dirty nodes' absolute bounds) is retained for a future partial
//! redraw; the GPU repaints the whole window for now (§1.4).

use std::sync::Mutex;

use kagari_base::{NodeId, Rect};
use kagari_layout::LayoutTree;

use crate::arena::Arena;
use crate::element::DamageSink;

/// Shared damage state: the set of dirty nodes by kind, flagged by reactive-prop effects and
/// drained by the frame loop. Lives behind an [`Arc`](std::sync::Arc) so effects (which are
/// `'static`) can hold a handle; the interior [`Mutex`] keeps it `Send + Sync` for [`DamageSink`].
#[derive(Default)]
pub struct DamageState {
    inner: Mutex<DamageInner>,
}

#[derive(Default)]
struct DamageInner {
    /// Nodes needing relayout (size/position changed).
    layout: Vec<NodeId>,
    /// Nodes needing repaint only (appearance changed).
    paint: Vec<NodeId>,
    /// A whole-tree repaint is pending (e.g. a theme swap, #43): every token must re-resolve. Not
    /// tied to specific nodes — the layout reskin happens as elements re-apply their resolved style
    /// during the next `request_layout`; this flag only wakes the loop and drives a full repaint.
    full: bool,
}

impl DamageSink for DamageState {
    fn mark_paint_dirty(&self, id: NodeId) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.paint.push(id);
        }
    }

    fn mark_layout_dirty(&self, id: NodeId) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.layout.push(id);
        }
    }
}

impl DamageState {
    /// Whether any node is dirty this frame (the scheduler, #36, polls this to decide whether to
    /// redraw at all). True while a full repaint ([`mark_all_dirty`](Self::mark_all_dirty)) is pending.
    pub fn is_dirty(&self) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.full || !inner.layout.is_empty() || !inner.paint.is_empty())
            .unwrap_or(false)
    }

    /// Flags a full repaint: every token must re-resolve (e.g. a theme swap, #43). Drives the next
    /// frame to wake and repaint the whole window (§1.4); the layout reskin follows from elements
    /// re-applying their resolved style on the next `request_layout`. Cleared by [`clear`](Self::clear).
    pub fn mark_all_dirty(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.full = true;
        }
    }

    /// Drains and returns the layout-dirty nodes. The frame loop feeds these to
    /// [`LayoutTree::mark_dirty`] before recomputing layout.
    pub fn take_layout_dirty(&self) -> Vec<NodeId> {
        match self.inner.lock() {
            Ok(mut inner) => std::mem::take(&mut inner.layout),
            Err(_) => Vec::new(),
        }
    }

    /// The union of every dirty node's **absolute** bounds, or `None` when there is nothing to
    /// redraw (no dirty node, or every dirty node resolves to an empty rect — e.g. a removed
    /// NodeId or one not yet laid out).
    ///
    /// taffy layout coords are parent-relative (RK-007), so each node's bounds are resolved to
    /// absolute by accumulating its ancestors' origins via the arena's parent pointers.
    ///
    /// A partial-redraw consumer must read this *before* the frame loop drains layout-dirty (see
    /// [`take_layout_dirty`](Self::take_layout_dirty)) and clears damage, otherwise the moved /
    /// resized regions are gone from the set. Today the GPU repaints the whole window (§1.4), so
    /// nothing consumes it yet.
    ///
    /// A pending full repaint ([`mark_all_dirty`](Self::mark_all_dirty)) is **not** reflected here —
    /// it tracks no specific nodes. A partial-redraw consumer must treat `is_dirty()` being true with
    /// an empty/partial `damage_rect` as the whole viewport.
    pub fn damage_rect(&self, arena: &Arena, layout: &LayoutTree) -> Option<Rect> {
        let inner = self.inner.lock().ok()?;
        let union = inner
            .layout
            .iter()
            .chain(inner.paint.iter())
            .fold(Rect::default(), |acc, &id| {
                acc.union(absolute_bounds(arena, layout, id))
            });
        // `Rect::union` treats an empty operand as the identity, so an all-empty set folds back to
        // the empty default — report that as `None` ("nothing to redraw"), not a zero-area rect.
        (!union.is_empty()).then_some(union)
    }

    /// Each dirty node's **absolute** bounds (layout- and paint-dirty), skipping empty rects (a
    /// removed / not-yet-laid-out `NodeId`). Unlike [`damage_rect`](Self::damage_rect) (the union),
    /// this keeps per-node rects for the debug overlay's damage visualization (#69). A pending full
    /// repaint ([`mark_all_dirty`](Self::mark_all_dirty)) tracks no specific nodes and is not
    /// reflected here (the caller treats `is_dirty()` with an empty result as the whole viewport).
    pub fn damage_rects(&self, arena: &Arena, layout: &LayoutTree) -> Vec<Rect> {
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        inner
            .layout
            .iter()
            .chain(inner.paint.iter())
            .map(|&id| absolute_bounds(arena, layout, id))
            .filter(|r| !r.is_empty())
            .collect()
    }

    /// Clears all damage: the frame consumed it. The GPU repaints the whole window for now (§1.4);
    /// once a partial-redraw path lands, callers will read [`damage_rect`](Self::damage_rect) first.
    pub fn clear(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.layout.clear();
            inner.paint.clear();
            inner.full = false;
        }
    }
}

/// Resolves `id`'s parent-relative layout bounds to absolute by summing ancestor origins.
fn absolute_bounds(arena: &Arena, layout: &LayoutTree, id: NodeId) -> Rect {
    let mut rect = layout.layout(id);
    let mut parent = arena.get(id).and_then(|node| node.parent);
    while let Some(p) = parent {
        let p_origin = layout.layout(p).origin;
        rect.origin.x += p_origin.x;
        rect.origin.y += p_origin.y;
        parent = arena.get(p).and_then(|node| node.parent);
    }
    rect
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::{Arena, Node};
    use kagari_base::Size;
    use kagari_layout::{FlexDirection, LayoutStyle, LayoutTree};

    /// Builds a column tree `root → [spacer (20h), mid → leaf (30×10)]`, computed once, and
    /// returns the arena, layout tree, and the `spacer` + `leaf` ids. `mid` is offset below the
    /// spacer, so `leaf`'s absolute bounds exercise ancestor-origin accumulation, and the two
    /// returned nodes occupy disjoint vertical bands (spacer y0..20, leaf y20..30) so a union
    /// over both is non-trivial.
    fn setup() -> (Arena, LayoutTree, NodeId, NodeId) {
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();

        let root = arena.insert(Node::default());
        let spacer = arena.insert(Node::default());
        let mid = arena.insert(Node::default());
        let leaf = arena.insert(Node::default());
        if let Some(n) = arena.get_mut(spacer) {
            n.parent = Some(root);
        }
        if let Some(n) = arena.get_mut(mid) {
            n.parent = Some(root);
        }
        if let Some(n) = arena.get_mut(leaf) {
            n.parent = Some(mid);
        }

        layout.insert(root, None);
        layout.insert(spacer, Some(root));
        layout.insert(mid, Some(root));
        layout.insert(leaf, Some(mid));

        layout.set_style(
            root,
            &LayoutStyle {
                flex_direction: FlexDirection::Column,
                size: Some(Size { w: 100.0, h: 100.0 }),
                ..LayoutStyle::default()
            },
        );
        layout.set_style(
            spacer,
            &LayoutStyle {
                size: Some(Size { w: 100.0, h: 20.0 }),
                ..LayoutStyle::default()
            },
        );
        layout.set_style(mid, &LayoutStyle::default());
        layout.set_style(
            leaf,
            &LayoutStyle {
                size: Some(Size { w: 30.0, h: 10.0 }),
                ..LayoutStyle::default()
            },
        );
        layout.set_children(root, &[spacer, mid]);
        layout.set_children(mid, &[leaf]);

        layout.compute(root, Size { w: 100.0, h: 100.0 }).unwrap();
        (arena, layout, spacer, leaf)
    }

    #[test]
    fn paint_dirty_should_not_trigger_relayout() {
        let (arena, mut layout, _spacer, leaf) = setup();
        // After compute, taffy has cleared every node's dirty flag.
        assert!(!layout.is_dirty(leaf));

        let damage = DamageState::default();
        damage.mark_paint_dirty(leaf);

        // Paint-dirty contributes nothing to the layout drain, so no node is re-marked dirty.
        let to_relayout = damage.take_layout_dirty();
        assert!(
            to_relayout.is_empty(),
            "paint-dirty does not enqueue a relayout"
        );
        for id in to_relayout {
            layout.mark_dirty(id);
        }
        assert!(
            !layout.is_dirty(leaf),
            "an appearance-only change leaves the node clean for layout"
        );
        let _ = arena;
    }

    #[test]
    fn layout_dirty_should_trigger_relayout() {
        let (arena, mut layout, _spacer, leaf) = setup();
        assert!(!layout.is_dirty(leaf));

        let damage = DamageState::default();
        damage.mark_layout_dirty(leaf);

        let to_relayout = damage.take_layout_dirty();
        assert_eq!(to_relayout, vec![leaf], "layout-dirty enqueues the node");
        for id in to_relayout {
            layout.mark_dirty(id);
        }
        assert!(
            layout.is_dirty(leaf),
            "a layout-affecting change marks the node for relayout"
        );
        let _ = arena;
    }

    #[test]
    fn damage_rect_should_resolve_absolute_bounds() {
        let (arena, layout, _spacer, leaf) = setup();
        let damage = DamageState::default();

        // Nothing dirty yet → no damage rect.
        assert!(damage.damage_rect(&arena, &layout).is_none());

        damage.mark_paint_dirty(leaf);
        let rect = damage
            .damage_rect(&arena, &layout)
            .expect("a dirty node yields a damage rect");

        // leaf sits inside `mid`, which is offset below the 20px spacer: its absolute y must
        // accumulate the ancestor origin (not the parent-relative 0).
        assert!(
            rect.origin.y >= 20.0,
            "leaf's absolute y ({}) accumulates the spacer offset through mid",
            rect.origin.y
        );
        assert_eq!(rect.size.w, 30.0, "damage rect spans the leaf's width");
        assert_eq!(rect.size.h, 10.0, "damage rect spans the leaf's height");
    }

    #[test]
    fn damage_rects_should_return_one_absolute_rect_per_dirty_node() {
        let (arena, layout, spacer, leaf) = setup();
        let damage = DamageState::default();

        // Nothing dirty → no rects.
        assert!(damage.damage_rects(&arena, &layout).is_empty());

        // Two nodes (one of each kind) → two per-node absolute rects (not a union).
        damage.mark_paint_dirty(spacer);
        damage.mark_layout_dirty(leaf);
        let rects = damage.damage_rects(&arena, &layout);
        assert_eq!(
            rects.len(),
            2,
            "one rect per dirty node, not a single union"
        );
        assert!(
            rects.iter().any(|r| r.size.h == 10.0),
            "the leaf's 30x10 rect is present"
        );
    }

    #[test]
    fn damage_rect_should_union_multiple_dirty_nodes() {
        let (arena, layout, spacer, leaf) = setup();
        let damage = DamageState::default();

        // Two nodes in disjoint vertical bands (spacer y0..20, leaf y20..30), one of each kind.
        damage.mark_paint_dirty(spacer);
        damage.mark_layout_dirty(leaf);
        let rect = damage
            .damage_rect(&arena, &layout)
            .expect("dirty nodes yield a damage rect");

        // The union must enclose BOTH bands: top at the spacer's y0, bottom at the leaf's y30.
        assert_eq!(rect.origin.y, 0.0, "union starts at the spacer's top");
        assert_eq!(
            rect.size.h, 30.0,
            "union spans the spacer and the leaf below it (0..30), not a single node"
        );
    }

    #[test]
    fn is_dirty_should_track_marks_and_drain() {
        let (arena, layout, _spacer, leaf) = setup();
        let damage = DamageState::default();

        assert!(!damage.is_dirty(), "fresh state is clean");
        damage.mark_layout_dirty(leaf);
        assert!(damage.is_dirty(), "a mark makes it dirty");

        assert_eq!(
            damage.take_layout_dirty(),
            vec![leaf],
            "drain returns the mark"
        );
        assert!(
            !damage.is_dirty(),
            "draining the only mark clears the dirty state"
        );
        let (_, _) = (arena, layout);
    }
}
