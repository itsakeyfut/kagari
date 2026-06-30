//! Build/paint/event contexts and the damage seam for the element tree (#31, extended in #34).

use std::sync::Arc;

use kagari_base::NodeId;
use kagari_layout::LayoutTree;
use kagari_render::{Atlas, Scene};
use kagari_style::Theme;
use kagari_text::TextSystem;

use crate::arena::Arena;
use crate::event::{CaptureOp, Delivery, HitRegion, HitTest, MouseEvent};

/// Sink for damage raised when a reactive prop re-resolves. #31 wires the hook (a reactive
/// paint prop's effect calls [`DamageSink::mark_paint_dirty`]); #35 implements the real
/// layout/paint-dirty tracking and damage rects behind it ([`DamageState`](crate::damage::DamageState)).
pub trait DamageSink: Send + Sync {
    /// Mark `id`'s painted output as stale (a reactive paint prop changed: appearance only,
    /// no relayout).
    fn mark_paint_dirty(&self, id: NodeId);

    /// Mark `id` as needing relayout (a reactive layout prop changed: size/position). Default
    /// no-op so paint-only sinks need not implement it; the first reactive layout setter that
    /// calls this lands in #146.
    fn mark_layout_dirty(&self, _id: NodeId) {}
}

/// Context for [`Element::request_layout`](super::Element::request_layout): the arena to build
/// into, the layout tree to register taffy nodes/styles/measures in, the text system for shaping
/// text leaves, and the damage sink reactive-prop effects report to.
pub struct LayoutCx<'a> {
    pub arena: &'a mut Arena,
    pub layout: &'a mut LayoutTree,
    pub text: &'a mut TextSystem,
    pub damage: Arc<dyn DamageSink>,
    /// The current theme, for resolving layout tokens (gap/padding) into concrete px (#42).
    pub theme: &'a Theme,
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
    /// The hit-test to record interactive regions into (#48), optional like `atlas`: `None` skips
    /// recording (used by GPU-free Scene-structure tests that don't exercise hit-testing). Built
    /// during the same paint walk as the scene, so a region's `order` matches its draw z.
    hit_test: Option<&'a mut HitTest>,
    /// The current theme, for resolving paint tokens (bg/rounded/border) into concrete values (#42).
    pub theme: &'a Theme,
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
        hit_test: Option<&'a mut HitTest>,
        theme: &'a Theme,
    ) -> Self {
        Self {
            scene,
            layout,
            text,
            atlas,
            hit_test,
            theme,
            next_order: 0,
        }
    }

    /// Returns the next painter's-order value (incrementing the counter).
    pub fn next_order(&mut self) -> u32 {
        let order = self.next_order;
        self.next_order += 1;
        order
    }

    /// Records an interactive [`HitRegion`] into the hit-test, if one is attached (#48). A no-op
    /// when `hit_test` is `None` (e.g. GPU-free Scene-only tests).
    pub fn record_hit(&mut self, region: HitRegion) {
        if let Some(ht) = self.hit_test.as_deref_mut() {
            ht.push(region);
        }
    }
}

/// An input event delivered to elements (#48). More variants (keyboard, gesture, drag) arrive with
/// their issues (#49/#51/#52).
#[non_exhaustive]
pub enum Event {
    Mouse(MouseEvent),
}

/// Context for [`Element::handle_event`](super::Element::handle_event) during a dispatch walk (#48).
///
/// Carries the dispatch delivery (the path to descend and whether to bubble or fire on a filtered
/// set), the read-only `arena` for handlers that query the tree, and the propagation/capture state a
/// handler can drive via [`stop_propagation`](Self::stop_propagation) /
/// [`capture_pointer`](Self::capture_pointer) / [`release_pointer`](Self::release_pointer).
pub struct EventCx<'a> {
    /// The retained tree, read-only: handlers may inspect node relationships (focus dispatch #49
    /// reads it). Dispatch never mutates the arena — handlers write to signals (design.md §3).
    pub arena: &'a Arena,
    delivery: Delivery<'a>,
    /// The node whose handlers are currently running, so `capture_pointer` grabs the right node.
    current: Option<NodeId>,
    stopped: bool,
    capture: Option<CaptureOp>,
}

impl<'a> EventCx<'a> {
    /// Builds a dispatch context for one delivery pass (the dispatch driver constructs this).
    pub(crate) fn new(arena: &'a Arena, delivery: Delivery<'a>) -> Self {
        Self {
            arena,
            delivery,
            current: None,
            stopped: false,
            capture: None,
        }
    }

    /// Halts the rest of the capture/bubble walk: ancestors' handlers do not run after this.
    pub fn stop_propagation(&mut self) {
        self.stopped = true;
    }

    /// Grabs the pointer to the node whose handler is running: subsequent mouse events route to it
    /// (a drag grab) until the matching mouse-up or [`release_pointer`](Self::release_pointer).
    pub fn capture_pointer(&mut self) {
        if let Some(current) = self.current {
            self.capture = Some(CaptureOp::Capture(current));
        }
    }

    /// Releases a pointer grab taken with [`capture_pointer`](Self::capture_pointer).
    pub fn release_pointer(&mut self) {
        self.capture = Some(CaptureOp::Release);
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped
    }

    pub(crate) fn capture_request(&self) -> Option<CaptureOp> {
        self.capture
    }

    pub(crate) fn set_current(&mut self, id: NodeId) {
        self.current = Some(id);
    }

    /// The child of `id` that lies on the dispatch path (the node to recurse into), if any.
    pub(crate) fn next_child_on_path(&self, id: NodeId) -> Option<NodeId> {
        self.delivery.next_after(id)
    }

    /// Whether `id`'s handlers should fire for this delivery (always on a bubble pass; only for
    /// nodes in the filtered set on an enter/leave pass).
    pub(crate) fn should_fire(&self, id: NodeId) -> bool {
        self.delivery.should_fire(id)
    }
}
