//! Dock substrate (#222, specs §4.2): a serde-serializable **region model** plus the pure tree
//! operations a dockable workspace needs — tabify/split on a drop, remove-with-collapse, and drop-zone
//! geometry. It is the mechanism the Dock widget (#239) builds on: this module owns the *model* (what is
//! docked where) and the *rules* (how a drop mutates the tree); the widget owns the *chrome* (titlebars,
//! tab bars, the live drag wiring on #178/#219) and the *policy* (which panels, the default layout).
//!
//! Element/GPU/reactive-free — just value types and pure functions (like [`kagari_layout::scroll`]), so it
//! is fully unit-testable. Persist a [`DockTree`] via the generic named-key API on the persistence service
//! (`PersistenceService::set`/`get`, #68).
//!
//! **Invariants** are enforced in ONE place — `normalize` — funnelled through by every construction path
//! ([`DockTree::new`], the `From<Option<DockNode>>` used by deserialization) and every mutation
//! ([`dock`](DockTree::dock)/[`remove`](DockTree::remove)). A normalized tree has: no empty `Tabs` and no
//! single-tab `Tabs` (collapses to a `Leaf`); no empty or single-child `Split` (collapses to the child);
//! `Tabs::active` in range; every `Split` weight finite and `> 0`; and **each `PanelId` at most once**
//! (duplicates — e.g. from a hand-edited RON file — are dropped, first occurrence wins).

use std::collections::HashSet;

use kagari_base::{Axis, Point, Rect};
use serde::{Deserialize, Serialize};

/// An opaque panel identifier. The app mints these (one per dockable panel) and maps them back to its own
/// panel data; the dock model only shuffles ids around. A newtype like [`NodeId`](kagari_base::NodeId),
/// but — since a [`DockTree`] is persisted to RON — `#[serde(transparent)]` so it (de)serializes as a bare
/// scalar (`1`, not `PanelId(1)`), keeping the on-disk layout hand-editable and consistent with the
/// codebase's other RON-persisted scalar newtypes (e.g. [`Px`](kagari_base::Px)).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PanelId(u64);

impl PanelId {
    /// Mints a panel id from a raw value (the app owns the numbering).
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw value.
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// A node in the workspace layout tree. `#[non_exhaustive]` — floating / detached-window regions are a
/// post-MVP variant (specs §4.2), so a new variant must not be a breaking change (ARK-001). Read/match it
/// to render (the Dock widget #239); construct trees through [`DockTree`]'s checked builders so invariants
/// hold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DockNode {
    /// A split of `children` along `axis`; each child carries a relative [`weight`](DockChild::weight). A
    /// normalized split has ≥ 2 children.
    Split {
        /// The orientation the children are arranged along.
        axis: Axis,
        /// The child regions, in order, with their relative weights.
        children: Vec<DockChild>,
    },
    /// A tabbed group of panels; `active` selects one. A normalized `Tabs` has ≥ 2 panels (a single tab
    /// collapses to a [`Leaf`](DockNode::Leaf)).
    Tabs {
        /// The panels sharing this region, in tab order.
        panels: Vec<PanelId>,
        /// The selected tab index (always `< panels.len()` in a normalized tree).
        active: usize,
    },
    /// A single panel filling its region.
    Leaf(PanelId),
}

/// One child of a [`Split`](DockNode::Split): a subtree and its relative `weight` (pane size). Weights are
/// relative — a `[1.0, 2.0]` split gives the second child twice the extent. Normalized weights are finite
/// and `> 0`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DockChild {
    /// The child region.
    pub node: DockNode,
    /// This child's relative size within the split.
    pub weight: f32,
}

/// Where a dragged panel lands relative to a target region: `Center` tabifies into it; the four edges
/// split it (placing the new panel on that side).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropZone {
    /// Tabify: add the panel as a tab of the target region.
    Center,
    /// Split: place the panel to the left of the target region.
    Left,
    /// Split: place the panel to the right.
    Right,
    /// Split: place the panel above.
    Top,
    /// Split: place the panel below.
    Bottom,
}

