//! Build/paint/event contexts and the damage seam for the element tree (#31, extended in #34).

use std::sync::Arc;

use kagari_base::NodeId;
use kagari_layout::LayoutTree;
use kagari_render::{Atlas, Scene};
use kagari_text::TextSystem;

use crate::arena::Arena;

/// Sink for damage raised when a reactive prop re-resolves. #31 wires the hook (a reactive
/// paint prop's effect calls [`DamageSink::mark_paint_dirty`]); #35 implements the real
/// layout/paint-dirty tracking and damage rects behind it.
pub trait DamageSink: Send + Sync {
    /// Mark `id`'s painted output as stale (a reactive paint prop changed).
    fn mark_paint_dirty(&self, id: NodeId);
}

/// Context for [`Element::request_layout`](super::Element::request_layout): the arena to build
/// into, the layout tree to register taffy nodes/styles/measures in, the text system for shaping
/// text leaves, and the damage sink reactive-prop effects report to.
pub struct LayoutCx<'a> {
    pub arena: &'a mut Arena,
    pub layout: &'a mut LayoutTree,
    pub text: &'a mut TextSystem,
    pub damage: Arc<dyn DamageSink>,
}

/// Context for [`Element::paint`](super::Element::paint): the scene to emit primitives into, the
/// computed layout (for child bounds), the text system + glyph atlas for rasterizing text.
///
/// `atlas` is optional: with `None`, text leaves skip glyph rasterization (used by GPU-free
/// Scene-structure tests); the app always supplies `Some(renderer.atlas_mut())`.
pub struct PaintCx<'a> {
    pub scene: &'a mut Scene,
    pub layout: &'a LayoutTree,
    pub text: &'a mut TextSystem,
    pub atlas: Option<&'a mut Atlas>,
    /// Monotonic painter's-order counter: each emitted primitive group takes the next value, so
    /// primitives draw back-to-front in tree-traversal order (parent before its children).
    next_order: u32,
}

impl<'a> PaintCx<'a> {
    /// Creates a paint context with the painter's-order counter at zero.
    pub fn new(
        scene: &'a mut Scene,
        layout: &'a LayoutTree,
        text: &'a mut TextSystem,
        atlas: Option<&'a mut Atlas>,
    ) -> Self {
        Self {
            scene,
            layout,
            text,
            atlas,
            next_order: 0,
        }
    }

    /// Returns the next painter's-order value (incrementing the counter).
    pub fn next_order(&mut self) -> u32 {
        let order = self.next_order;
        self.next_order += 1;
        order
    }
}

/// An input event. Minimal placeholder for Phase 3; variants are added with the event system.
#[non_exhaustive]
pub enum Event {}

/// Context for [`Element::handle_event`](super::Element::handle_event).
pub struct EventCx<'a> {
    pub arena: &'a mut Arena,
}
