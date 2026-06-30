//! Cursor shapes (#53, specs §1.9): an element declares a cursor; on pointer move the topmost hit —
//! walking up to its nearest cursor-declaring ancestor — decides which cursor the window shows; outside
//! any cursor region it reverts to default; and the window re-applies the cursor only when the resolved
//! icon changes (no per-move churn).
//!
//! This is the headless model: the [`CursorIcon`] vocabulary, the [`CursorRegistry`] that resolves the
//! icon at a point (after a hit-test pick), and the [`CursorState`] change-detector. The
//! `CursorIcon → winit` mapping and the actual `window.set_cursor` call land with the deferred live
//! mouse wiring (deferred since #48), where a real consumer exists. The registry mirrors the focus model:
//! a `NodeId → CursorIcon` association populated at build via `LayoutCx.cursor` (like `track_focus`).

use std::collections::HashMap;

use kagari_base::{NodeId, Point};

use crate::arena::Arena;
use crate::event::{HitTest, ancestor_path};

/// A cursor shape, kagari's own enum mapping to winit's `CursorIcon` for the common shapes (the mapping
/// lives at the window boundary, with the deferred live wiring). Keeping it a kagari type avoids leaking
/// the winit type through the public builder (design.md §1). `#[non_exhaustive]` so more shapes can be
/// added without a breaking change.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[non_exhaustive]
pub enum CursorIcon {
    /// The platform default arrow — also the resolved cursor outside any declared region.
    #[default]
    Default,
    /// A pointing hand (clickable affordance).
    Pointer,
    /// An I-beam (editable / selectable text).
    Text,
    /// A crosshair (precise selection).
    Crosshair,
    /// An open hand (draggable, not yet grabbed).
    Grab,
    /// A closed hand (actively dragging).
    Grabbing,
    /// Column resize (horizontal split handle).
    ColResize,
    /// Row resize (vertical split handle).
    RowResize,
    /// East-west resize (left/right edge).
    EwResize,
    /// North-south resize (top/bottom edge).
    NsResize,
}

/// Per-window `NodeId → declared CursorIcon`, populated at build by [`Div::cursor`](crate::element::Div::cursor)
/// (via `LayoutCx.cursor`), mirroring [`FocusRegistry`](crate::event::FocusRegistry). [`resolve`](Self::resolve)
/// reads it after a hit-test pick to decide the cursor under the pointer.
#[derive(Default)]
pub struct CursorRegistry {
    cursors: HashMap<NodeId, CursorIcon>,
}

impl CursorRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `node`'s declared cursor (called at build by `Div::cursor`).
    pub fn register(&mut self, node: NodeId, icon: CursorIcon) {
        self.cursors.insert(node, icon);
    }

    /// The cursor at `pos`: the topmost hit's nearest cursor-declaring node, found by walking the arena
    /// ancestor chain from that node up to the root (innermost first). Returns [`CursorIcon::default`]
    /// when `pos` hits no region, or no node on the chain declares a cursor.
    pub fn resolve(&self, hit: &HitTest, arena: &Arena, pos: Point) -> CursorIcon {
        let Some(node) = hit.pick(pos) else {
            return CursorIcon::default();
        };
        // `ancestor_path` is root → … → node; reverse so the innermost (the picked node) is tried first
        // and the nearest declaration wins.
        ancestor_path(arena, node)
            .into_iter()
            .rev()
            .find_map(|n| self.cursors.get(&n).copied())
            .unwrap_or_default()
    }
}

/// Tracks the cursor last handed to the window, so the resolved icon is applied only when it changes —
/// avoiding per-move churn (and the damage that a redundant re-apply would imply). The live
/// `window.set_cursor` call is wired with the deferred mouse driver; this is the headless change-detector
/// it drives. Starts at [`CursorIcon::default`], matching the window's initial cursor.
pub struct CursorState {
    current: CursorIcon,
}

impl CursorState {
    /// Creates a state at the default cursor (the window's initial cursor).
    pub fn new() -> Self {
        Self {
            current: CursorIcon::default(),
        }
    }