/// What a dock drop is anchored to. `#[non_exhaustive]` so a future stable region handle can be added
/// without a break. Anchoring by a **stable** identity (a `PanelId`, or the root) — rather than a
/// positional path — keeps [`dock`](DockTree::dock) robust: removing the moving panel first cannot
/// invalidate the target.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum DockTarget {
    /// Dock relative to the region (the `Leaf`, or the `Tabs` group) that contains this panel — so an edge
    /// drop splits the whole group and a `Center` drop tabifies into it.
    Panel(PanelId),
    /// Dock relative to the whole workspace: an edge wraps the entire layout in a new split; on an empty
    /// workspace, any drop seeds the first panel.
    Root,
}

/// The workspace layout: a normalized [`DockNode`] tree, or empty (no panels docked). Construct via
/// [`empty`](Self::empty)/[`leaf`](Self::leaf)/[`new`](Self::new); mutate via [`dock`](Self::dock)/
/// [`remove`](Self::remove); read via [`root`](Self::root)/[`panels`](Self::panels). Serializes to RON as
/// its inner `Option<DockNode>`, re-normalizing on load so a hand-edited/corrupt file can't inject a
/// malformed tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "Option<DockNode>", into = "Option<DockNode>")]
pub struct DockTree(Option<DockNode>);

impl DockTree {
    /// An empty workspace — no panels docked.
    pub fn empty() -> Self {
        Self(None)
    }

    /// A workspace of a single panel.
    pub fn leaf(panel: PanelId) -> Self {
        Self(Some(DockNode::Leaf(panel)))
    }

    /// A workspace from a raw node, **normalized** (collapse degenerate regions, clamp `active`, sanitize
    /// weights, drop duplicate panels).
    pub fn new(node: DockNode) -> Self {
        Self(normalize(Some(node)))
    }

    /// Whether no panels are docked.
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    /// The root node, or `None` when empty. The Dock widget (#239) walks this to render.
    pub fn root(&self) -> Option<&DockNode> {
        self.0.as_ref()
    }

    /// Every panel id in the tree, in depth-first order.
    pub fn panels(&self) -> Vec<PanelId> {
        let mut out = Vec::new();
        if let Some(node) = &self.0 {
            collect_panels(node, &mut out);
        }
        out
    }

    /// Whether `panel` is docked anywhere in the tree.
    pub fn contains(&self, panel: PanelId) -> bool {
        self.panels().contains(&panel)
    }

    /// Moves `moving` to dock at `zone` of `target`, normalizing the result. `moving` is first removed from
    /// wherever it currently is (so this is a *move*, not a copy), then inserted; if that empties the tree,
    /// `moving` becomes the whole workspace. A no-op when `target` is [`Panel(moving)`](DockTarget::Panel)
    /// and that region holds only `moving` (dropping a panel onto its own solitary region changes nothing).
    pub fn dock(&mut self, target: DockTarget, zone: DropZone, moving: PanelId) {
        // Resolve a stable anchor that survives removing `moving` (a panel id ≠ moving), or None for a
        // root-relative dock. This is why removal-first is safe: the anchor is re-found by id afterwards.
        let anchor = match target {
            DockTarget::Root => None,
            // Guard against a target that isn't in the tree: without this, `moving` would be removed first
            // and then have no region to insert into, silently vanishing. An absent target is an invalid
            // drop → no-op (leave the tree, and `moving`, untouched).
            DockTarget::Panel(p) if p != moving => {
                if self.contains(p) {
                    Some(p)
                } else {
                    return;
                }
            }
            DockTarget::Panel(_) => match self.region_sibling(moving) {
                Some(other) => Some(other),
                None => return, // onto its own solitary region → no-op
            },
        };

        let without = self.0.take().and_then(|node| remove_panel(node, moving));
        let Some(root) = without else {
            // Removing `moving` emptied the tree — it becomes the whole workspace.
            self.0 = Some(DockNode::Leaf(moving));
            return;
        };

        let inserted = match anchor {
            None => root_dock(root, moving, zone),
            Some(a) if zone == DropZone::Center => tabify(root, a, moving),
            Some(a) => split_region(root, a, moving, zone),
        };
        self.0 = normalize(Some(inserted));
    }

