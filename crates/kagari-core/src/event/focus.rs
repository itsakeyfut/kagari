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
use std::sync::{Arc, Mutex, MutexGuard};

use kagari_base::NodeId;

use crate::arena::Arena;
use crate::event::ancestor_path;
use crate::reactive::prelude::*;
use crate::reactive::{RwSignal, use_context};

/// A focusable target's identity, independent of the arena [`NodeId`] (gpui model). Minted by
/// [`FocusRegistry::handle`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FocusId(u64);

/// A clone-able handle to a focusable target. Holds the [`FocusId`] plus the registry's current-focus
/// and focus-visible signals, so [`is_focused`](Self::is_focused) / [`is_focus_visible`](Self::is_focus_visible)
/// / [`focus`](Self::focus) work standalone (no context needed). Bind it to an element with
/// `div().track_focus(&handle)`.
#[derive(Clone)]
pub struct FocusHandle {
    id: FocusId,
    current: RwSignal<Option<FocusId>>,
    visible: RwSignal<bool>,
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

    /// Whether this handle currently holds focus, read **without** subscribing (#298). Use this in a
    /// `paint` (not a reactive context) after separately subscribing to focus for repaint — reading the
    /// tracked [`is_focused`](Self::is_focused) outside a tracking context warns and would miss updates.
    pub fn is_focused_untracked(&self) -> bool {
        self.current.get_untracked() == Some(self.id)
    }

    /// Whether this handle holds focus **and** the focus indicator should be shown — i.e. the most
    /// recent input was a keyboard, not a pointer (the `:focus-visible` heuristic). A **tracked** read
    /// (of both the focus and focus-visible signals), so it drives fine-grained repaint. Use this for
    /// focus-ring styling so the ring appears on keyboard focus and stays hidden for pointer focus.
    pub fn is_focus_visible(&self) -> bool {
        self.is_focused() && self.visible.get()
    }

    /// Moves focus to this handle's target.
    pub fn focus(&self) {
        self.current.set(Some(self.id));
    }

    /// A standalone handle backed by its own focus signal, not registered with any [`FocusRegistry`]
    /// (so it does not participate in tab order). Returned by [`use_focus_handle`] when no focus context
    /// is present. Creates an [`RwSignal`], so it must be called under an ambient reactive `Owner` (like
    /// [`FocusRegistry::new`]).
    fn detached() -> Self {
        FocusHandle {
            id: FocusId(u64::MAX),
            current: RwSignal::new(None),
            visible: RwSignal::new(false),
        }
    }
}

/// The interior-mutable state of a [`FocusRegistry`], shared across clones: the next id to mint, the
/// tab-order list (in `handle()` call order = tree order), and the `FocusId → NodeId` map populated at
/// build by `track_focus`.
struct FocusInner {
    next: u64,
    order: Vec<FocusId>,
    nodes: HashMap<FocusId, NodeId>,
}

/// Per-window focus state: the current-focus signal plus a shared, cloneable inner (next id / tab order /
/// `FocusId → NodeId` map). Cloning shares the same inner and the same (`Copy`) focus signal, so a clone
/// handed to a view via context (see [`use_focus_handle`]) mints into the very tab order the window uses
/// for navigation and [`focused_node`](Self::focused_node).
///
/// The shared state is behind `Arc<Mutex<..>>` (not `Rc<RefCell<..>>`) so the registry is `Send + Sync`
/// for `provide_context`; the UI/reactive runtime is thread-local, so the mutex is uncontended.
#[derive(Clone)]
pub struct FocusRegistry {
    current: RwSignal<Option<FocusId>>,
    /// Whether the focus indicator should be shown — set by the App at the input boundary to the most
    /// recent modality (`true` on a key press, `false` on a pointer press): the `:focus-visible` state.
    visible: RwSignal<bool>,
    inner: Arc<Mutex<FocusInner>>,
}

impl FocusRegistry {
    /// Creates an empty registry. Creates the current-focus and focus-visible [`RwSignal`]s, so it must
    /// be called under an ambient reactive `Owner` (the app root owner; tests establish one — RK-005).
    pub fn new() -> Self {
        Self {
            current: RwSignal::new(None),
            visible: RwSignal::new(false),
            inner: Arc::new(Mutex::new(FocusInner {
                next: 0,
                order: Vec::new(),
                nodes: HashMap::new(),
            })),
        }
    }

