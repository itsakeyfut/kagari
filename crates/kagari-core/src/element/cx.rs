//! Build/paint/event contexts and the damage seam for the element tree (#31).

use std::sync::Arc;

use kagari_base::NodeId;
use kagari_render::Scene;

use crate::arena::Arena;

/// Sink for damage raised when a reactive prop re-resolves. #31 wires the hook (a reactive
/// paint prop's effect calls [`DamageSink::mark_paint_dirty`]); #35 implements the real
/// layout/paint-dirty tracking and damage rects behind it.
pub trait DamageSink: Send + Sync {
    /// Mark `id`'s painted output as stale (a reactive paint prop changed).
    fn mark_paint_dirty(&self, id: NodeId);
}

/// Context for [`Element::request_layout`](super::Element::request_layout): the arena to build
/// into, and the damage sink that reactive-prop effects report to. The LayoutTree (#33) is added
/// here later.
pub struct LayoutCx<'a> {
    pub arena: &'a mut Arena,
    pub damage: Arc<dyn DamageSink>,
}

/// Context for [`Element::paint`](super::Element::paint): the scene to emit primitives into.
pub struct PaintCx<'a> {
    pub scene: &'a mut Scene,
}

/// An input event. Minimal placeholder for Phase 3; variants are added with the event system.
#[non_exhaustive]
pub enum Event {}

/// Context for [`Element::handle_event`](super::Element::handle_event).
pub struct EventCx<'a> {
    pub arena: &'a mut Arena,
}