    /// Removes `panel`, collapsing the region it leaves (an emptied `Tabs`/`Split` is dropped, a
    /// single-child survivor is promoted). A no-op if `panel` is absent; removing the last panel yields an
    /// empty workspace.
    pub fn remove(&mut self, panel: PanelId) {
        self.0 = self.0.take().and_then(|node| remove_panel(node, panel));
    }

    /// Another panel sharing `panel`'s tab group, if any (used to anchor a self-targeted dock).
    fn region_sibling(&self, panel: PanelId) -> Option<PanelId> {
        fn walk(node: &DockNode, panel: PanelId) -> Option<PanelId> {
            match node {
                DockNode::Leaf(_) => None,
                DockNode::Tabs { panels, .. } if panels.contains(&panel) => {
                    panels.iter().copied().find(|id| *id != panel)
                }
                DockNode::Tabs { .. } => None,
                DockNode::Split { children, .. } => {
                    children.iter().find_map(|c| walk(&c.node, panel))
                }
            }
        }
        self.0.as_ref().and_then(|node| walk(node, panel))
    }
}

impl From<Option<DockNode>> for DockTree {
    fn from(node: Option<DockNode>) -> Self {
        Self(normalize(node))
    }
}

impl From<DockTree> for Option<DockNode> {
    fn from(tree: DockTree) -> Self {
        tree.0
    }
}

/// Fraction of each side occupied by an edge drop-band; the remaining centre is the tabify zone.
const EDGE_BAND: f32 = 0.25;

/// The drop [`DropZone`] a pointer at `p` falls in within `region` — a **total** function. `Center` when
/// `p` is in the inner area, or the region is degenerate (zero/negative area), or `p` is outside no band.
/// The four edges are the outer `EDGE_BAND` fraction (half-open `[0, EDGE_BAND)` from each side, so the
/// exact boundary is `Center`). At a corner (in two bands) the point resolves to whichever edge it is
/// *deeper* into; an exact tie prefers `Left`/`Right` over `Top`/`Bottom` (deterministic). An out-of-region
/// point resolves toward its nearest edge (callers pass an in-region point).
pub fn drop_zone(region: Rect, p: Point) -> DropZone {
    let (w, h) = (region.size.w, region.size.h);
    if w <= 0.0 || h <= 0.0 {
        return DropZone::Center;
    }
    // Inward distance from each side, normalized to [0, 1].
    let left = ((p.x - region.origin.x) / w).clamp(0.0, 1.0);
    let top = ((p.y - region.origin.y) / h).clamp(0.0, 1.0);
    let right = 1.0 - left;
    let bottom = 1.0 - top;
    // The nearest side whose band the point is within (order L,R,T,B breaks corner ties toward L/R).
    [
        (left, DropZone::Left),
        (right, DropZone::Right),
        (top, DropZone::Top),
        (bottom, DropZone::Bottom),
    ]
    .into_iter()
    .filter(|(d, _)| *d < EDGE_BAND)
    .min_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    .map(|(_, zone)| zone)
    .unwrap_or(DropZone::Center)
}

// --- Pure tree operations (rebuild-by-value; trees are small) ---

/// Collects every panel id depth-first.
fn collect_panels(node: &DockNode, out: &mut Vec<PanelId>) {
    match node {
        DockNode::Leaf(id) => out.push(*id),
        DockNode::Tabs { panels, .. } => out.extend_from_slice(panels),
        DockNode::Split { children, .. } => {
            for c in children {
                collect_panels(&c.node, out);
            }
        }
    }
}

