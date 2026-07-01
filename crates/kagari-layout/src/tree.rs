//! The layout tree (#33): a `taffy` tree mapped to kagari `NodeId`s, with per-leaf measurement.

use std::collections::HashMap;

use kagari_base::{NodeId, Point, Rect, Size};
use taffy::{AvailableSpace, Size as TaffySize, TaffyTree};

use crate::error::LayoutError;
use crate::measure::{AvailableSize, MeasureFn};
use crate::style::LayoutStyle;

/// A retained layout tree backed by `taffy`.
///
/// Each kagari [`NodeId`] maps 1:1 to a taffy node (and is stored as the taffy node's context, so
/// the measure callback can recover it). Leaves with a [`MeasureFn`] report their intrinsic size
/// during [`LayoutTree::compute`].
pub struct LayoutTree {
    taffy: TaffyTree<NodeId>,
    fwd: HashMap<NodeId, taffy::NodeId>,
    measures: HashMap<NodeId, MeasureFn>,
}

impl LayoutTree {
    /// Creates an empty layout tree.
    pub fn new() -> Self {
        Self {
            taffy: TaffyTree::new(),
            fwd: HashMap::new(),
            measures: HashMap::new(),
        }
    }

    /// Registers `node` (optionally under `parent`). The kagari `NodeId` is the taffy node's
    /// context so the measure callback can map back to it.
    pub fn insert(&mut self, node: NodeId, parent: Option<NodeId>) {
        let Ok(taffy_id) = self
            .taffy
            .new_leaf_with_context(taffy::Style::default(), node)
        else {
            return;
        };
        self.fwd.insert(node, taffy_id);
        if let Some(parent) = parent {
            if let Some(&parent_id) = self.fwd.get(&parent) {
                let _ = self.taffy.add_child(parent_id, taffy_id);
            }
        }
    }

    /// Sets `node`'s layout style (no-op if the node is not in the tree).
    pub fn set_style(&mut self, node: NodeId, style: &LayoutStyle) {
        if let Some(&taffy_id) = self.fwd.get(&node) {
            let _ = self.taffy.set_style(taffy_id, style.to_taffy());
        }
    }

    /// Sets `node`'s leaf measure function (e.g. shaped-text size).
    pub fn set_measure(&mut self, node: NodeId, measure: MeasureFn) {
        self.measures.insert(node, measure);
    }

    /// Sets `parent`'s children to `children` (replacing any existing list), in order. Nodes
    /// must already be `insert`ed; unknown nodes are skipped. Used to link a built subtree.
    pub fn set_children(&mut self, parent: NodeId, children: &[NodeId]) {
        let Some(&parent_id) = self.fwd.get(&parent) else {
            return;
        };
        let child_ids: Vec<taffy::NodeId> = children
            .iter()
            .filter_map(|c| self.fwd.get(c).copied())
            .collect();
        let _ = self.taffy.set_children(parent_id, &child_ids);
    }

    /// Recomputes layout for the `root` subtree against `viewport`. Leaves with a measure
    /// function report their intrinsic size.
    pub fn compute(&mut self, root: NodeId, viewport: Size) -> Result<(), LayoutError> {
        let &root_id = self.fwd.get(&root).ok_or(LayoutError::NodeNotFound(root))?;

        let available = TaffySize {
            width: AvailableSpace::Definite(viewport.w),
            height: AvailableSpace::Definite(viewport.h),
        };

        // Borrow `taffy` and `measures` as disjoint fields so the measure closure can read the
        // measures while taffy is borrowed mutably for the layout pass.
        let taffy = &mut self.taffy;
        let measures = &self.measures;
        taffy
            .compute_layout_with_measure(
                root_id,
                available,
                |known, space, _taffy_id, node_id, _style| {
                    let Some(node_id) = node_id else {
                        return TaffySize::ZERO;
                    };
                    let Some(measure) = measures.get(node_id) else {
                        return TaffySize::ZERO;
                    };
                    let measured = measure(AvailableSize {
                        width: known.width.or_else(|| definite(space.width)),
                        height: known.height.or_else(|| definite(space.height)),
                    });
                    // Honor any known (already-resolved) dimension; otherwise use the measure.
                    TaffySize {
                        width: known.width.unwrap_or(measured.w),
                        height: known.height.unwrap_or(measured.h),
                    }
                },
            )
            .map_err(|e| LayoutError::Taffy(format!("{e}")))?;
        Ok(())
    }

