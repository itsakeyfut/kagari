//! Dynamic reactive scopes (#32): build static structure once, rebuild only the dynamic parts.
//!
//! [`DynList`] (keyed list) and [`DynIf`] (conditional subtree) are reactive scopes. Their effect
//! is a **cheap marker** (ADR 0001 / §1.4): it reads/subscribes the collection or condition,
//! stages the new value, and flags structure-dirty. The heavy work — diffing keys and
//! inserting/removing arena nodes — happens in [`DynList::reconcile`], driven by the frame-loop
//! scheduler (#36); tests call it explicitly.
//!
//! Each dynamic child is built under a per-child [`Owner`] so that removing it disposes its own
//! reactive effects (e.g. a child's reactive `bg`) — resolving the RK-005 cleanup gap. Unchanged
//! children keep their stable `NodeId` (#30) and arena state; gpui-style full-tree re-run is not
//! done (§1.3).

use std::sync::{Arc, Mutex};

use kagari_base::{NodeId, Point, Rect};
use kagari_layout::LayoutStyle;

use super::{AnyElement, Element, Event, EventCx, IntoElement, LayoutCx, PaintCx};
use crate::arena::Node;
// `Owner` via the reactive seam (not `reactive_graph` directly) so the runtime stays an
// implementation detail behind `crate::reactive`.
use crate::reactive::{Owner, create_effect};

/// A keyed child: its key, arena node, owning reactive scope, and retained element.
type KeyedChild<K> = (K, NodeId, Owner, AnyElement);

/// Deregisters a removed subtree's focus/cursor entries **before** the arena is torn down, so a live
/// unmount (#278) leaves no dangling `FocusId → dead NodeId` or stale cursor and the registries do not
/// grow across mount/unmount cycles. A no-op when the app has not attached the registries (GPU-free
/// tests pass `None`). `arena`/`focus`/`cursor` are disjoint `LayoutCx` fields (split borrow).
fn deregister_focus_cursor(cx: &mut LayoutCx, root: NodeId) {
    let arena = &*cx.arena;
    if let Some(reg) = cx.focus.as_deref_mut() {
        reg.deregister_subtree(arena, root);
    }
    if let Some(reg) = cx.cursor.as_deref_mut() {
        reg.deregister_subtree(arena, root);
    }
}

/// A keyed, reactive list. Builds a child per item; on a collection-signal change only the
/// changed children are rebuilt (keyed reconciliation), unchanged children keep their nodes.
pub struct DynList<Item, K> {
    items_fn: Arc<dyn Fn() -> Vec<Item> + Send + Sync>,
    key_fn: Box<dyn Fn(&Item) -> K>,
    view_fn: Box<dyn Fn(&Item) -> AnyElement>,
    children: Vec<KeyedChild<K>>,
    /// Items staged by the effect, consumed by [`DynList::apply_staged`]. Also the single source of
    /// truth for "a change is pending" ([`is_dirty`](Self::is_dirty) derives from it, #278).
    pending: Arc<Mutex<Option<Vec<Item>>>>,
    /// This list's reactive scope; child owners are created under it.
    owner: Option<Owner>,
    id: Option<NodeId>,
}

/// Creates a keyed reactive list.
///
/// `items_fn` produces the current items (read inside the effect to subscribe), `key_fn` gives
/// each item a stable identity, and `view_fn` builds an element for an item.
///
/// `key_fn` must return a **unique** key per item: duplicate keys are not stable identities, so
/// the second and later occurrences of a key are rebuilt on every reconcile.
pub fn dyn_list<Item, K, V>(
    items_fn: impl Fn() -> Vec<Item> + Send + Sync + 'static,
    key_fn: impl Fn(&Item) -> K + 'static,
    view_fn: impl Fn(&Item) -> V + 'static,
) -> DynList<Item, K>
where
    Item: Send + 'static,
    K: PartialEq + 'static,
    V: IntoElement,
{
    DynList {
        items_fn: Arc::new(items_fn),
        key_fn: Box::new(key_fn),
        view_fn: Box::new(move |item| view_fn(item).into_element()),
        children: Vec::new(),
        pending: Arc::new(Mutex::new(None)),
        owner: None,
        id: None,
    }
}