/// Normalizes a (possibly `None`) root: the single invariant-enforcement point. Dedupes panel ids globally
/// (first wins), collapses empty/single-child regions, clamps `active`, sanitizes weights. Returns `None`
/// for an empty tree.
fn normalize(node: Option<DockNode>) -> Option<DockNode> {
    let mut seen = HashSet::new();
    node.and_then(|n| normalize_node(n, &mut seen))
}

fn normalize_node(node: DockNode, seen: &mut HashSet<PanelId>) -> Option<DockNode> {
    match node {
        DockNode::Leaf(id) => seen.insert(id).then_some(DockNode::Leaf(id)),
        DockNode::Tabs { panels, active } => {
            let kept: Vec<PanelId> = panels.into_iter().filter(|id| seen.insert(*id)).collect();
            match kept.len() {
                0 => None,
                1 => kept.into_iter().next().map(DockNode::Leaf),
                n => Some(DockNode::Tabs {
                    panels: kept,
                    active: active.min(n - 1),
                }),
            }
        }
        DockNode::Split { axis, children } => {
            let kept: Vec<DockChild> = children
                .into_iter()
                .filter_map(|c| {
                    normalize_node(c.node, seen).map(|node| DockChild {
                        node,
                        weight: sanitize_weight(c.weight),
                    })
                })
                .collect();
            match kept.len() {
                0 => None,
                1 => kept.into_iter().next().map(|c| c.node),
                _ => Some(DockNode::Split {
                    axis,
                    children: kept,
                }),
            }
        }
    }
}

/// A weight coerced to a usable value: a non-finite or `<= 0` weight (e.g. from a corrupt file) becomes 1.
fn sanitize_weight(w: f32) -> f32 {
    if w.is_finite() && w > 0.0 { w } else { 1.0 }
}

/// Removes `panel`, collapsing empties (an emptied `Tabs`/`Split` → `None`, a single survivor → promoted).
fn remove_panel(node: DockNode, panel: PanelId) -> Option<DockNode> {
    match node {
        DockNode::Leaf(id) => (id != panel).then_some(DockNode::Leaf(id)),
        DockNode::Tabs { panels, active } => {
            // Preserve the *selected* panel across the removal: shift `active` left by the removed tabs that
            // were before it, so closing a tab to the left of the active one doesn't switch the selection.
            let removed_before = panels
                .iter()
                .take(active)
                .filter(|id| **id == panel)
                .count();
            let kept: Vec<PanelId> = panels.into_iter().filter(|id| *id != panel).collect();
            match kept.len() {
                0 => None,
                1 => kept.into_iter().next().map(DockNode::Leaf),
                n => Some(DockNode::Tabs {
                    active: active.saturating_sub(removed_before).min(n - 1),
                    panels: kept,
                }),
            }
        }
        DockNode::Split { axis, children } => {
            let kept: Vec<DockChild> = children
                .into_iter()
                .filter_map(|c| {
                    remove_panel(c.node, panel).map(|node| DockChild {
                        node,
                        weight: c.weight,
                    })
                })
                .collect();
            match kept.len() {
                0 => None,
                1 => kept.into_iter().next().map(|c| c.node),
                _ => Some(DockNode::Split {
                    axis,
                    children: kept,
                }),
            }
        }
    }
}