    /// Records the newly resolved cursor; returns `Some(icon)` to apply to the window only when it
    /// differs from the last one, else `None` (unchanged → no re-apply).
    pub fn apply(&mut self, resolved: CursorIcon) -> Option<CursorIcon> {
        (resolved != self.current).then(|| {
            self.current = resolved;
            resolved
        })
    }
}

impl Default for CursorState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::Node;
    use crate::event::{HitRegion, InteractFlags};
    use kagari_base::Rect;

    /// A cursor-flagged hit region for `node` at the given xywh and painter order, clipped to itself.
    fn region(node: NodeId, x: f32, y: f32, w: f32, h: f32, order: u32) -> HitRegion {
        let bounds = Rect::from_xywh(x, y, w, h);
        HitRegion {
            node,
            bounds,
            clip: bounds,
            order,
            flags: InteractFlags {
                cursor: true,
                ..InteractFlags::default()
            },
        }
    }

    #[test]
    fn cursor_should_resolve_topmost_declared() {
        // Two overlapping regions cover (5,5); each node declares its own cursor. The front-most
        // (larger order) node is picked, so its cursor wins.
        let mut arena = Arena::new();
        let back = arena.insert(Node::default());
        let front = arena.insert(Node::default());

        let mut hit = HitTest::new();
        hit.push(region(back, 0.0, 0.0, 10.0, 10.0, 0));
        hit.push(region(front, 0.0, 0.0, 10.0, 10.0, 1));

        let mut reg = CursorRegistry::new();
        reg.register(back, CursorIcon::Text);
        reg.register(front, CursorIcon::Pointer);

        assert_eq!(
            reg.resolve(&hit, &arena, Point::new(5.0, 5.0)),
            CursorIcon::Pointer,
            "the front-most declared cursor wins"
        );
    }

    #[test]
    fn cursor_should_walk_to_ancestor_declaration() {
        // The picked (innermost) node declares no cursor; its arena ancestor does. The walk up the
        // chain resolves the nearest declaration — the ancestor's Pointer.
        let mut arena = Arena::new();
        let parent = arena.insert(Node::default());
        let child = arena.insert(Node {
            parent: Some(parent),
            children: Vec::new(),
        });

        let mut hit = HitTest::new();
        // Only the child is under the pointer (picked); the parent declares the cursor.
        hit.push(region(child, 0.0, 0.0, 10.0, 10.0, 0));

        let mut reg = CursorRegistry::new();
        reg.register(parent, CursorIcon::Pointer);

        assert_eq!(
            reg.resolve(&hit, &arena, Point::new(5.0, 5.0)),
            CursorIcon::Pointer,
            "the nearest ancestor with a declared cursor wins"
        );
    }

    #[test]
    fn cursor_should_default_outside() {
        // A point in no region resolves to the default cursor.
        let mut arena = Arena::new();
        let node = arena.insert(Node::default());
        let mut hit = HitTest::new();
        hit.push(region(node, 0.0, 0.0, 10.0, 10.0, 0));
        let mut reg = CursorRegistry::new();
        reg.register(node, CursorIcon::Pointer);

        assert_eq!(
            reg.resolve(&hit, &arena, Point::new(50.0, 50.0)),
            CursorIcon::Default,
            "outside every region the cursor is the default"
        );
    }

    #[test]
    fn cursor_state_should_reapply_only_on_change() {
        let mut state = CursorState::new();
        // First non-default resolve applies.
        assert_eq!(state.apply(CursorIcon::Pointer), Some(CursorIcon::Pointer));
        // Same icon again: no re-apply.
        assert_eq!(state.apply(CursorIcon::Pointer), None);
        // A different icon applies.
        assert_eq!(state.apply(CursorIcon::Text), Some(CursorIcon::Text));
        // Back to default applies (leaving a cursor region).
        assert_eq!(state.apply(CursorIcon::Default), Some(CursorIcon::Default));
        // Default again: no re-apply.
        assert_eq!(state.apply(CursorIcon::Default), None);
    }
}