    /// Marks `node` dirty so the next [`compute`](Self::compute) re-lays-out it (no-op if the
    /// node is unknown). taffy marks the node *and its ancestors* dirty, so the relayout
    /// propagates up to the root while clean sibling subtrees keep their cached layout. The
    /// damage layer (#35) calls this for layout-dirty nodes before recomputing.
    pub fn mark_dirty(&mut self, node: NodeId) {
        if let Some(&taffy_id) = self.fwd.get(&node) {
            let _ = self.taffy.mark_dirty(taffy_id);
        }
    }

    /// Whether `node`'s layout is dirty (needs recompute). `false` if the node is unknown.
    pub fn is_dirty(&self, node: NodeId) -> bool {
        self.fwd
            .get(&node)
            .and_then(|&taffy_id| self.taffy.dirty(taffy_id).ok())
            .unwrap_or(false)
    }

    /// The computed bounds of `node` (origin + size), or a zero rect if the node is unknown or
    /// not yet laid out.
    pub fn layout(&self, node: NodeId) -> Rect {
        let Some(&taffy_id) = self.fwd.get(&node) else {
            return Rect::default();
        };
        match self.taffy.layout(taffy_id) {
            Ok(layout) => Rect {
                origin: Point {
                    x: layout.location.x,
                    y: layout.location.y,
                },
                size: Size {
                    w: layout.size.width,
                    h: layout.size.height,
                },
            },
            Err(_) => Rect::default(),
        }
    }

    /// The absolute (window-space) bounds of `node`: its size plus the sum of its own and every
    /// ancestor's parent-relative origin up to the root, via taffy's parent links. This matches the
    /// paint pass's origin accumulation (each element paints its children at `parent_origin +
    /// local`), so it is the anchor rect an overlay places against (#175). A scroll's paint-time
    /// `-offset` translation is *not* reflected here — anchoring to a scrolled element is off by the
    /// scroll offset (a follow-up).
    pub fn absolute_layout(&self, node: NodeId) -> Rect {
        let Some(&taffy_id) = self.fwd.get(&node) else {
            return Rect::default();
        };
        let Ok(own) = self.taffy.layout(taffy_id) else {
            return Rect::default();
        };
        let size = Size {
            w: own.size.width,
            h: own.size.height,
        };
        // Start from the node's own location (already fetched), then add each ancestor's up to root.
        let mut origin = Point {
            x: own.location.x,
            y: own.location.y,
        };
        let mut cur = self.taffy.parent(taffy_id);
        while let Some(tid) = cur {
            if let Ok(layout) = self.taffy.layout(tid) {
                origin.x += layout.location.x;
                origin.y += layout.location.y;
            }
            cur = self.taffy.parent(tid);
        }
        Rect { origin, size }
    }
}

impl Default for LayoutTree {
    fn default() -> Self {
        Self::new()
    }
}