impl<Item, K> DynList<Item, K>
where
    Item: Send + 'static,
    K: PartialEq + 'static,
{
    /// Whether a collection change is staged and not yet applied — derived from `pending` (the single
    /// source of truth), so it cannot drift from the staged payload (#278). Tests poll it.
    pub fn is_dirty(&self) -> bool {
        self.pending.lock().map(|p| p.is_some()).unwrap_or(false)
    }

    /// The realized keyed children, for a virtualizing parent ([`VirtualizedList`](super::VirtualizedList))
    /// to paint at custom offset-based positions — bypassing the taffy-driven [`Element::paint`].
    /// Yields `(key, node_id, element)` in realized order; the child `Owner` is elided.
    pub(crate) fn realized_children(
        &mut self,
    ) -> impl Iterator<Item = (&K, NodeId, &mut AnyElement)> {
        self.children
            .iter_mut()
            .map(|(k, id, _, el)| (&*k, *id, el))
    }

    /// Applies the staged items: a keyed diff against the current children. New keys are built
    /// under a fresh child [`Owner`]; gone keys have their owner cleaned up (disposing their
    /// effects) and all three ownership planes freed (reactive owner + arena subtree + layout subtree)
    /// plus their focus/cursor registrations deregistered (#278); existing keys keep everything. Called
    /// by [`reconcile`](Element::reconcile) (frame loop) and once at build; a `None` `pending` is a no-op.
    pub(crate) fn apply_staged(&mut self, cx: &mut LayoutCx) {
        let items = match self.pending.lock() {
            Ok(mut pending) => pending.take(),
            Err(_) => return,
        };
        let Some(items) = items else {
            return;
        };

        let owner = self.owner.clone().unwrap_or_default();
        let mut old = std::mem::take(&mut self.children);
        // Snapshot the previous child ids so a no-op re-stage (same keys, same order — e.g. a sub-row
        // VirtualizedList scroll) skips the parent relayout below (review M1).
        let prev_ids: Vec<NodeId> = old.iter().map(|(_, id, ..)| *id).collect();
        let mut next = Vec::with_capacity(items.len());

        // Keyed diff. Linear `position` + `remove` is O(n^2) in the list length; fine for the
        // small-to-moderate lists this targets. A `HashMap<K, _>` index (needs `K: Hash`) is the
        // O(n) follow-up for huge lists (e.g. timeline clips) — see the milestone follow-up.
        for item in &items {
            let key = (self.key_fn)(item);
            if let Some(pos) = old.iter().position(|(k, ..)| *k == key) {
                // Unchanged identity: keep the node, owner, and element.
                next.push(old.remove(pos));
            } else {
                // New child: build it under its own owner so removal can dispose its effects.
                let child_owner = owner.child();
                let (node_id, element) = child_owner.with(|| {
                    let mut element = (self.view_fn)(item);
                    let node_id = element.request_layout(cx);
                    (node_id, element)
                });
                if let Some(node) = cx.arena.get_mut(node_id) {
                    node.parent = self.id;
                }
                next.push((key, node_id, child_owner, element));
            }
        }

        // Whatever remains in `old` was removed: deregister its focus/cursor (before the arena is torn
        // down), dispose its reactive scope (effects), then free its whole layout + arena subtree —
        // single-node `remove` would leak descendants' slots / taffy nodes otherwise (#278, RK-006).
        for (_, node_id, child_owner, _element) in old {
            deregister_focus_cursor(cx, node_id);
            child_owner.cleanup();
            cx.layout.remove_subtree(node_id);
            cx.arena.remove_subtree(node_id);
        }

        let ids: Vec<NodeId> = next.iter().map(|(_, node_id, ..)| *node_id).collect();
        // Only re-link + relayout the parent when the child set actually changed (membership or order);
        // a same-value re-stage must not churn (review M1). `ids != prev_ids` iff a row was added, removed,
        // or reordered — all of which need `set_children` + a parent relayout; an unchanged set skips both.
        if ids != prev_ids {
            if let Some(parent_id) = self.id {
                if let Some(node) = cx.arena.get_mut(parent_id) {
                    node.children = ids.clone();
                }
                cx.layout.set_children(parent_id, &ids);
                // The structural change relays out the parent (new/removed children) and feeds the damage
                // rect for a localized partial redraw (#69) instead of the node-less structure signal.
                cx.damage.mark_layout_dirty(parent_id);
            }
        }
        self.children = next;
    }
}