/// Adds `moving` as a tab of the region containing `target` (the `Leaf(target)` becomes a two-tab group; a
/// `Tabs` containing `target` gains `moving`, selected).
fn tabify(node: DockNode, target: PanelId, moving: PanelId) -> DockNode {
    match node {
        DockNode::Leaf(id) if id == target => DockNode::Tabs {
            panels: vec![id, moving],
            active: 1,
        },
        DockNode::Leaf(id) => DockNode::Leaf(id),
        DockNode::Tabs { mut panels, active } => {
            if panels.contains(&target) {
                panels.push(moving);
                let active = panels.len() - 1;
                DockNode::Tabs { panels, active }
            } else {
                DockNode::Tabs { panels, active }
            }
        }
        DockNode::Split { axis, children } => DockNode::Split {
            axis,
            children: children
                .into_iter()
                .map(|c| DockChild {
                    node: tabify(c.node, target, moving),
                    weight: c.weight,
                })
                .collect(),
        },
    }
}

/// Wraps the region containing `target` in a new split, placing `moving` on `edge`'s side.
fn split_region(node: DockNode, target: PanelId, moving: PanelId, edge: DropZone) -> DockNode {
    match node {
        DockNode::Leaf(id) if id == target => wrap_split(DockNode::Leaf(id), moving, edge),
        DockNode::Leaf(id) => DockNode::Leaf(id),
        DockNode::Tabs { panels, active } => {
            if panels.contains(&target) {
                wrap_split(DockNode::Tabs { panels, active }, moving, edge)
            } else {
                DockNode::Tabs { panels, active }
            }
        }
        DockNode::Split { axis, children } => DockNode::Split {
            axis,
            children: children
                .into_iter()
                .map(|c| DockChild {
                    node: split_region(c.node, target, moving, edge),
                    weight: c.weight,
                })
                .collect(),
        },
    }
}

/// Wraps `region` in a two-child split with `moving` on `edge`'s side, equal weights.
fn wrap_split(region: DockNode, moving: PanelId, edge: DropZone) -> DockNode {
    let axis = match edge {
        DropZone::Left | DropZone::Right => Axis::Horizontal,
        _ => Axis::Vertical,
    };
    let moving_child = DockChild {
        node: DockNode::Leaf(moving),
        weight: 1.0,
    };
    let region_child = DockChild {
        node: region,
        weight: 1.0,
    };
    let children = match edge {
        DropZone::Left | DropZone::Top => vec![moving_child, region_child],
        _ => vec![region_child, moving_child],
    };
    DockNode::Split { axis, children }
}

