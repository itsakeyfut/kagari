//! Focus (#49, specs §1.5): a `FocusHandle` whose identity (`FocusId`) is independent of the arena
//! `NodeId`, tree-order tab navigation, focus-within, and the focus state keyboard dispatch targets.
//!
//! **gpui model**: a `FocusHandle` can be created and held (e.g. in app state) before its element
//! mounts, and survives arena churn — so focus identity is a `FocusId` minted by the registry, not a
//! `NodeId`. The current focus is a reactive `RwSignal<Option<FocusId>>`, so `is_focused()` is a
//! tracked read and focused / focus-within styling updates fine-grained (`.bg(move || if h.is_focused()
//! { accent() } else { surface() })`).
//!
//! The [`FocusRegistry`] is explicit here (threaded via `LayoutCx.focus`); the live winit keyboard
//! wiring and a context-provided registry land with the first interactive consumer (#49 scope, like
//! #48's deferred mouse wiring).

use std::collections::HashMap;

use kagari_base::NodeId;

use crate::arena::Arena;
use crate::event::ancestor_path;
use crate::reactive::RwSignal;
use crate::reactive::prelude::*;

/// A focusable target's identity, independent of the arena [`NodeId`] (gpui model). Minted by
/// [`FocusRegistry::handle`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FocusId(u64);

/// A clone-able handle to a focusable target. Holds the [`FocusId`] plus the registry's current-focus
/// signal, so [`is_focused`](Self::is_focused) / [`focus`](Self::focus) work standalone (no context
/// needed). Bind it to an element with `div().track_focus(&handle)`.
#[derive(Clone)]
pub struct FocusHandle {
    id: FocusId,
    current: RwSignal<Option<FocusId>>,
}

impl FocusHandle {
    /// This handle's focus identity.
    pub fn id(&self) -> FocusId {
        self.id
    }

    /// Whether this handle currently holds focus. A **tracked** read of the focus signal, so reading
    /// it inside a reactive prop closure makes focus-driven styling update on focus changes.
    pub fn is_focused(&self) -> bool {
        self.current.get() == Some(self.id)
    }

    /// Moves focus to this handle's target.
    pub fn focus(&self) {
        self.current.set(Some(self.id));
    }
}

/// Per-window focus state: the current-focus signal, the next id to mint, the tab-order list (in
/// `handle()` call order = tree order), and the `FocusId → NodeId` map populated at build by
/// `track_focus`.
pub struct FocusRegistry {
    current: RwSignal<Option<FocusId>>,
    next: u64,
    order: Vec<FocusId>,
    nodes: HashMap<FocusId, NodeId>,
}

impl FocusRegistry {
    /// Creates an empty registry. Creates the current-focus [`RwSignal`], so it must be called under
    /// an ambient reactive `Owner` (the app root owner; tests establish one — RK-005).
    pub fn new() -> Self {
        Self {
            current: RwSignal::new(None),
            next: 0,
            order: Vec::new(),
            nodes: HashMap::new(),
        }
    }

    /// The current-focus signal (e.g. for the app to read/observe the focused id).
    pub fn current(&self) -> RwSignal<Option<FocusId>> {
        self.current
    }

    /// Mints a new [`FocusHandle`] and appends it to the tab order. Components mint handles in
    /// pre-order (parent before children), so the tab order is tree order.
    pub fn handle(&mut self) -> FocusHandle {
        let id = FocusId(self.next);
        self.next += 1;
        self.order.push(id);
        FocusHandle {
            id,
            current: self.current,
        }
    }

    /// Associates a [`FocusId`] with its arena [`NodeId`] (called at build by `track_focus`), so
    /// keyboard dispatch and focus-within can resolve the focused node.
    pub fn register(&mut self, id: FocusId, node: NodeId) {
        self.nodes.insert(id, node);
    }

    /// The arena node of the currently-focused target, if any is focused and registered.
    pub fn focused_node(&self) -> Option<NodeId> {
        self.current
            .get_untracked()
            .and_then(|id| self.nodes.get(&id).copied())
    }

