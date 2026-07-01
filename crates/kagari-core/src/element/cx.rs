//! Build/paint/event contexts and the damage seam for the element tree (#31, extended in #34).

use std::sync::Arc;

use kagari_base::{NodeId, Rect, Size};
use kagari_layout::LayoutTree;
use kagari_render::{Atlas, Scene};
use kagari_style::Theme;
use kagari_text::TextSystem;

use crate::arena::Arena;
use crate::event::{
    Action, CaptureOp, CursorRegistry, Delivery, FocusRegistry, GestureEvent, HitRegion, HitTest,
    KeyEvent, MouseEvent,
};
use crate::overlay::OverlayRegistry;

/// The painter-order base for overlay content (#62): above any realistic main-content primitive
/// count, so an overlay always composites frontmost. The space above it is partitioned into per-slot
/// bands so later-registered overlays draw over earlier ones.
const OVERLAY_ORDER_BASE: u32 = 1 << 28;
/// The painter-order span reserved per overlay slot (the primitive budget per overlay before bands
/// would touch). Saturating arithmetic keeps extreme slot counts from panicking.
const OVERLAY_SLOT_STRIDE: u32 = 1 << 16;

/// One overlay's active painter-order namespace: `base` from its z-slot + a running `counter`.
struct OverlayOrderState {
    base: u32,
    counter: u32,
}

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
    /// The focus registry to record `track_focus` associations into (#49), optional like the paint
    /// pass's hit-test sink: `None` skips registration (the app path until live keyboard wiring
    /// lands; the focus unit tests pass `Some`). A `Div` with a tracked focus id registers its
    /// `FocusId → NodeId` here at build so dispatch/focus-within can resolve the focused node.
    pub focus: Option<&'a mut FocusRegistry>,
    /// The cursor registry to record `cursor` declarations into (#53), optional like `focus`: `None`
    /// skips registration (the app path until live cursor wiring lands; the cursor unit tests pass
    /// `Some`). A `Div` with a declared cursor registers its `NodeId → CursorIcon` here at build so
    /// pointer-move cursor resolution can find it.
    pub cursor: Option<&'a mut CursorRegistry>,
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
    /// The active clip rect (absolute), or `None` when unclipped. A [`Scroll`](super::Scroll) pushes
    /// its viewport bounds here so descendants' primitives and hit regions are masked to the visible
    /// area (nested scrolls intersect). `None` is the default — Div/Text then emit unclipped, so
    /// non-scrolled output is unchanged.
    clip: Option<Rect>,
    /// The frame's overlay registry (#62), if attached: an [`Overlay`](super::Overlay) registers into
    /// it during paint. `None` skips registration (GPU-free tests without overlays).
    overlay: Option<&'a mut OverlayRegistry>,
    /// The active overlay painter-order namespaces (a stack for nested overlays). Empty = main
    /// content; the top entry's high-order band is used by [`next_order`](Self::next_order) so an
    /// overlay's subtree composites frontmost.
    overlay_orders: Vec<OverlayOrderState>,
    /// The window viewport size (logical px), for anchored overlay auto-flip / on-screen clamping
    /// (#175). Set by `render_tree`; default zero (unused unless an overlay is anchored).
    viewport: Size,
}

