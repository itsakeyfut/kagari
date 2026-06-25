//! Opaque identifiers for nodes and windows.

/// Opaque element-node identifier.
///
/// The `u64` leaves room for a generational index, but the exact index/generation
/// packing is owned by the retained arena in `kagari-core` (Phase 3); here it is
/// just an opaque handle. Use [`NodeId::from_raw`]/[`NodeId::raw`] to mint and read.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(u64);

impl NodeId {
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub fn raw(self) -> u64 {
        self.0
    }
}

/// Opaque window identifier.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WindowId(u32);

impl WindowId {
    pub fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub fn raw(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn node_id_from_raw_should_round_trip() {
        assert_eq!(NodeId::from_raw(42).raw(), 42);
    }

    #[test]
    fn window_id_from_raw_should_round_trip() {
        assert_eq!(WindowId::from_raw(3).raw(), 3);
    }

    #[test]
    fn node_id_should_work_as_hash_key() {
        let mut set = HashSet::new();
        set.insert(NodeId::from_raw(1));
        set.insert(NodeId::from_raw(1));
        assert_eq!(set.len(), 1);
        assert!(set.contains(&NodeId::from_raw(1)));
        assert!(!set.contains(&NodeId::from_raw(2)));
    }
}