impl<Item, K> Element for DynList<Item, K>
where
    Item: Send + 'static,
    K: PartialEq + 'static,
{
    fn request_layout(&mut self, cx: &mut LayoutCx) -> NodeId {
        if let Some(id) = self.id {
            return id;
        }
        let id = cx.arena.insert(Node::default());
        self.id = Some(id);
        cx.layout.insert(id, None);
        cx.layout.set_style(id, &LayoutStyle::default());
        // Scope child owners under the ambient owner so an ancestor's removal disposes this whole
        // dyn node. Callers build under an owner (App root #36 / tests); the detached-owner
        // fallback is only the no-ambient-owner degenerate case.
        self.owner = Some(Owner::current().unwrap_or_default());

        // Staging effect (registered under the current owner): subscribe to the collection, stage the
        // items, and mark structure-dirty on the damage sink so the frame loop wakes and runs a
        // reconcile pass (#278). The heavy apply is separate (`apply_staged`).
        let pending = Arc::clone(&self.pending);
        let items_fn = Arc::clone(&self.items_fn);
        let damage = Arc::clone(&cx.damage);
        create_effect(move || {
            if let Ok(mut slot) = pending.lock() {
                *slot = Some(items_fn());
            }
            damage.mark_structure_dirty(id);
        });

        // Build the initial children from what the effect just staged (build-once initial mount).
        self.apply_staged(cx);
        id
    }

    fn paint(&mut self, bounds: Rect, cx: &mut PaintCx) {
        // Paint each child at its absolute bounds (layout coords are parent-relative).
        for (_, node_id, _, element) in &mut self.children {
            let local = cx.layout.layout(*node_id);
            let child_bounds = Rect {
                origin: Point {
                    x: bounds.origin.x + local.origin.x,
                    y: bounds.origin.y + local.origin.y,
                },
                size: local.size,
            };
            element.paint(child_bounds, cx);
        }
    }

    fn handle_event(&mut self, _ev: &Event, _cx: &mut EventCx) {}

    fn reconcile(&mut self, cx: &mut LayoutCx) {
        // Apply this list's staged collection change (mount/unmount rows), then recurse so nested dyn
        // nodes inside the (possibly newly-built) rows reconcile in the same pass (#278).
        self.apply_staged(cx);
        for (_, _, _, element) in &mut self.children {
            element.reconcile(cx);
        }
    }
}

impl<Item, K> IntoElement for DynList<Item, K>
where
    Item: Send + 'static,
    K: PartialEq + 'static,
{
    fn into_element(self) -> AnyElement {
        Box::new(self)
    }
}

/// A conditional subtree: mounts/unmounts a child element on a boolean signal.
pub struct DynIf {
    cond_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    view_fn: Box<dyn Fn() -> AnyElement>,
    child: Option<(NodeId, Owner, AnyElement)>,
    /// Staged condition, consumed by [`DynIf::apply_staged`]; also the source of truth for
    /// [`is_dirty`](Self::is_dirty) (#278).
    pending: Arc<Mutex<Option<bool>>>,
    owner: Option<Owner>,
    id: Option<NodeId>,
}

/// Creates a conditional subtree: `view_fn`'s element is mounted while `cond_fn` is true.
pub fn dyn_if<V>(
    cond_fn: impl Fn() -> bool + Send + Sync + 'static,
    view_fn: impl Fn() -> V + 'static,
) -> DynIf
where
    V: IntoElement,
{
    DynIf {
        cond_fn: Arc::new(cond_fn),
        view_fn: Box::new(move || view_fn().into_element()),
        child: None,
        pending: Arc::new(Mutex::new(None)),
        owner: None,
        id: None,
    }
}

impl DynIf {
    /// Whether a condition change is staged and not yet applied — derived from `pending` (#278).
    pub fn is_dirty(&self) -> bool {
        self.pending.lock().map(|p| p.is_some()).unwrap_or(false)
    }

    /// Mounts the child when the staged condition is true and it is absent; unmounts when false and
    /// present — freeing all three ownership planes (reactive owner + arena + layout subtree) and
    /// deregistering the removed subtree's focus/cursor (#278). Called by [`reconcile`](Element::reconcile)
    /// (frame loop) and once at build; a `None` `pending` is a no-op.
    pub(crate) fn apply_staged(&mut self, cx: &mut LayoutCx) {
        let cond = match self.pending.lock() {
            Ok(mut pending) => pending.take(),
            Err(_) => return,
        };
        let Some(cond) = cond else {
            return;
        };

        let owner = self.owner.clone().unwrap_or_default();
        // `changed` is true only on an actual mount/unmount arm; a re-stage of the same condition
        // (already-mounted / already-absent) must not relayout the parent (review M1).
        let changed = match (cond, self.child.take()) {
            (true, None) => {
                let child_owner = owner.child();
                let (node_id, element) = child_owner.with(|| {
                    let mut element = (self.view_fn)();
                    let node_id = element.request_layout(cx);
                    (node_id, element)
                });
                if let Some(node) = cx.arena.get_mut(node_id) {
                    node.parent = self.id;
                }
                self.child = Some((node_id, child_owner, element));
                true
            }
            (false, Some((node_id, child_owner, _element))) => {
                deregister_focus_cursor(cx, node_id);
                child_owner.cleanup();
                cx.layout.remove_subtree(node_id);
                cx.arena.remove_subtree(node_id);
                true
            }
            // Already mounted (true) or already absent (false): keep state, no structural change.
            (true, existing @ Some(_)) => {
                self.child = existing;
                false
            }
            (false, None) => false,
        };

        if changed {
            let ids: Vec<NodeId> = self.child.iter().map(|(node_id, ..)| *node_id).collect();
            if let Some(parent_id) = self.id {
                if let Some(node) = cx.arena.get_mut(parent_id) {
                    node.children = ids.clone();
                }
                cx.layout.set_children(parent_id, &ids);
                // Relayout the parent + feed the damage rect for a localized redraw (#69/#278).
                cx.damage.mark_layout_dirty(parent_id);
            }
        }
    }
}