/// Docks `moving` relative to the whole workspace root: an edge wraps the entire tree; `Center` tabifies a
/// `Leaf`/`Tabs` root, or (a `Split` root has no single group to tab into) appends below.
fn root_dock(root: DockNode, moving: PanelId, zone: DropZone) -> DockNode {
    match zone {
        DropZone::Center => match root {
            DockNode::Leaf(id) => DockNode::Tabs {
                panels: vec![id, moving],
                active: 1,
            },
            DockNode::Tabs { mut panels, .. } => {
                panels.push(moving);
                let active = panels.len() - 1;
                DockNode::Tabs { panels, active }
            }
            split @ DockNode::Split { .. } => wrap_split(split, moving, DropZone::Bottom),
        },
        edge => wrap_split(root, moving, edge),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: u64) -> PanelId {
        PanelId::from_raw(n)
    }

    #[test]
    fn dock_tree_should_serialize_roundtrip() {
        // A mixed Split(Tabs, Leaf) tree survives a RON round-trip unchanged.
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
        let text = ron::ser::to_string(&tree).expect("serialize");
        let back: DockTree = ron::from_str(&text).expect("deserialize");
        assert_eq!(tree, back, "the dock tree round-trips through RON");
    }

    #[test]
    fn dock_drop_center_should_tabify() {
        // Dock panel 2 onto panel 1's centre → a two-tab group, the dropped panel selected.
        let mut tree = DockTree::leaf(p(1));
        tree.dock(DockTarget::Panel(p(1)), DropZone::Center, p(2));
        assert_eq!(
            tree.root(),
            Some(&DockNode::Tabs {
                panels: vec![p(1), p(2)],
                active: 1,
            }),
            "a centre drop tabifies the target region"
        );
    }

    #[test]
    fn dock_drop_edge_should_split() {
        // Dock panel 2 onto panel 1's left edge → a horizontal split with the new panel first.
        let mut tree = DockTree::leaf(p(1));
        tree.dock(DockTarget::Panel(p(1)), DropZone::Left, p(2));
        assert_eq!(
            tree.root(),
            Some(&DockNode::Split {
                axis: Axis::Horizontal,
                children: vec![
                    DockChild {
                        node: DockNode::Leaf(p(2)),
                        weight: 1.0,
                    },
                    DockChild {
                        node: DockNode::Leaf(p(1)),
                        weight: 1.0,
                    },
                ],
            }),
            "an edge drop splits the target region, new panel on the edge side"
        );
    }

    #[test]
    fn dock_remove_should_collapse_empty_regions() {
        // Remove a panel from a two-way split → the split collapses to the survivor.
        let mut tree = DockTree::new(DockNode::Split {
            axis: Axis::Vertical,
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
        });
        tree.remove(p(1));
        assert_eq!(
            tree.root(),
            Some(&DockNode::Leaf(p(2))),
            "removing one side of a split collapses it to the other"
        );
    }

    #[test]
    fn dock_remove_last_panel_should_empty() {
        let mut tree = DockTree::leaf(p(1));
        tree.remove(p(1));
        assert!(
            tree.is_empty(),
            "removing the last panel empties the workspace"
        );
        assert_eq!(tree.root(), None);
    }

    #[test]
    fn dock_onto_empty_should_seed_the_workspace() {
        // Any drop on an empty workspace seeds the first panel.
        let mut tree = DockTree::empty();
        tree.dock(DockTarget::Root, DropZone::Center, p(7));
        assert_eq!(
            tree.root(),
            Some(&DockNode::Leaf(p(7))),
            "docking into an empty workspace seeds the first panel"
        );
    }

    #[test]
    fn dock_onto_self_should_noop() {
        // Dropping a lone panel onto its own region changes nothing.
        let mut tree = DockTree::leaf(p(1));
        tree.dock(DockTarget::Panel(p(1)), DropZone::Center, p(1));
        assert_eq!(
            tree.root(),
            Some(&DockNode::Leaf(p(1))),
            "self-dock is a no-op"
        );
    }

    #[test]
    fn dock_within_same_group_should_reorder() {
        // Moving the FIRST tab onto a sibling in its own group actually reorders it to the end (and selects
        // it) — a real move within the group, defined and panic-free.
        let mut tree = DockTree::new(DockNode::Tabs {
            panels: vec![p(1), p(2), p(3)],
            active: 0,
        });
        tree.dock(DockTarget::Panel(p(2)), DropZone::Center, p(1));
        assert_eq!(
            tree.root(),
            Some(&DockNode::Tabs {
                panels: vec![p(2), p(3), p(1)],
                active: 2,
            }),
            "moving the first tab within its group reorders it to the end and selects it"
        );
    }

    #[test]
    fn dock_should_move_not_copy() {
        // Docking a panel that is already elsewhere removes the old occurrence (no duplicate).
        let mut tree = DockTree::new(DockNode::Split {
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
        });
        tree.dock(DockTarget::Panel(p(1)), DropZone::Center, p(2));
        // p(2) left its split pane (collapsing the split) and joined p(1)'s group — appears exactly once.
        assert_eq!(
            tree.panels(),
            vec![p(1), p(2)],
            "the moved panel is not duplicated"
        );
        assert_eq!(
            tree.root(),
            Some(&DockNode::Tabs {
                panels: vec![p(1), p(2)],
                active: 1,
            })
        );
    }

    #[test]
    fn dock_tree_should_normalize_malformed_on_deserialize() {
        // A hand-edited RON with an empty Tabs child, a duplicate panel, and an out-of-range active must
        // load as a valid, normalized tree — the empty Tabs drops, the single-child split collapses, the
        // duplicate is dropped (first wins), active is clamped.
        let malformed = "Some(Split(axis: Horizontal, children: [\
            (node: Tabs(panels: [], active: 3), weight: 1.0), \
            (node: Tabs(panels: [1, 1], active: 9), weight: 1.0)]))";
        let tree: DockTree = ron::from_str(malformed).expect("deserialize malformed");
        // Empty Tabs → dropped; dup PanelId(1) → single tab → collapses to Leaf(1); single-child split →
        // collapses to that leaf.
        assert_eq!(
            tree.root(),
            Some(&DockNode::Leaf(p(1))),
            "a malformed tree normalizes on load"
        );
        assert_eq!(
            tree.panels(),
            vec![p(1)],
            "the duplicate panel appears once"
        );
    }

    #[test]
    fn dock_new_should_dedupe_panels_and_sanitize_weights() {
        // The same panel in two places → the second is dropped; a non-positive/NaN weight → 1.0.
        let tree = DockTree::new(DockNode::Split {
            axis: Axis::Horizontal,
            children: vec![
                DockChild {
                    node: DockNode::Leaf(p(1)),
                    weight: -5.0,
                },
                DockChild {
                    node: DockNode::Leaf(p(1)),
                    weight: f32::NAN,
                },
                DockChild {
                    node: DockNode::Leaf(p(2)),
                    weight: 3.0,
                },
            ],
        });
        // Second p(1) dropped → children [Leaf(1) w1, Leaf(2) w3] (the -5 weight sanitized to 1).
        assert_eq!(
            tree.root(),
            Some(&DockNode::Split {
                axis: Axis::Horizontal,
                children: vec![
                    DockChild {
                        node: DockNode::Leaf(p(1)),
                        weight: 1.0,
                    },
                    DockChild {
                        node: DockNode::Leaf(p(2)),
                        weight: 3.0,
                    },
                ],
            }),
            "duplicates dropped (first wins) and weights sanitized"
        );
    }

    #[test]
    fn drop_zone_should_classify_center_and_edges() {
        let r = Rect::from_xywh(0.0, 0.0, 100.0, 100.0);
        assert_eq!(
            drop_zone(r, Point::new(50.0, 50.0)),
            DropZone::Center,
            "middle"
        );
        assert_eq!(
            drop_zone(r, Point::new(5.0, 50.0)),
            DropZone::Left,
            "left band"
        );
        assert_eq!(
            drop_zone(r, Point::new(95.0, 50.0)),
            DropZone::Right,
            "right band"
        );
        assert_eq!(
            drop_zone(r, Point::new(50.0, 5.0)),
            DropZone::Top,
            "top band"
        );
        assert_eq!(
            drop_zone(r, Point::new(50.0, 95.0)),
            DropZone::Bottom,
            "bottom band"
        );
        // A corner is in both the left and top bands, equally deep → left wins (L/R precedence).
        assert_eq!(
            drop_zone(r, Point::new(5.0, 5.0)),
            DropZone::Left,
            "corner tie → left"
        );
        // The exact band boundary (25%) is the centre (half-open bands).
        assert_eq!(
            drop_zone(r, Point::new(25.0, 50.0)),
            DropZone::Center,
            "boundary is centre"
        );
        // A degenerate (zero-area) region is always the centre.
        let z = Rect::from_xywh(0.0, 0.0, 0.0, 0.0);
        assert_eq!(
            drop_zone(z, Point::new(0.0, 0.0)),
            DropZone::Center,
            "degenerate → centre"
        );
    }

    #[test]
    fn dock_remove_should_track_active_across_earlier_tab() {
        // Closing a tab to the LEFT of the active one keeps the SAME panel selected (active shifts left) —
        // not a bare clamp, which would silently move the selection to the next panel.
        let mut tree = DockTree::new(DockNode::Tabs {
            panels: vec![p(1), p(2), p(3)],
            active: 1, // p(2) selected
        });
        tree.remove(p(1));
        assert_eq!(
            tree.root(),
            Some(&DockNode::Tabs {
                panels: vec![p(2), p(3)],
                active: 0, // still p(2), not p(3)
            }),
            "removing a tab left of the active one keeps the selection on the same panel"
        );
    }

    #[test]
    fn dock_onto_absent_target_should_noop() {
        // Docking relative to a panel that isn't in the tree is an invalid drop → a no-op: the tree is
        // untouched and the moving panel is neither lost nor added.
        let mut tree = DockTree::leaf(p(1));
        tree.dock(DockTarget::Panel(p(99)), DropZone::Center, p(2));
        assert_eq!(
            tree.root(),
            Some(&DockNode::Leaf(p(1))),
            "an absent target leaves the tree untouched"
        );
        assert!(
            !tree.contains(p(2)),
            "the moving panel is not lost (nor added) on an invalid drop"
        );
    }

    #[test]
    fn dock_tree_should_serialize_panel_ids_as_bare_scalars() {
        // `#[serde(transparent)]` on PanelId → the on-disk RON is `panels: [1, 2]`, not `[(1), (2)]` /
        // `[PanelId(1), ...]` — hand-editable and consistent with the RON scalar-newtype convention.
        let tree = DockTree::new(DockNode::Tabs {
            panels: vec![p(1), p(2)],
            active: 0,
        });
        let text = ron::ser::to_string(&tree).expect("serialize");
        assert!(
            !text.contains("(1)") && !text.contains("PanelId"),
            "panel ids serialize as bare scalars, not newtype tuples: {text}"
        );
    }

    #[test]
    fn dock_root_edge_should_wrap_whole_tree() {
        // A `Root` edge drop wraps the entire layout in a new split, the new panel on that side.
        let mut tree = DockTree::new(DockNode::Tabs {
            panels: vec![p(1), p(2)],
            active: 0,
        });
        tree.dock(DockTarget::Root, DropZone::Top, p(3));
        assert_eq!(
            tree.root(),
            Some(&DockNode::Split {
                axis: Axis::Vertical,
                children: vec![
                    DockChild {
                        node: DockNode::Leaf(p(3)),
                        weight: 1.0,
                    },
                    DockChild {
                        node: DockNode::Tabs {
                            panels: vec![p(1), p(2)],
                            active: 0,
                        },
                        weight: 1.0,
                    },
                ],
            }),
            "a root Top-edge drop wraps the whole tree, new panel on top"
        );
    }

    #[test]
    fn dock_root_center_on_split_should_append_below() {
        // `Root` + `Center` when the root is a Split (no single group to tab into) appends the panel below.
        let split = DockNode::Split {
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
        };
        let mut tree = DockTree::new(split.clone());
        tree.dock(DockTarget::Root, DropZone::Center, p(3));
        assert_eq!(
            tree.root(),
            Some(&DockNode::Split {
                axis: Axis::Vertical,
                children: vec![
                    DockChild {
                        node: split,
                        weight: 1.0,
                    },
                    DockChild {
                        node: DockNode::Leaf(p(3)),
                        weight: 1.0,
                    },
                ],
            }),
            "root Center on a Split appends the panel below the whole split"
        );
    }

    #[test]
    fn dock_deserialize_should_clamp_active_on_surviving_multitab() {
        // A multi-tab Tabs (≥2 panels, so it survives) with an out-of-range `active` clamps into range.
        let tree: DockTree =
            ron::from_str("Some(Tabs(panels: [1, 2, 3], active: 9))").expect("deserialize");
        assert_eq!(
            tree.root(),
            Some(&DockNode::Tabs {
                panels: vec![p(1), p(2), p(3)],
                active: 2,
            }),
            "an out-of-range active clamps to the last tab"
        );
    }

    #[test]
    fn panel_id_should_round_trip() {
        assert_eq!(PanelId::from_raw(9).raw(), 9);
    }
}