    /// Moves focus to the next focusable in tab order (wrapping); from no focus, the first.
    pub fn focus_next(&self) {
        self.step(1);
    }

    /// Moves focus to the previous focusable in tab order (wrapping); from no focus, the last.
    pub fn focus_prev(&self) {
        self.step(-1);
    }

    /// Advances the focus by `delta` (+1 next / -1 prev) over the tab order, wrapping.
    fn step(&self, delta: isize) {
        if self.order.is_empty() {
            return;
        }
        let len = self.order.len();
        let next = match self.current.get_untracked() {
            Some(cur) => match self.order.iter().position(|&id| id == cur) {
                // (cur + delta) mod len, with wrapping handled via `rem_euclid`.
                Some(i) => (i as isize + delta).rem_euclid(len as isize) as usize,
                None => 0,
            },
            // No focus yet: next → first, prev → last.
            None if delta >= 0 => 0,
            None => len - 1,
        };
        self.current.set(Some(self.order[next]));
    }

    /// Whether `node` contains focus: it is the focused node or an ancestor of it.
    pub fn is_focus_within(&self, arena: &Arena, node: NodeId) -> bool {
        match self.focused_node() {
            Some(focused) => ancestor_path(arena, focused).contains(&node),
            None => false,
        }
    }
}

impl Default for FocusRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::{Arena, Node};
    use crate::reactive::Owner;

    fn node(arena: &mut Arena, parent: Option<NodeId>) -> NodeId {
        arena.insert(Node {
            parent,
            children: Vec::new(),
        })
    }

    #[test]
    fn focus_handle_should_report_is_focused() {
        let owner = Owner::new();
        owner.set();

        let mut reg = FocusRegistry::new();
        let a = reg.handle();
        let b = reg.handle();

        assert!(!a.is_focused(), "nothing focused initially");
        a.focus();
        assert!(a.is_focused(), "a is focused after focus()");
        assert!(!b.is_focused(), "b is not focused");

        drop(owner);
    }

    #[test]
    fn tab_should_move_focus_in_tree_order() {
        let owner = Owner::new();
        owner.set();

        let mut reg = FocusRegistry::new();
        // Minted in tree order.
        let a = reg.handle();
        let b = reg.handle();
        let c = reg.handle();

        reg.focus_next(); // None → first
        assert!(a.is_focused(), "focus_next from none selects the first");
        reg.focus_next();
        assert!(b.is_focused(), "advances to the second");
        reg.focus_prev();
        assert!(a.is_focused(), "focus_prev steps back to the first");
        reg.focus_prev();
        assert!(
            c.is_focused(),
            "focus_prev from the first wraps to the last"
        );

        drop(owner);
    }

    #[test]
    fn focus_next_on_empty_registry_should_be_noop() {
        // No focusables registered: `step`'s empty-order guard makes nav a no-op (no panic on the
        // `len - 1` / index math), leaving focus unset.
        let owner = Owner::new();
        owner.set();

        let reg = FocusRegistry::new();
        reg.focus_next();
        reg.focus_prev();
        assert_eq!(
            reg.current().get_untracked(),
            None,
            "navigating an empty registry leaves focus unset"
        );

        drop(owner);
    }

    #[test]
    fn focus_within_should_be_true_for_ancestor() {
        let owner = Owner::new();
        owner.set();

        let mut reg = FocusRegistry::new();
        let mut arena = Arena::new();
        // root → inner (focused); sibling is a separate child of root.
        let root = node(&mut arena, None);
        let inner = node(&mut arena, Some(root));
        let sibling = node(&mut arena, Some(root));

        let h = reg.handle();
        reg.register(h.id(), inner);
        h.focus();

        assert!(
            reg.is_focus_within(&arena, root),
            "the root is focus-within (ancestor of the focused inner)"
        );
        assert!(
            reg.is_focus_within(&arena, inner),
            "the focused node itself is focus-within"
        );
        assert!(
            !reg.is_focus_within(&arena, sibling),
            "a sibling of the focused node is not focus-within"
        );

        drop(owner);
    }
}