impl Element for DynIf {
    fn request_layout(&mut self, cx: &mut LayoutCx) -> NodeId {
        if let Some(id) = self.id {
            return id;
        }
        let id = cx.arena.insert(Node::default());
        self.id = Some(id);
        cx.layout.insert(id, None);
        cx.layout.set_style(id, &LayoutStyle::default());
        // Scope child owners under the ambient owner so an ancestor's removal disposes this whole
        // dyn node. Callers build under an owner (App root #36 / tests); the detached-owner
        // fallback is only the no-ambient-owner degenerate case.
        self.owner = Some(Owner::current().unwrap_or_default());

        let pending = Arc::clone(&self.pending);
        let cond_fn = Arc::clone(&self.cond_fn);
        let damage = Arc::clone(&cx.damage);
        create_effect(move || {
            if let Ok(mut slot) = pending.lock() {
                *slot = Some(cond_fn());
            }
            damage.mark_structure_dirty(id);
        });

        self.apply_staged(cx);
        id
    }

    fn paint(&mut self, bounds: Rect, cx: &mut PaintCx) {
        if let Some((node_id, _, element)) = &mut self.child {
            let local = cx.layout.layout(*node_id);
            let child_bounds = Rect {
                origin: Point {
                    x: bounds.origin.x + local.origin.x,
                    y: bounds.origin.y + local.origin.y,
                },
                size: local.size,
            };
            element.paint(child_bounds, cx);
        }
    }

    fn handle_event(&mut self, _ev: &Event, _cx: &mut EventCx) {}

    fn reconcile(&mut self, cx: &mut LayoutCx) {
        // Apply this node's staged mount/unmount, then recurse into the (possibly newly-mounted) child
        // so nested dyn nodes reconcile in the same pass (#278).
        self.apply_staged(cx);
        if let Some((_, _, element)) = &mut self.child {
            element.reconcile(cx);
        }
    }
}

impl IntoElement for DynIf {
    fn into_element(self) -> AnyElement {
        Box::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::Arena;
    use crate::damage::DamageState;
    use crate::element::{DamageSink, div};
    use crate::event::{
        CursorIcon, CursorRegistry, FocusRegistry, HitRegion, HitTest, InteractFlags,
    };
    use crate::paint::render_tree;
    use crate::reactive::rx;
    use kagari_base::{Color, Size};
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};
    use reactive_graph::owner::Owner;
    use reactive_graph::prelude::*;
    use reactive_graph::signal::signal;

    const VIEWPORT: Size = Size { w: 200.0, h: 200.0 };

    /// Drives one full `render_tree` frame (no manual `reconcile`/`apply_staged`) with the focus and
    /// cursor registries threaded, returning the root node id — so a test can prove `dyn_if`/`dyn_list`
    /// mount/unmount **live** off the frame loop (#278), including the unmount's focus/cursor deregister.
    #[allow(clippy::too_many_arguments)]
    fn frame_full(
        root: &mut AnyElement,
        arena: &mut Arena,
        layout: &mut LayoutTree,
        text: &mut TextSystem,
        damage: &Arc<DamageState>,
        focus: Option<&mut FocusRegistry>,
        cursor: Option<&mut CursorRegistry>,
    ) -> NodeId {
        let mut scene = Scene::new();
        render_tree(
            root,
            arena,
            layout,
            text,
            None,
            None,
            None,
            focus,
            cursor,
            None,
            &mut scene,
            VIEWPORT,
            damage,
            &Theme::default(),
        )
        .unwrap()
    }

    /// `frame_full` with no cursor registry (the common case).
    fn frame(
        root: &mut AnyElement,
        arena: &mut Arena,
        layout: &mut LayoutTree,
        text: &mut TextSystem,
        damage: &Arc<DamageState>,
        focus: Option<&mut FocusRegistry>,
    ) -> NodeId {
        frame_full(root, arena, layout, text, damage, focus, None)
    }