    /// Locks the shared inner, recovering a poisoned mutex rather than panicking (the invariants are
    /// simple counters/maps, so a panic while holding the lock leaves the state usable).
    fn inner(&self) -> MutexGuard<'_, FocusInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current-focus signal (e.g. for the app to read/observe the focused id).
    pub fn current(&self) -> RwSignal<Option<FocusId>> {
        self.current
    }

    /// Sets whether the focus indicator should be shown (the `:focus-visible` state). The App calls this
    /// at the input boundary: `true` on a key press (keyboard modality), `false` on a pointer press. The
    /// widget focus ring reads [`FocusHandle::is_focus_visible`], so it appears on keyboard focus and
    /// stays hidden for pointer focus. Deduped against the current value: key auto-repeat and pointer
    /// floods would otherwise write the signal every event, each notifying subscribers and marking
    /// paint-dirty — only a modality *change* should repaint.
    pub fn set_focus_visible(&self, visible: bool) {
        if self.visible.get_untracked() != visible {
            self.visible.set(visible);
        }
    }

    /// Mints a new [`FocusHandle`] and appends it to the tab order. Components mint handles in
    /// pre-order (parent before children), so the tab order is tree order.
    pub fn handle(&self) -> FocusHandle {
        let mut inner = self.inner();
        let id = FocusId(inner.next);
        inner.next += 1;
        inner.order.push(id);
        FocusHandle {
            id,
            current: self.current,
            visible: self.visible,
        }
    }

    /// Associates a [`FocusId`] with its arena [`NodeId`] (called at build by `track_focus`), so
    /// keyboard dispatch and focus-within can resolve the focused node.
    pub fn register(&self, id: FocusId, node: NodeId) {
        self.inner().nodes.insert(id, node);
    }

    /// The arena node of the currently-focused target, if any is focused and registered.
    pub fn focused_node(&self) -> Option<NodeId> {
        let id = self.current.get_untracked()?;
        self.inner().nodes.get(&id).copied()
    }

    /// Deregisters every focusable whose node is within `root`'s subtree — a live unmount (#278). Removes
    /// their `FocusId → NodeId` entries and tab-order slots, and clears the current focus if it pointed at
    /// a removed target (so keyboard dispatch cannot resolve a now-dead node, and the tab order / node map
    /// do not grow unbounded across mount/unmount cycles). Uses the arena while the subtree still exists,
    /// so call it **before** `arena.remove_subtree`. No-op if the subtree holds no focusables.
    pub fn deregister_subtree(&self, arena: &Arena, root: NodeId) {
        let removed: Vec<FocusId> = {
            let mut inner = self.inner();
            let removed: Vec<FocusId> = inner
                .nodes
                .iter()
                .filter(|&(_, &node)| is_within(arena, node, root))
                .map(|(&id, _)| id)
                .collect();
            for id in &removed {
                inner.nodes.remove(id);
            }
            inner.order.retain(|id| !removed.contains(id));
            removed
        };
        // Clear focus if it pointed at a removed target. Done outside the lock: `set` runs subscribers
        // synchronously and they may read the registry (the mutex is not reentrant), as in `step`.
        if self
            .current
            .get_untracked()
            .is_some_and(|cur| removed.contains(&cur))
        {
            self.current.set(None);
        }
    }

    /// Moves focus to the next focusable in tab order (wrapping); from no focus, the first.
    pub fn focus_next(&self) {
        self.step(1);
    }

    /// Moves focus to the previous focusable in tab order (wrapping); from no focus, the last.
    pub fn focus_prev(&self) {
        self.step(-1);
    }

    /// Moves focus to the next focusable **within** `scope_root`'s subtree (wrapping) — the focus-trap
    /// tab order (#219). Only focusables whose registered node is a descendant-or-self of `scope_root`
    /// participate; from focus outside the scope, enters at the first. A no-op when the scope has no
    /// registered focusables.
    pub fn focus_next_within(&self, arena: &Arena, scope_root: NodeId) {
        self.step_within(arena, scope_root, 1);
    }

