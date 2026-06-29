//! Retained node arena (#30).
//!
//! A `slotmap`-backed store of the element tree's nodes, keyed by [`NodeId`]. The tree is
//! built once and persists across frames (NOT a per-frame bump arena — heavy per-frame drawing
//! goes through the immediate canvas, not this arena). slotmap gives O(1), cache-dense storage
//! and **generation-checked keys**: a stale [`NodeId`] (e.g. a removed timeline clip or
//! scene-hierarchy row) safely misses a reused slot instead of aliasing it (no use-after-free /
//! ABA), which matters for editors with heavy node add/remove.
//!
//! [`NodeId`] is a plain `u64` newtype in kagari-base; it transports slotmap's `KeyData`
//! (index + generation) through [`KeyData::as_ffi`]/[`KeyData::from_ffi`]. The packing knowledge
//! lives here so kagari-base stays free of a slotmap dependency.

use kagari_base::NodeId;
use slotmap::{DefaultKey, Key, KeyData, SlotMap};

/// Bridges a [`NodeId`] to the slotmap key it carries.
fn to_key(id: NodeId) -> DefaultKey {
    KeyData::from_ffi(id.raw()).into()
}

/// Bridges a slotmap key back to the [`NodeId`] that transports it. The round-trip is exact for
/// occupied keys: their version is odd, so `from_ffi`'s `| 1` is a no-op.
fn to_id(key: DefaultKey) -> NodeId {
    NodeId::from_raw(key.data().as_ffi())
}

/// A retained element-tree node.
///
/// Minimal for now (#30): the parent/children tree skeleton. The element, layout, and paint
/// payload are added by #31/#33/#34.
#[derive(Debug, Default)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

/// The retained node arena. Build-once, persists across frames; hands out generation-checked
/// [`NodeId`] handles so effects (#31+) can update nodes across frames without aliasing.
#[derive(Debug)]
pub struct Arena {
    nodes: SlotMap<DefaultKey, Node>,
}

impl Arena {
    /// Creates an empty arena.
    pub fn new() -> Self {
        Self {
            nodes: SlotMap::new(),
        }
    }

    /// Inserts a node, returning its stable handle.
    pub fn insert(&mut self, node: Node) -> NodeId {
        to_id(self.nodes.insert(node))
    }

    /// Returns the node for `id`, or `None` if it was removed (its slot may have been reused by
    /// a different node — the generation check rejects the stale handle).
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(to_key(id))
    }

    /// Mutable counterpart of [`Arena::get`].
    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.get_mut(to_key(id))
    }

    /// Removes and returns the node for `id`, or `None` if the handle is stale.
    ///
    /// Removes only this node — not its descendants. To free a whole subtree (e.g. when a
    /// dynamic child is unmounted) use [`Arena::remove_subtree`], or its children's slots leak.
    pub fn remove(&mut self, id: NodeId) -> Option<Node> {
        self.nodes.remove(to_key(id))
    }

    /// Removes `id` and all its descendants, returning the number of nodes removed.
    ///
    /// Reactive-owner cleanup frees a removed subtree's effects, but not its arena nodes; this
    /// frees the nodes so the two ownership planes stay in sync on removal (#32).
    pub fn remove_subtree(&mut self, id: NodeId) -> usize {
        let mut removed = 0;
        if let Some(node) = self.nodes.remove(to_key(id)) {
            removed += 1;
            for child in node.children {
                removed += self.remove_subtree(child);
            }
        }
        removed
    }

    /// Whether `id` still refers to a live node.
    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(to_key(id))
    }
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arena_insert_should_return_stable_id() {
        let mut arena = Arena::new();
        let id = arena.insert(Node::default());

        assert!(arena.contains(id));
        assert!(arena.get(id).is_some());
        // The handle is stable: repeated lookups resolve the same node.
        assert!(arena.get(id).is_some());
    }

    #[test]
    fn arena_get_after_remove_should_be_none() {
        let mut arena = Arena::new();
        let id = arena.insert(Node::default());

        assert!(arena.remove(id).is_some());
        assert!(arena.get(id).is_none());
        assert!(!arena.contains(id));
        // Removing an already-stale handle is a safe miss, not a panic.
        assert!(arena.remove(id).is_none());
    }

    #[test]
    fn arena_remove_subtree_should_free_descendants() {
        let mut arena = Arena::new();
        let root = arena.insert(Node::default());
        let child = arena.insert(Node {
            parent: Some(root),
            children: Vec::new(),
        });
        let grandchild = arena.insert(Node {
            parent: Some(child),
            children: Vec::new(),
        });
        arena.get_mut(child).unwrap().children.push(grandchild);
        arena.get_mut(root).unwrap().children.push(child);

        assert_eq!(
            arena.remove_subtree(root),
            3,
            "root + child + grandchild removed"
        );
        assert!(!arena.contains(root));
        assert!(!arena.contains(child));
        assert!(
            !arena.contains(grandchild),
            "descendants are freed, not leaked"
        );
    }

    #[test]
    fn arena_remove_should_not_alias_reused_slot() {
        let mut arena = Arena::new();

        // Distinct markers so we can tell the two nodes apart.
        let a = arena.insert(Node {
            parent: Some(NodeId::from_raw(0xAAAA)),
            children: Vec::new(),
        });
        arena.remove(a);

        // `b` likely reuses `a`'s slot index but with a bumped generation.
        let b = arena.insert(Node {
            parent: Some(NodeId::from_raw(0xBBBB)),
            children: Vec::new(),
        });

        assert_ne!(
            a, b,
            "the reused slot must mint a distinct generation-checked id"
        );
        assert!(
            arena.get(a).is_none(),
            "the stale handle must not alias the reused slot"
        );
        assert_eq!(
            arena.get(b).and_then(|n| n.parent),
            Some(NodeId::from_raw(0xBBBB)),
            "the live handle resolves its own node"
        );
    }

    #[test]
    fn arena_get_mut_should_mutate_node() {
        let mut arena = Arena::new();
        let parent = arena.insert(Node::default());
        let child = arena.insert(Node {
            parent: Some(parent),
            children: Vec::new(),
        });

        arena.get_mut(parent).unwrap().children.push(child);

        assert_eq!(arena.get(parent).unwrap().children, vec![child]);
    }
}