    /// Counts `root` and all its arena descendants — so a container-recursion guard can assert a nested
    /// `dyn_if` mounted exactly one node, uniformly across containers (including `overlay`, whose child
    /// paints deferred so a scene-quad count would miss it).
    fn count_subtree(arena: &Arena, root: NodeId) -> usize {
        1 + arena
            .get(root)
            .map(|n| {
                n.children
                    .iter()
                    .map(|&c| count_subtree(arena, c))
                    .sum::<usize>()
            })
            .unwrap_or(0)
    }

    struct NoopDamage;
    impl DamageSink for NoopDamage {
        fn mark_paint_dirty(&self, _id: NodeId) {}
    }

    #[derive(Default)]
    struct RecordingDamage {
        dirtied: Mutex<Vec<NodeId>>,
    }
    impl RecordingDamage {
        fn count(&self) -> usize {
            self.dirtied.lock().map(|v| v.len()).unwrap_or(0)
        }
    }
    impl DamageSink for RecordingDamage {
        fn mark_paint_dirty(&self, id: NodeId) {
            if let Ok(mut v) = self.dirtied.lock() {
                v.push(id);
            }
        }
    }

    /// A reusable build environment so tests can construct a `LayoutCx` for `reconcile`.
    struct Env {
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        damage: Arc<dyn DamageSink>,
        theme: kagari_style::Theme,
    }
    impl Env {
        fn new(damage: Arc<dyn DamageSink>) -> Self {
            Self {
                arena: Arena::new(),
                layout: LayoutTree::new(),
                text: TextSystem::new(FontDb::new()),
                damage,
                theme: kagari_style::Theme::default(),
            }
        }
        fn cx(&mut self) -> LayoutCx<'_> {
            LayoutCx {
                arena: &mut self.arena,
                layout: &mut self.layout,
                text: &mut self.text,
                damage: Arc::clone(&self.damage),
                theme: &self.theme,
                focus: None,
                cursor: None,
            }
        }
    }

    /// Builds a `[1, 2, 3]` list and returns it with the build environment + the list node id.
    fn build_list() -> (
        DynList<u32, u32>,
        Env,
        NodeId,
        reactive_graph::signal::WriteSignal<Vec<u32>>,
    ) {
        let (items, set_items) = signal(vec![1u32, 2, 3]);
        let mut env = Env::new(Arc::new(NoopDamage));
        let mut list = dyn_list(move || items.get(), |k: &u32| *k, |_k: &u32| div());
        let id = list.request_layout(&mut env.cx());
        (list, env, id, set_items)
    }

    #[test]
    fn keyed_list_should_rebuild_only_changed_children() {
        let owner = Owner::new();
        owner.set();

        let (mut list, mut env, id, set_items) = build_list();
        let v1 = env.arena.get(id).unwrap().children.clone();
        assert_eq!(v1.len(), 3);

        // [1, 2, 3] -> [1, 3, 4]: remove key 2, add key 4, keep 1 and 3.
        set_items.set(vec![1, 3, 4]);
        list.reconcile(&mut env.cx());
        let v2 = env.arena.get(id).unwrap().children.clone();

        assert_eq!(v2.len(), 3);
        assert!(!env.arena.contains(v1[1]), "key 2's node is removed");
        assert!(!v1.contains(&v2[2]), "key 4 is a freshly minted node");

        drop(owner);
    }

    #[test]
    fn keyed_list_should_preserve_unchanged_children() {
        let owner = Owner::new();
        owner.set();

        let (mut list, mut env, id, set_items) = build_list();
        let v1 = env.arena.get(id).unwrap().children.clone();

        set_items.set(vec![1, 3, 4]);
        list.reconcile(&mut env.cx());
        let v2 = env.arena.get(id).unwrap().children.clone();

        assert_eq!(v2[0], v1[0], "key 1 keeps its node");
        assert_eq!(v2[1], v1[2], "key 3 keeps its node (moved index 2 -> 1)");

        drop(owner);
    }

    #[test]
    fn keyed_list_remove_should_dispose_child_effect() {
        // Removing a list child must dispose the child's own reactive effect (RK-005): after
        // removal, writing the signal it subscribed to must not re-run it / flag damage.
        let owner = Owner::new();
        owner.set();

        let red = Background::Solid(Color::new(1.0, 0.0, 0.0, 1.0));
        let blue = Background::Solid(Color::new(0.0, 0.0, 1.0, 1.0));
        let (bg, set_bg) = signal(red);
        let (items, set_items) = signal(vec![1u32]);

        let damage = Arc::new(RecordingDamage::default());
        let mut env = Env::new(damage.clone());

        let mut list = dyn_list(
            move || items.get(),
            |k: &u32| *k,
            move |_k: &u32| div().background(rx(move || bg.get())),
        );
        list.request_layout(&mut env.cx());

        let flags_after_build = damage.count();
        assert!(
            flags_after_build >= 1,
            "the child's bg effect must fire on build"
        );

        // While mounted, a signal write re-runs the child effect (proves it is alive).
        set_bg.set(blue);
        let flags_while_alive = damage.count();
        assert!(
            flags_while_alive > flags_after_build,
            "the child effect reacts to writes while mounted"
        );

        // Remove the only item; its child owner is cleaned up, disposing the bg effect.
        set_items.set(vec![]);
        list.reconcile(&mut env.cx());

        // The now-disposed effect must not react to further writes.
        set_bg.set(red);
        assert_eq!(
            damage.count(),
            flags_while_alive,
            "a removed child's effect must be disposed (RK-005)"
        );

        drop(owner);
    }

    #[test]
    fn keyed_list_remove_should_free_multi_node_child_subtree() {
        // RK-006: removing a list child whose view is a multi-node subtree must free the whole
        // subtree (reconcile uses `arena.remove_subtree`), not just the child's root node.
        let owner = Owner::new();
        owner.set();

        let (items, set_items) = signal(vec![1u32]);
        let mut env = Env::new(Arc::new(NoopDamage));
        // Each item is a div with a (grand)child div — a two-node subtree.
        let mut list = dyn_list(
            move || items.get(),
            |k: &u32| *k,
            |_k: &u32| div().child(div()),
        );
        let list_id = list.request_layout(&mut env.cx());

        let child_root = env.arena.get(list_id).unwrap().children[0];
        let grandchild = env.arena.get(child_root).unwrap().children[0];
        assert!(env.arena.contains(grandchild), "grandchild built");

        // Remove the item; both the child root and its grandchild must be freed.
        set_items.set(vec![]);
        list.reconcile(&mut env.cx());
        assert!(!env.arena.contains(child_root), "child root freed");
        assert!(
            !env.arena.contains(grandchild),
            "grandchild subtree freed, not leaked (remove_subtree)"
        );

        drop(owner);
    }

    #[test]
    fn keyed_list_should_preserve_nodes_on_reorder() {
        let owner = Owner::new();
        owner.set();

        let (mut list, mut env, id, set_items) = build_list();
        let v1 = env.arena.get(id).unwrap().children.clone();

        // Pure reorder [1, 2, 3] -> [3, 2, 1]: every node is kept, only the order changes.
        set_items.set(vec![3, 2, 1]);
        list.reconcile(&mut env.cx());
        let v2 = env.arena.get(id).unwrap().children.clone();

        assert_eq!(
            v2,
            vec![v1[2], v1[1], v1[0]],
            "nodes are reordered, not rebuilt"
        );
        for nid in &v1 {
            assert!(env.arena.contains(*nid), "no node was dropped on reorder");
        }

        drop(owner);
    }

    #[test]
    fn dyn_if_should_mount_unmount_on_condition() {
        let owner = Owner::new();
        owner.set();

        let (cond, set_cond) = signal(true);
        let mut env = Env::new(Arc::new(NoopDamage));

        let mut node = dyn_if(move || cond.get(), div);
        let id = node.request_layout(&mut env.cx());

        // Condition is true: the child is mounted.
        let mounted = env.arena.get(id).unwrap().children.clone();
        assert_eq!(mounted.len(), 1);
        let child = mounted[0];
        assert!(env.arena.contains(child));

        // Flip to false: the child is unmounted and its node removed.
        set_cond.set(false);
        node.reconcile(&mut env.cx());
        assert_eq!(env.arena.get(id).unwrap().children.len(), 0);
        assert!(
            !env.arena.contains(child),
            "the child node is removed on unmount"
        );

        // Flip back to true: a fresh child is remounted (a new node, not the stale one).
        set_cond.set(true);
        node.reconcile(&mut env.cx());
        let remounted = env.arena.get(id).unwrap().children.clone();
        assert_eq!(remounted.len(), 1, "the child is remounted");
        assert_ne!(remounted[0], child, "remount mints a fresh node");
        assert!(env.arena.contains(remounted[0]));

        drop(owner);
    }

    #[test]
    fn dyn_if_should_mount_live_via_render_tree() {
        // #278: a dyn_if bound to a signal mounts/unmounts its child *live* off render_tree — no
        // explicit reconcile()/apply_staged() call. Signals are written between frames (RK-009).
        let owner = Owner::new();
        owner.set();
        let (cond, set_cond) = signal(false);
        let mut root = dyn_if(move || cond.get(), div).into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());

        let root_id = frame(&mut root, &mut arena, &mut layout, &mut text, &damage, None);
        assert_eq!(
            arena.get(root_id).unwrap().children.len(),
            0,
            "cond=false: no child"
        );

        set_cond.set(true);
        assert!(
            damage.is_dirty(),
            "a staged structural change wakes the loop"
        );
        frame(&mut root, &mut arena, &mut layout, &mut text, &damage, None);
        assert_eq!(
            arena.get(root_id).unwrap().children.len(),
            1,
            "cond=true mounts the child live via the frame loop"
        );

        set_cond.set(false);
        frame(&mut root, &mut arena, &mut layout, &mut text, &damage, None);
        assert_eq!(
            arena.get(root_id).unwrap().children.len(),
            0,
            "cond=false unmounts the child live"
        );
        drop(owner);
    }

    #[test]
    fn dyn_list_should_update_live_via_render_tree() {
        // #278: a dyn_list adds/removes rows live off render_tree.
        let owner = Owner::new();
        owner.set();
        let (items, set_items) = signal(vec![1u32, 2]);
        let mut root = dyn_list(move || items.get(), |k: &u32| *k, |_k: &u32| div()).into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());

        let root_id = frame(&mut root, &mut arena, &mut layout, &mut text, &damage, None);
        assert_eq!(arena.get(root_id).unwrap().children.len(), 2);

        set_items.set(vec![1, 2, 3, 4]);
        frame(&mut root, &mut arena, &mut layout, &mut text, &damage, None);
        assert_eq!(
            arena.get(root_id).unwrap().children.len(),
            4,
            "rows added live"
        );

        set_items.set(vec![1]);
        frame(&mut root, &mut arena, &mut layout, &mut text, &damage, None);
        assert_eq!(
            arena.get(root_id).unwrap().children.len(),
            1,
            "rows removed live"
        );
        drop(owner);
    }

    #[test]
    fn live_toggle_should_settle_to_idle() {
        // The gate: once a structural change is applied and the frame clears damage, a frame with no
        // signal write stages nothing — the loop returns to idle (no perpetual reconcile churn), so the
        // gated pass does not walk every frame.
        let owner = Owner::new();
        owner.set();
        let (cond, set_cond) = signal(false);
        let mut root = dyn_if(move || cond.get(), div).into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());

        frame(&mut root, &mut arena, &mut layout, &mut text, &damage, None);
        set_cond.set(true);
        frame(&mut root, &mut arena, &mut layout, &mut text, &damage, None);
        assert!(
            !damage.is_dirty(),
            "after applying the toggle the loop is idle (no perpetual reconcile)"
        );
        assert!(
            damage.take_structure_dirty().is_empty(),
            "no structure change is pending when idle"
        );
        drop(owner);
    }

    #[test]
    fn unmount_should_free_layout_node() {
        // #278: a live unmount frees the child's LayoutTree node (layout.remove_subtree), not only the
        // arena node — otherwise taffy nodes leak on every dyn churn.
        let owner = Owner::new();
        owner.set();
        let (cond, set_cond) = signal(true);
        let mut root =
            dyn_if(move || cond.get(), || div().size(Size { w: 10.0, h: 10.0 })).into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());

        let root_id = frame(&mut root, &mut arena, &mut layout, &mut text, &damage, None);
        let child = arena.get(root_id).unwrap().children[0];
        assert_ne!(
            layout.layout(child),
            Rect::default(),
            "the mounted child is laid out"
        );

        set_cond.set(false);
        frame(&mut root, &mut arena, &mut layout, &mut text, &damage, None);
        assert!(
            !arena.contains(child),
            "the unmounted child's arena node is freed"
        );
        assert_eq!(
            layout.layout(child),
            Rect::default(),
            "the layout node is freed too (remove_subtree), not leaked"
        );
        drop(owner);
    }

    #[test]
    fn unmount_should_deregister_focus() {
        // #278: a live unmount deregisters the removed subtree's focus, so a focused-then-removed node
        // does not black-hole keyboard dispatch (focused_node → a dead NodeId).
        let owner = Owner::new();
        owner.set();
        let mut reg = FocusRegistry::new();
        let handle = reg.handle();
        let hid = handle.id();
        let (cond, set_cond) = signal(true);
        let mut root =
            dyn_if(move || cond.get(), move || div().track_focus(&handle)).into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());

        frame(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            &damage,
            Some(&mut reg),
        );
        reg.current().set(Some(hid));
        assert!(
            reg.focused_node().is_some(),
            "the mounted focusable resolves to its node"
        );

        set_cond.set(false);
        frame(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            &damage,
            Some(&mut reg),
        );
        assert_eq!(
            reg.focused_node(),
            None,
            "the unmounted focusable is deregistered (no dangling FocusId → dead node)"
        );
        drop(owner);
    }

    #[test]
    fn containers_should_recurse_reconcile_into_nested_dyn() {
        // #278 guard: every container must recurse reconcile into its children, or a nested dyn_if
        // beneath it won't mount live. Wrapping each container around a dyn_if and toggling it must add
        // exactly one arena node (the mounted child) — a uniform check that also covers `overlay`, whose
        // child paints deferred (a scene-quad count would miss it). `VirtualizedList` is covered
        // separately by `virtualized_should_reconcile_nested_dyn_live` (its rows are index-built, not
        // child-wrapped, so it doesn't fit the `wrap(AnyElement)` shape).
        fn assert_recurses(wrap: impl Fn(AnyElement) -> AnyElement) {
            let owner = Owner::new();
            owner.set();
            let (cond, set_cond) = signal(false);
            let inner =
                dyn_if(move || cond.get(), || div().size(Size { w: 10.0, h: 10.0 })).into_element();
            let mut root = wrap(inner);
            let mut arena = Arena::new();
            let mut layout = LayoutTree::new();
            let mut text = TextSystem::new(FontDb::new());
            let damage = Arc::new(DamageState::default());

            let root_id = frame(&mut root, &mut arena, &mut layout, &mut text, &damage, None);
            let before = count_subtree(&arena, root_id);
            set_cond.set(true);
            frame(&mut root, &mut arena, &mut layout, &mut text, &damage, None);
            let after = count_subtree(&arena, root_id);
            assert_eq!(
                after,
                before + 1,
                "the container recursed reconcile → the nested dyn_if mounted exactly one node live"
            );
            drop(owner);
        }

        assert_recurses(|c| div().child(c).into_element());
        assert_recurses(|c| crate::element::scroll().child(c).into_element());
        assert_recurses(|c| crate::element::transform(c).into_element());
        assert_recurses(|c| crate::element::overlay(c).into_element());
    }

    #[test]
    fn virtualized_should_reconcile_nested_dyn_live() {
        // #278/#86 guard for the flagship container the arch review flagged: a `dyn_if` inside a
        // realized virtualized row mounts live off render_tree (VirtualizedList::reconcile recurses its
        // realized rows). One row is realized (viewport holds it); toggling the row's dyn_if adds a node.
        use crate::element::{ScrollHandle, virtualized_list};
        let owner = Owner::new();
        owner.set();
        let (cond, set_cond) = signal(false);
        let handle = ScrollHandle::new();
        let mut root =
            virtualized_list(40.0, 1, Size { w: 100.0, h: 100.0 }, &handle, move |_idx| {
                div().child(dyn_if(
                    move || cond.get(),
                    || div().size(Size { w: 5.0, h: 5.0 }),
                ))
            })
            .into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());

        let root_id = frame(&mut root, &mut arena, &mut layout, &mut text, &damage, None);
        let before = count_subtree(&arena, root_id);
        set_cond.set(true);
        frame(&mut root, &mut arena, &mut layout, &mut text, &damage, None);
        assert_eq!(
            count_subtree(&arena, root_id),
            before + 1,
            "VirtualizedList recursed reconcile into its realized row → the row's nested dyn_if mounted live"
        );
        drop(owner);
    }

    #[test]
    fn unmount_should_deregister_cursor() {
        // #278: a live unmount deregisters the removed subtree's cursor (symmetric with focus), so a
        // stale (removed) NodeId can't resolve a cursor and the map doesn't leak. Observed via `resolve`.
        let owner = Owner::new();
        owner.set();
        let mut reg = CursorRegistry::new();
        let (cond, set_cond) = signal(true);
        let mut root = dyn_if(
            move || cond.get(),
            || {
                div()
                    .cursor(CursorIcon::Pointer)
                    .size(Size { w: 10.0, h: 10.0 })
            },
        )
        .into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = Arc::new(DamageState::default());

        let root_id = frame_full(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            &damage,
            None,
            Some(&mut reg),
        );
        let child = arena.get(root_id).unwrap().children[0];
        let bounds = layout.layout(child);
        // A hit region over the mounted child, so `resolve` can find its declared cursor.
        let mut hit = HitTest::new();
        hit.push(HitRegion {
            node: child,
            bounds,
            clip: bounds,
            order: 0,
            flags: InteractFlags {
                cursor: true,
                ..InteractFlags::default()
            },
        });
        let pos = Point::new(bounds.origin.x + 1.0, bounds.origin.y + 1.0);
        assert_eq!(
            reg.resolve(&hit, &arena, pos),
            CursorIcon::Pointer,
            "the mounted child's declared cursor resolves"
        );

        set_cond.set(false);
        frame_full(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            &damage,
            None,
            Some(&mut reg),
        );
        assert_eq!(
            reg.resolve(&hit, &arena, pos),
            CursorIcon::Default,
            "after unmount the child's cursor is deregistered (resolves to default, not a stale entry)"
        );
        drop(owner);
    }
}