impl<'a> PaintCx<'a> {
    /// Creates a paint context with the painter's-order counter at zero and no clip.
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
            clip: None,
            overlay: None,
            overlay_orders: Vec::new(),
            viewport: Size { w: 0.0, h: 0.0 },
        }
    }

    /// Sets the window viewport size for anchored-overlay placement (#175); `render_tree` calls this.
    pub(crate) fn set_viewport(&mut self, viewport: Size) {
        self.viewport = viewport;
    }

    /// The window viewport size (for anchored overlay auto-flip / clamping).
    pub(crate) fn viewport(&self) -> Size {
        self.viewport
    }

    /// Attaches the frame's overlay registry (`render_tree` sets this): [`Overlay`](super::Overlay)
    /// nodes register into it during the paint walk.
    pub(crate) fn attach_overlay(&mut self, registry: &'a mut OverlayRegistry) {
        self.overlay = Some(registry);
    }

    /// Registers an overlay node at `bounds` into the attached registry, returning its z-slot
    /// (registration order), or `None` when no registry is attached (a lone overlay then falls back
    /// to slot 0 — still frontmost, just no cross-overlay ordering).
    pub(crate) fn overlay_register(&mut self, node: NodeId, bounds: Rect) -> Option<usize> {
        self.overlay
            .as_deref_mut()
            .map(|reg| reg.register(node, bounds))
    }

    /// Enters an overlay's high painter-order band for `slot` (registration order): subsequent
    /// [`next_order`](Self::next_order) values sort above all main content and above lower-slot
    /// overlays. Nesting pushes a stack; pair with [`exit_overlay`](Self::exit_overlay).
    pub(crate) fn enter_overlay(&mut self, slot: usize) {
        let base =
            OVERLAY_ORDER_BASE.saturating_add((slot as u32).saturating_mul(OVERLAY_SLOT_STRIDE));
        self.overlay_orders
            .push(OverlayOrderState { base, counter: 0 });
    }

    /// Leaves the current overlay order band, restoring the enclosing one (or the main counter).
    pub(crate) fn exit_overlay(&mut self) {
        self.overlay_orders.pop();
    }

    /// The active clip rect, or `None` when unclipped. Text glyphs read this to mask themselves to a
    /// scroll viewport.
    pub(crate) fn clip(&self) -> Option<Rect> {
        self.clip
    }

    /// Intersects `rect` with the active clip — the identity when unclipped. An empty result means
    /// `rect` is fully clipped away (the caller should then skip emitting the primitive).
    pub(crate) fn clip_rect(&self, rect: Rect) -> Rect {
        match self.clip {
            None => rect,
            Some(c) => rect.intersect(c).unwrap_or_default(),
        }
    }

    /// Pushes an absolute clip, intersecting it with any active clip, and returns the previous clip
    /// so a later [`set_clip`](Self::set_clip) restores it (a scroll wraps its child paint in this).
    pub(crate) fn push_clip(&mut self, rect: Rect) -> Option<Rect> {
        let prev = self.clip;
        self.clip = Some(match prev {
            None => rect,
            Some(c) => c.intersect(rect).unwrap_or_default(),
        });
        prev
    }

    /// Restores a clip saved by [`push_clip`](Self::push_clip).
    pub(crate) fn set_clip(&mut self, clip: Option<Rect>) {
        self.clip = clip;
    }

    /// Returns the next painter's-order value. Inside an overlay (`enter_overlay`) it draws from that
    /// overlay's high-order band so the overlay composites frontmost; otherwise from the main
    /// monotonic counter.
    pub fn next_order(&mut self) -> u32 {
        match self.overlay_orders.last_mut() {
            Some(oo) => {
                // Each overlay owns a STRIDE-wide band; exceeding it would bleed into the next slot's
                // band (z-collision, no panic — saturating). Realistic overlays (menus/tooltips) are
                // far under the budget, so this only guards a pathological subtree in debug builds.
                debug_assert!(
                    oo.counter < OVERLAY_SLOT_STRIDE,
                    "overlay subtree exceeded its per-slot painter-order budget; z bands may collide"
                );
                let order = oo.base.saturating_add(oo.counter);
                oo.counter = oo.counter.saturating_add(1);
                order
            }
            None => {
                let order = self.next_order;
                self.next_order = self.next_order.saturating_add(1);
                order
            }
        }
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
    Keyboard(KeyEvent),
    /// A resolved [`Action`] (#50), dispatched up the focus chain to `on_action` handlers.
    Action(Action),
    /// A recognized [`GestureEvent`] (#51), dispatched up the hit path to `on_gesture` handlers.
    Gesture(GestureEvent),
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