fn definite(space: AvailableSpace) -> Option<f32> {
    match space {
        AvailableSpace::Definite(value) => Some(value),
        AvailableSpace::MinContent | AvailableSpace::MaxContent => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::AvailableSize;
    use crate::style::{FlexDirection, LayoutStyle};
    use kagari_base::Size;

    fn node(raw: u64) -> NodeId {
        NodeId::from_raw(raw)
    }

    #[test]
    fn flex_layout_should_position_children() {
        let mut tree = LayoutTree::new();
        let root = node(1);
        let a = node(2);
        let b = node(3);

        tree.insert(root, None);
        tree.insert(a, Some(root));
        tree.insert(b, Some(root));

        // Row container, two fixed 20x20 children laid out left-to-right.
        tree.set_style(
            root,
            &LayoutStyle {
                flex_direction: FlexDirection::Row,
                size: Some(Size { w: 100.0, h: 50.0 }),
                ..LayoutStyle::default()
            },
        );
        let child_style = LayoutStyle {
            size: Some(Size { w: 20.0, h: 20.0 }),
            ..LayoutStyle::default()
        };
        tree.set_style(a, &child_style);
        tree.set_style(b, &child_style);

        tree.compute(root, Size { w: 100.0, h: 50.0 }).unwrap();

        assert_eq!(tree.layout(a).origin.x, 0.0);
        assert_eq!(
            tree.layout(b).origin.x,
            20.0,
            "second child sits after the first"
        );
        assert_eq!(tree.layout(a).size.w, 20.0);
    }

    #[test]
    fn column_layout_should_stack_children_vertically() {
        let mut tree = LayoutTree::new();
        let root = node(1);
        let a = node(2);
        let b = node(3);
        tree.insert(root, None);
        tree.insert(a, Some(root));
        tree.insert(b, Some(root));

        tree.set_style(
            root,
            &LayoutStyle {
                flex_direction: FlexDirection::Column,
                size: Some(Size { w: 50.0, h: 100.0 }),
                ..LayoutStyle::default()
            },
        );
        let child_style = LayoutStyle {
            size: Some(Size { w: 20.0, h: 20.0 }),
            ..LayoutStyle::default()
        };
        tree.set_style(a, &child_style);
        tree.set_style(b, &child_style);

        tree.compute(root, Size { w: 50.0, h: 100.0 }).unwrap();

        assert_eq!(tree.layout(a).origin.y, 0.0);
        assert_eq!(
            tree.layout(b).origin.y,
            20.0,
            "column stacks the second child below the first"
        );
        assert_eq!(
            tree.layout(b).origin.x,
            0.0,
            "no horizontal advance in a column"
        );
    }

    #[test]
    fn measure_leaf_should_report_intrinsic_size() {
        let mut tree = LayoutTree::new();
        let root = node(1);
        let leaf = node(2);
        tree.insert(root, None);
        tree.insert(leaf, Some(root));
        tree.set_style(root, &LayoutStyle::default());

        // Stub MeasureFn (the real text measure is wired in #34): always reports 30x10.
        tree.set_measure(
            leaf,
            Box::new(|_avail: AvailableSize| Size { w: 30.0, h: 10.0 }),
        );

        tree.compute(root, Size { w: 200.0, h: 200.0 }).unwrap();

        let bounds = tree.layout(leaf);
        assert_eq!(bounds.size.w, 30.0, "leaf width comes from its MeasureFn");
        assert_eq!(bounds.size.h, 10.0, "leaf height comes from its MeasureFn");
    }

    #[test]
    fn compute_unknown_root_should_error() {
        let mut tree = LayoutTree::new();
        let result = tree.compute(node(99), Size { w: 10.0, h: 10.0 });
        assert!(matches!(result, Err(LayoutError::NodeNotFound(_))));
    }

    #[test]
    fn mark_dirty_should_ignore_unknown_node() {
        // Marking a node that was never inserted is a silent no-op (no panic).
        let mut tree = LayoutTree::new();
        tree.mark_dirty(node(99));
        assert!(!tree.is_dirty(node(99)));
    }

    #[test]
    fn is_dirty_should_be_false_for_unknown_node() {
        let tree = LayoutTree::new();
        assert!(!tree.is_dirty(node(42)));
    }

    #[test]
    fn absolute_layout_should_sum_ancestor_origins() {
        // Column root: a 30px spacer then a 50px container holding a child. The child's absolute
        // origin accumulates its container's origin (30) — not just its parent-relative (0).
        let mut tree = LayoutTree::new();
        let (root, spacer, container, child) = (node(1), node(2), node(3), node(4));
        tree.insert(root, None);
        tree.insert(spacer, Some(root));
        tree.insert(container, Some(root));
        tree.insert(child, Some(container));
        tree.set_style(
            root,
            &LayoutStyle {
                flex_direction: FlexDirection::Column,
                size: Some(Size { w: 100.0, h: 100.0 }),
                ..LayoutStyle::default()
            },
        );
        tree.set_style(
            spacer,
            &LayoutStyle {
                size: Some(Size { w: 50.0, h: 30.0 }),
                ..LayoutStyle::default()
            },
        );
        tree.set_style(
            container,
            &LayoutStyle {
                size: Some(Size { w: 50.0, h: 50.0 }),
                ..LayoutStyle::default()
            },
        );
        tree.set_style(
            child,
            &LayoutStyle {
                size: Some(Size { w: 20.0, h: 20.0 }),
                ..LayoutStyle::default()
            },
        );
        tree.compute(root, Size { w: 100.0, h: 100.0 }).unwrap();

        // Parent-relative: the child sits at (0,0) inside its container.
        assert_eq!(tree.layout(child).origin.y, 0.0);
        // Absolute: container is stacked below the 30px spacer, so the child's absolute y is 30.
        let abs = tree.absolute_layout(child);
        assert_eq!(abs.origin.y, 30.0, "absolute y sums the container's origin");
        assert_eq!(abs.size.w, 20.0, "absolute bounds keep the node's own size");
    }

    #[test]
    fn absolute_layout_should_return_default_for_unknown_node() {
        // An unknown node is a silent zero rect (no panic), mirroring `layout`/`is_dirty`.
        let tree = LayoutTree::new();
        assert_eq!(tree.absolute_layout(node(99)), Rect::default());
    }
}
