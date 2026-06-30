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

use std::sync::atomic::{AtomicBool, Ordering};
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

/// A keyed, reactive list. Builds a child per item; on a collection-signal change only the
/// changed children are rebuilt (keyed reconciliation), unchanged children keep their nodes.
pub struct DynList<Item, K> {
    items_fn: Arc<dyn Fn() -> Vec<Item> + Send + Sync>,
    key_fn: Box<dyn Fn(&Item) -> K>,
    view_fn: Box<dyn Fn(&Item) -> AnyElement>,
    children: Vec<KeyedChild<K>>,
    /// Items staged by the effect, consumed by [`DynList::reconcile`].
    pending: Arc<Mutex<Option<Vec<Item>>>>,
    /// Cheap structure-dirty flag for the scheduler (#36) to discover pending reconciles.
    dirty: Arc<AtomicBool>,
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
        dirty: Arc::new(AtomicBool::new(false)),
        owner: None,
        id: None,
    }
}

impl<Item, K> DynList<Item, K>
where
    Item: Send + 'static,
    K: PartialEq + 'static,
{
    /// Whether the collection changed since the last reconcile (the scheduler, #36, polls this).
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::SeqCst)
    }

    /// Applies the staged items: a keyed diff against the current children. New keys are built
    /// under a fresh child [`Owner`]; gone keys have their owner cleaned up (disposing their
    /// effects) and their node removed; existing keys keep their node, owner, and element.
    pub fn reconcile(&mut self, cx: &mut LayoutCx) {
        let items = match self.pending.lock() {
            Ok(mut pending) => pending.take(),
            Err(_) => return,
        };
        let Some(items) = items else {
            return;
        };
        self.dirty.store(false, Ordering::SeqCst);

        let owner = self.owner.clone().unwrap_or_default();
        let mut old = std::mem::take(&mut self.children);
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

        // Whatever remains in `old` was removed: dispose its reactive scope (effects), then free
        // its whole arena subtree — `remove` is single-node, so a multi-node child would leak its
        // descendants' slots otherwise.
        for (_, node_id, child_owner, _element) in old {
            child_owner.cleanup();
            cx.arena.remove_subtree(node_id);
        }

        let ids: Vec<NodeId> = next.iter().map(|(_, node_id, ..)| *node_id).collect();
        if let Some(parent_id) = self.id {
            if let Some(node) = cx.arena.get_mut(parent_id) {
                node.children = ids.clone();
            }
            cx.layout.set_children(parent_id, &ids);
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

        // Staging effect (registered under the current owner): subscribe to the collection,
        // stage the items, and flag structure-dirty. The heavy reconcile is separate (#36).
        let pending = Arc::clone(&self.pending);
        let dirty = Arc::clone(&self.dirty);
        let items_fn = Arc::clone(&self.items_fn);
        create_effect(move || {
            if let Ok(mut slot) = pending.lock() {
                *slot = Some(items_fn());
            }
            dirty.store(true, Ordering::SeqCst);
        });

        // Build the initial children from what the effect just staged.
        self.reconcile(cx);
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
    pending: Arc<Mutex<Option<bool>>>,
    dirty: Arc<AtomicBool>,
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
        dirty: Arc::new(AtomicBool::new(false)),
        owner: None,
        id: None,
    }
}

impl DynIf {
    /// Whether the condition changed since the last reconcile (polled by the scheduler, #36).
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::SeqCst)
    }

    /// Mounts the child when the staged condition is true and it is absent; unmounts (disposing
    /// its owner scope and removing its node) when false and present.
    pub fn reconcile(&mut self, cx: &mut LayoutCx) {
        let cond = match self.pending.lock() {
            Ok(mut pending) => pending.take(),
            Err(_) => return,
        };
        let Some(cond) = cond else {
            return;
        };
        self.dirty.store(false, Ordering::SeqCst);

        let owner = self.owner.clone().unwrap_or_default();
        match (cond, self.child.take()) {
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
            }
            (false, Some((node_id, child_owner, _element))) => {
                child_owner.cleanup();
                cx.arena.remove_subtree(node_id);
            }
            // Already mounted (true) or already absent (false): keep state.
            (true, existing @ Some(_)) => self.child = existing,
            (false, None) => {}
        }

        let ids: Vec<NodeId> = self.child.iter().map(|(node_id, ..)| *node_id).collect();
        if let Some(parent_id) = self.id {
            if let Some(node) = cx.arena.get_mut(parent_id) {
                node.children = ids.clone();
            }
            cx.layout.set_children(parent_id, &ids);
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
        let dirty = Arc::clone(&self.dirty);
        let cond_fn = Arc::clone(&self.cond_fn);
        create_effect(move || {
            if let Ok(mut slot) = pending.lock() {
                *slot = Some(cond_fn());
            }
            dirty.store(true, Ordering::SeqCst);
        });

        self.reconcile(cx);
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
    use crate::element::{DamageSink, div};
    use crate::reactive::rx;
    use kagari_base::Color;
    use kagari_layout::LayoutTree;
    use kagari_render::Background;
    use kagari_text::{FontDb, TextSystem};
    use reactive_graph::owner::Owner;
    use reactive_graph::prelude::*;
    use reactive_graph::signal::signal;

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
}