    /// Moves focus to the previous focusable within `scope_root`'s subtree (wrapping); the focus-trap
    /// analogue of [`focus_prev`](Self::focus_prev).
    pub fn focus_prev_within(&self, arena: &Arena, scope_root: NodeId) {
        self.step_within(arena, scope_root, -1);
    }

    /// Advances the focus by `delta` (+1 next / -1 prev) over the tab order, wrapping.
    fn step(&self, delta: isize) {
        let inner = self.inner();
        if inner.order.is_empty() {
            return;
        }
        let len = inner.order.len();
        let next = match self.current.get_untracked() {
            Some(cur) => match inner.order.iter().position(|&id| id == cur) {
                // (cur + delta) mod len, with wrapping handled via `rem_euclid`.
                Some(i) => (i as isize + delta).rem_euclid(len as isize) as usize,
                None => 0,
            },
            // No focus yet: next → first, prev → last.
            None if delta >= 0 => 0,
            None => len - 1,
        };
        let target = inner.order[next];
        // Release the lock before writing the signal: `set` runs subscribers synchronously (they may
        // read the registry, e.g. `focused_node`), and the mutex is not reentrant.
        drop(inner);
        self.current.set(Some(target));
    }

    /// Advances focus by `delta` over only the focusables inside `scope_root`'s subtree (the focus-trap
    /// tab order), wrapping. Filters the flat tab order to ids whose registered node is a
    /// descendant-or-self of `scope_root`.
    fn step_within(&self, arena: &Arena, scope_root: NodeId, delta: isize) {
        let inner = self.inner();
        let scoped: Vec<FocusId> = inner
            .order
            .iter()
            .copied()
            .filter(|id| {
                inner
                    .nodes
                    .get(id)
                    .is_some_and(|&node| is_within(arena, node, scope_root))
            })
            .collect();
        if scoped.is_empty() {
            return;
        }
        let len = scoped.len();
        let next = match self
            .current
            .get_untracked()
            .and_then(|cur| scoped.iter().position(|&id| id == cur))
        {
            // (cur + delta) mod len, wrapping within the scope.
            Some(i) => (i as isize + delta).rem_euclid(len as isize) as usize,
            // No focus, or focus outside the scope: enter at the first (next) / last (prev).
            None if delta >= 0 => 0,
            None => len - 1,
        };
        let target = scoped[next];
        drop(inner);
        self.current.set(Some(target));
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

/// Whether `node` is `scope_root` or a descendant of it, walking parents without allocating (unlike
/// `ancestor_path`, which builds a `Vec`). The focus-trap filter tests this per focusable on every Tab
/// (#219), so it stays allocation-free and short-circuits on the first match. `pub(crate)` so the cursor
/// registry's subtree deregistration (#278) reuses it instead of allocating via `ancestor_path`.
pub(crate) fn is_within(arena: &Arena, mut node: NodeId, scope_root: NodeId) -> bool {
    loop {
        if node == scope_root {
            return true;
        }
        match arena.get(node).and_then(|n| n.parent) {
            Some(parent) => node = parent,
            None => return false,
        }
    }
}

/// Obtains a [`FocusHandle`] from the [`FocusRegistry`] provided in the current reactive context. The App
/// provides one per window at window creation, so any view built under the window owner can mint a handle
/// without the caller wiring a registry. Call this during view construction: mint order equals
/// construction order equals tree order, so the handle takes its place in the window's tab order.
///
/// When no focus context is present (e.g. a GPU-free unit test with no App), this returns a **detached**
/// handle — backed by its own focus signal, usable standalone (`is_focused`/`focus` work) but not part of
/// any tab order — rather than panicking.
pub fn use_focus_handle() -> FocusHandle {
    match use_context::<FocusRegistry>() {
        Some(registry) => registry.handle(),
        None => FocusHandle::detached(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::{Arena, Node};
    use crate::reactive::{Owner, provide_context};

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

        let reg = FocusRegistry::new();
        let a = reg.handle();
        let b = reg.handle();

        assert!(!a.is_focused(), "nothing focused initially");
        a.focus();
        assert!(a.is_focused(), "a is focused after focus()");
        assert!(!b.is_focused(), "b is not focused");

        drop(owner);
    }

    #[test]
    fn is_focus_visible_should_require_focus_and_keyboard_modality() {
        let owner = Owner::new();
        owner.set();

        let reg = FocusRegistry::new();
        let a = reg.handle();
        a.focus();

        // Focused, but no keyboard input yet (default modality) → the ring must not show.
        assert!(a.is_focused(), "a holds focus");
        assert!(
            !a.is_focus_visible(),
            "focus alone does not make the indicator visible (pointer/default modality)"
        );

        // A key press marks the modality keyboard → the ring shows.
        reg.set_focus_visible(true);
        assert!(
            a.is_focus_visible(),
            "keyboard modality shows the indicator"
        );

        // A pointer press hides it again, even while still focused.
        reg.set_focus_visible(false);
        assert!(
            !a.is_focus_visible(),
            "pointer modality hides the indicator while focus stays"
        );

        // With the keyboard modality set, losing focus still hides it (both conditions required).
        reg.set_focus_visible(true);
        reg.current().set(None);
        assert!(
            !a.is_focus_visible(),
            "no focus means no indicator regardless of modality"
        );

        drop(owner);
    }

    #[test]
    fn set_focus_visible_should_dedupe_unchanged() {
        let owner = Owner::new();
        owner.set();

        let reg = FocusRegistry::new();
        let a = reg.handle();
        a.focus();
        reg.set_focus_visible(true);
        // Repeating the same modality (as key auto-repeat does) leaves the state true — the dedupe is a
        // repaint optimization, not a behavior change.
        reg.set_focus_visible(true);
        assert!(
            a.is_focus_visible(),
            "repeated same-modality stamps are stable"
        );

        drop(owner);
    }

    #[test]
    fn tab_should_move_focus_in_tree_order() {
        let owner = Owner::new();
        owner.set();

        let reg = FocusRegistry::new();
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

        let reg = FocusRegistry::new();
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

    #[test]
    fn use_focus_handle_should_mint_a_focusable_handle() {
        let owner = Owner::new();
        owner.set();

        // A registry provided in context (as the App does per window) lets a view mint a handle that
        // shares the window's focus state — no caller-side wiring.
        let reg = FocusRegistry::new();
        provide_context(reg.clone());

        let mut arena = Arena::new();
        let n = node(&mut arena, None);

        let h = use_focus_handle();
        reg.register(h.id(), n);
        h.focus();

        assert!(
            h.is_focused(),
            "the context-minted handle reports focus after focus()"
        );
        assert_eq!(
            reg.focused_node(),
            Some(n),
            "the registry resolves the focused node via the shared inner"
        );

        // The handle also took its place in the shared tab order.
        reg.current().set(None);
        reg.focus_next();
        assert!(
            h.is_focused(),
            "focus_next from none selects the context-minted handle (it is in tab order)"
        );

        drop(owner);
    }

    #[test]
    fn focus_next_within_should_cycle_within_scope() {
        let owner = Owner::new();
        owner.set();

        // Arena: root → { outside_node, scope_root → { a_node, b_node } }.
        let mut arena = Arena::new();
        let root = node(&mut arena, None);
        let outside_node = node(&mut arena, Some(root));
        let scope_root = node(&mut arena, Some(root));
        let a_node = node(&mut arena, Some(scope_root));
        let b_node = node(&mut arena, Some(scope_root));

        let reg = FocusRegistry::new();
        // Flat tab order (mint order): outside, a, b. `a`/`b` are inside the scope; `outside` is not.
        let outside = reg.handle();
        let a = reg.handle();
        let b = reg.handle();
        reg.register(outside.id(), outside_node);
        reg.register(a.id(), a_node);
        reg.register(b.id(), b_node);

        // Confined nav visits only the scope's focusables (a, b), skipping `outside`, and wraps.
        reg.focus_next_within(&arena, scope_root);
        assert!(
            a.is_focused(),
            "confined next enters the scope at its first focusable"
        );
        reg.focus_next_within(&arena, scope_root);
        assert!(b.is_focused(), "advances to the scope's second focusable");
        reg.focus_next_within(&arena, scope_root);
        assert!(
            a.is_focused(),
            "wraps within the scope (never leaves to the outside focusable)"
        );
        reg.focus_prev_within(&arena, scope_root);
        assert!(b.is_focused(), "prev wraps to the scope's last focusable");

        drop(owner);
    }

    #[test]
    fn focus_next_within_should_enter_scope_from_outside_focus() {
        let owner = Owner::new();
        owner.set();

        // The real focus-trap entry: an overlay opens while focus is still on its outside trigger; the
        // first Tab must move focus *into* the scope (the `current = outside`, not-in-scope fallthrough).
        let mut arena = Arena::new();
        let root = node(&mut arena, None);
        let outside_node = node(&mut arena, Some(root));
        let scope_root = node(&mut arena, Some(root));
        let a_node = node(&mut arena, Some(scope_root));
        let b_node = node(&mut arena, Some(scope_root));

        let reg = FocusRegistry::new();
        let outside = reg.handle();
        let a = reg.handle();
        let b = reg.handle();
        reg.register(outside.id(), outside_node);
        reg.register(a.id(), a_node);
        reg.register(b.id(), b_node);

        // Focus starts *outside* the scope.
        outside.focus();
        assert!(outside.is_focused(), "focus starts on the outside trigger");

        reg.focus_next_within(&arena, scope_root);
        assert!(
            a.is_focused(),
            "Tab enters the scope at its first focusable (focus was outside the scope)"
        );
        reg.focus_prev_within(&arena, scope_root);
        assert!(
            b.is_focused(),
            "Shift+Tab from outside-focus enters at the scope's last focusable"
        );

        drop(owner);
    }

    #[test]
    fn focus_next_within_on_empty_scope_should_be_noop() {
        let owner = Owner::new();
        owner.set();

        // A trap scope with no registered focusables: nav is a no-op (focus unchanged), no panic.
        let mut arena = Arena::new();
        let root = node(&mut arena, None);
        let empty_scope = node(&mut arena, Some(root));
        let other_node = node(&mut arena, Some(root));

        let reg = FocusRegistry::new();
        let other = reg.handle();
        reg.register(other.id(), other_node);
        other.focus();

        reg.focus_next_within(&arena, empty_scope);
        assert_eq!(
            reg.current().get_untracked(),
            Some(other.id()),
            "navigating an empty trap scope leaves focus unchanged"
        );

        drop(owner);
    }

    #[test]
    fn deregister_subtree_should_remove_focusables_and_clear_stale_focus() {
        // #278: a live unmount deregisters the removed subtree's focusables and clears focus if it
        // pointed at one, so keyboard dispatch cannot resolve a dead node and tab order does not leak.
        let owner = Owner::new();
        owner.set();
        let mut arena = Arena::new();
        let root = node(&mut arena, None);
        let scope = node(&mut arena, Some(root));
        let inner_node = node(&mut arena, Some(scope));
        let outside = node(&mut arena, Some(root));

        let reg = FocusRegistry::new();
        let h_in = reg.handle();
        let h_out = reg.handle();
        reg.register(h_in.id(), inner_node);
        reg.register(h_out.id(), outside);
        h_in.focus();
        assert_eq!(reg.focused_node(), Some(inner_node));

        reg.deregister_subtree(&arena, scope);
        assert_eq!(
            reg.focused_node(),
            None,
            "focus on a removed target is cleared"
        );
        // The removed focusable is out of tab order; nav now visits only the survivor.
        reg.focus_next();
        assert!(
            h_out.is_focused(),
            "only the surviving focusable remains in tab order"
        );
        drop(owner);
    }

    #[test]
    fn use_focus_handle_should_return_detached_handle_without_context() {
        let owner = Owner::new();
        owner.set();

        // No FocusRegistry in context (e.g. a GPU-free test): a usable detached handle, not a panic.
        let h = use_focus_handle();
        assert!(!h.is_focused(), "a fresh detached handle is unfocused");
        h.focus();
        assert!(
            h.is_focused(),
            "the detached handle drives its own focus signal standalone"
        );

        drop(owner);
    }
}
