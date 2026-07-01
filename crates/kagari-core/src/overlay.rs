//! Overlay registry (#62): the per-frame record of deferred-draw overlay entries.
//!
//! An overlay node registers here during the paint walk (getting a z-slot in registration order)
//! instead of being clipped/z-ordered inline. The paint context uses the slot to draw the overlay's
//! subtree in a high painter-order namespace (frontmost, escaping parent clip). The recorded entries
//! are the extension point for anchored placement (#175) and the drag-image overlay (#172).

use kagari_base::{NodeId, Rect};

/// One registered overlay: the deferred-drawn node and its absolute bounds (the anchor rect a
/// placement follow-up, #175, will position against).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct OverlayEntry {
    pub node: NodeId,
    pub bounds: Rect,
}

/// A frame's registered overlays, in registration (z) order — later registrations draw on top.
/// Rebuilt each painted frame with capacity retained (mirrors [`HitTest`](crate::HitTest)).
#[derive(Default, Debug)]
pub struct OverlayRegistry {
    entries: Vec<OverlayEntry>,
}

impl OverlayRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Clears the registered overlays, retaining capacity for the next frame (perf.md).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Registers `node`'s overlay at `bounds`, returning its z-slot (registration order): a larger
    /// slot draws on top. Called during paint by an [`Overlay`](crate::Overlay) node.
    pub(crate) fn register(&mut self, node: NodeId, bounds: Rect) -> usize {
        let slot = self.entries.len();
        self.entries.push(OverlayEntry { node, bounds });
        slot
    }

    /// The registered overlays, in registration (z) order.
    pub fn entries(&self) -> &[OverlayEntry] {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(raw: u64) -> NodeId {
        NodeId::from_raw(raw)
    }

    #[test]
    fn overlay_registry_should_record_entries_in_order() {
        let mut reg = OverlayRegistry::new();
        let s0 = reg.register(node(1), Rect::from_xywh(0.0, 0.0, 10.0, 10.0));
        let s1 = reg.register(node(2), Rect::from_xywh(5.0, 5.0, 10.0, 10.0));
        assert_eq!((s0, s1), (0, 1), "slots increment in registration order");
        assert_eq!(reg.entries().len(), 2);
        assert_eq!(reg.entries()[0].node, node(1));
        assert_eq!(
            reg.entries()[1].bounds,
            Rect::from_xywh(5.0, 5.0, 10.0, 10.0)
        );
    }

    #[test]
    fn overlay_registry_should_clear_retaining_capacity() {
        let mut reg = OverlayRegistry::new();
        reg.register(node(1), Rect::from_xywh(0.0, 0.0, 1.0, 1.0));
        let cap = reg.entries.capacity();
        reg.clear();
        assert!(reg.entries().is_empty());
        assert!(reg.entries.capacity() >= cap, "capacity retained for reuse");
    }
}
