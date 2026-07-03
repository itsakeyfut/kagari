//! Overlay registry (#62): the per-frame record of deferred-draw overlay entries.
//!
//! An overlay node registers here during the paint walk (getting a z-slot in registration order)
//! instead of being clipped/z-ordered inline. The paint context uses the slot to draw the overlay's
//! subtree in a high painter-order namespace (frontmost, escaping parent clip). The recorded entries
//! are the extension point for anchored placement (#175) and the drag-image overlay (#172).

use std::sync::{Arc, Mutex, MutexGuard};

use kagari_base::{NodeId, Point, Rect};

use crate::reactive::RwSignal;
use crate::reactive::prelude::*;

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

/// One open interactive overlay in the window's layer stack (#219): its node (the focus-trap scope
/// root), its content bounds (for outside-press hit tests), the interactive opt-ins, and the controls to
/// dismiss it. Built by an `Overlay` with `.open(..)` and pushed at paint.
#[derive(Clone)]
pub struct OverlayLayer {
    /// The overlay node — the focus-trap scope root (its focusable descendants form the trap).
    pub node: NodeId,
    /// The overlay content's absolute bounds; a press outside every open layer's bounds is "outside".
    pub bounds: Rect,
    /// Dismiss on an outside press / Esc.
    pub dismiss_on_outside: bool,
    /// Confine Tab/Shift+Tab within this overlay while it is topmost.
    pub trap_focus: bool,
    /// Paint a modal backdrop and block input to content beneath.
    pub backdrop: bool,
    /// The controlled open signal — set false to dismiss.
    open: RwSignal<bool>,
    /// Optional dismissal callback, run after clearing `open`.
    on_dismiss: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl OverlayLayer {
    /// Assembles a layer entry (called by `Overlay`'s paint).
    pub(crate) fn new(
        node: NodeId,
        bounds: Rect,
        dismiss_on_outside: bool,
        trap_focus: bool,
        backdrop: bool,
        open: RwSignal<bool>,
        on_dismiss: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        Self {
            node,
            bounds,
            dismiss_on_outside,
            trap_focus,
            backdrop,
            open,
            on_dismiss,
        }
    }

    /// Whether this overlay is still open. A registered layer's `open` is normally `true`, but a dismiss
    /// clears it before the next paint rebuilds the stack — so a layer can be present yet already closed.
    fn is_open(&self) -> bool {
        self.open.get_untracked()
    }

    /// Dismisses this overlay: clears its `open` signal, then runs its `on_dismiss` callback.
    fn dismiss(&self) {
        self.open.set(false);
        if let Some(cb) = &self.on_dismiss {
            cb();
        }
    }
}

/// The window's interactive overlay layer stack (#219): the ordered set of open overlays, rebuilt each
/// painted frame (registration order = paint order = z order, so **topmost = last**). Provided into the
/// window context so an open `Overlay` registers here at paint; the shell reads it
/// during input for outside/Esc dismiss routing and focus-trap.
///
/// The state is behind `Arc<Mutex<..>>` (not `Rc<RefCell<..>>`) so the registry is `Send + Sync` for
/// `provide_context`; the UI/reactive runtime is thread-local, so the mutex is uncontended (mirrors
/// [`FocusRegistry`](crate::FocusRegistry), #218).
#[derive(Clone, Default)]
pub struct OverlayLayers {
    inner: Arc<Mutex<Vec<OverlayLayer>>>,
}

impl OverlayLayers {
    /// Creates an empty layer stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the shared stack, recovering a poisoned mutex rather than panicking (the state is a plain
    /// `Vec`, so a panic while holding the lock leaves it usable — mirrors #218's `FocusRegistry`).
    fn lock(&self) -> MutexGuard<'_, Vec<OverlayLayer>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Registers an open overlay (called at paint). Registration order is z order (topmost = last).
    pub(crate) fn push(&self, layer: OverlayLayer) {
        self.lock().push(layer);
    }

    /// Clears the stack, retaining capacity for the next frame (the shell clears before each paint,
    /// mirroring [`HitTest`](crate::HitTest)).
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Whether no overlays are open.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Whether `pos` is outside every open overlay's content bounds (an "outside" press). Only *open*
    /// layers count: a dismiss clears `open` before the next paint rebuilds the stack, so a just-closed
    /// layer must not still be treated as present (matches [`dismiss_topmost`](Self::dismiss_topmost)).
    pub fn pointer_outside_all(&self, pos: Point) -> bool {
        !self
            .lock()
            .iter()
            .any(|l| l.is_open() && l.bounds.contains(pos))
    }

    /// Dismisses the topmost dismiss-on-outside overlay (innermost-first). Returns whether one was
    /// dismissed. One layer per call (Esc/outside-press dismiss the innermost dismissible only).
    pub fn dismiss_topmost(&self) -> bool {
        // Clone + drop the lock before dismissing: `dismiss` writes the open signal (firing subscribers
        // synchronously) and runs a user `on_dismiss` callback, either of which could re-enter here. Only
        // still-open layers are candidates, so repeated dismisses before a repaint skip already-closed
        // overlays and fall through to the next dismissible (innermost-first).
        let layer = self
            .lock()
            .iter()
            .rev()
            .find(|l| l.dismiss_on_outside && l.is_open())
            .cloned();
        match layer {
            Some(layer) => {
                layer.dismiss();
                true
            }
            None => false,
        }
    }

    /// Whether any open overlay paints a modal backdrop (which blocks input to content beneath). Only
    /// *open* layers count (a just-dismissed backdrop, still in the stack pre-repaint, must not block).
    pub fn has_open_backdrop(&self) -> bool {
        self.lock().iter().any(|l| l.backdrop && l.is_open())
    }

    /// The topmost *open* focus-trapping overlay (innermost-first), whose scope confines Tab while it is
    /// open (a just-dismissed trap, still in the stack pre-repaint, must not keep confining focus).
    pub fn topmost_trap(&self) -> Option<OverlayLayer> {
        self.lock()
            .iter()
            .rev()
            .find(|l| l.trap_focus && l.is_open())
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactive::Owner;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn node(raw: u64) -> NodeId {
        NodeId::from_raw(raw)
    }

    /// Pushes an open overlay layer with the given bounds + opt-ins.
    fn push_layer(
        layers: &OverlayLayers,
        node_raw: u64,
        bounds: Rect,
        dismiss_on_outside: bool,
        trap_focus: bool,
        backdrop: bool,
        open: RwSignal<bool>,
    ) {
        layers.push(OverlayLayer::new(
            node(node_raw),
            bounds,
            dismiss_on_outside,
            trap_focus,
            backdrop,
            open,
            None,
        ));
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

    #[test]
    fn overlay_layers_should_detect_outside_press() {
        let owner = Owner::new();
        owner.set();

        let layers = OverlayLayers::new();
        push_layer(
            &layers,
            1,
            Rect::from_xywh(10.0, 10.0, 50.0, 50.0),
            true,
            false,
            false,
            RwSignal::new(true),
        );
        assert!(
            layers.pointer_outside_all(Point::new(5.0, 5.0)),
            "a press left of the overlay is outside"
        );
        assert!(
            !layers.pointer_outside_all(Point::new(20.0, 20.0)),
            "a press within the overlay content is not outside"
        );

        drop(owner);
    }

    #[test]
    fn overlay_outside_click_should_dismiss_topmost() {
        let owner = Owner::new();
        owner.set();

        // Two stacked dismissible overlays: only the topmost (last-registered) is dismissed, and its
        // `on_dismiss` runs; the lower overlay is untouched.
        let layers = OverlayLayers::new();
        let lower = RwSignal::new(true);
        let upper = RwSignal::new(true);
        let fired = Arc::new(AtomicBool::new(false));
        push_layer(
            &layers,
            1,
            Rect::from_xywh(0.0, 0.0, 40.0, 40.0),
            true,
            false,
            false,
            lower,
        );
        let fired_cb = Arc::clone(&fired);
        layers.push(OverlayLayer::new(
            node(2),
            Rect::from_xywh(50.0, 50.0, 40.0, 40.0),
            true,
            false,
            false,
            upper,
            Some(Arc::new(move || fired_cb.store(true, Ordering::SeqCst))),
        ));

        assert!(
            layers.dismiss_topmost(),
            "a dismissible overlay was dismissed"
        );
        assert!(!upper.get_untracked(), "the topmost overlay closed");
        assert!(lower.get_untracked(), "the lower overlay stayed open");
        assert!(fired.load(Ordering::SeqCst), "the topmost's on_dismiss ran");

        // A second call dismisses the next (now-topmost) dismissible — one layer per call.
        assert!(
            layers.dismiss_topmost(),
            "the next dismissible was dismissed"
        );
        assert!(
            !lower.get_untracked(),
            "the lower overlay closed on the second call"
        );

        drop(owner);
    }

    #[test]
    fn overlay_esc_should_dismiss_innermost() {
        let owner = Owner::new();
        owner.set();

        // Esc routes through the same `dismiss_topmost`: the innermost (topmost) dismissible closes,
        // one layer per Esc; a non-dismissible overlay above it is skipped.
        let layers = OverlayLayers::new();
        let dismissible = RwSignal::new(true);
        let modal = RwSignal::new(true);
        push_layer(
            &layers,
            1,
            Rect::from_xywh(0.0, 0.0, 40.0, 40.0),
            true,
            false,
            false,
            dismissible,
        );
        // A non-dismissible modal on top (dismiss_on_outside = false): Esc must skip it.
        push_layer(
            &layers,
            2,
            Rect::from_xywh(0.0, 0.0, 40.0, 40.0),
            false,
            false,
            true,
            modal,
        );

        assert!(layers.dismiss_topmost(), "the innermost dismissible closed");
        assert!(
            modal.get_untracked(),
            "the non-dismissible modal was skipped"
        );
        assert!(
            !dismissible.get_untracked(),
            "the dismissible overlay closed"
        );

        drop(owner);
    }

    #[test]
    fn overlay_layers_should_report_backdrop_and_trap() {
        let owner = Owner::new();
        owner.set();

        let layers = OverlayLayers::new();
        assert!(!layers.has_open_backdrop(), "no backdrop when empty");
        assert!(layers.topmost_trap().is_none(), "no trap when empty");

        push_layer(
            &layers,
            1,
            Rect::from_xywh(0.0, 0.0, 40.0, 40.0),
            true,
            true,
            true,
            RwSignal::new(true),
        );
        assert!(layers.has_open_backdrop(), "an open backdrop is reported");
        assert_eq!(
            layers.topmost_trap().map(|l| l.node),
            Some(node(1)),
            "the trap overlay is reported as the trap scope"
        );

        drop(owner);
    }

    #[test]
    fn overlay_modal_backdrop_should_block_without_dismissing() {
        let owner = Owner::new();
        owner.set();

        // A non-dismissible modal (backdrop + dismiss_on_outside=false): an outside press cannot dismiss
        // it, but `has_open_backdrop` is true, so the shell consumes the press (modal blocks input).
        let layers = OverlayLayers::new();
        let open = RwSignal::new(true);
        push_layer(
            &layers,
            1,
            Rect::from_xywh(20.0, 20.0, 40.0, 40.0),
            false,
            false,
            true,
            open,
        );

        assert!(
            !layers.dismiss_topmost(),
            "a non-dismissible modal is not dismissed"
        );
        assert!(open.get_untracked(), "the modal stays open");
        assert!(
            layers.has_open_backdrop(),
            "the modal backdrop still blocks (shell consumes the outside press)"
        );

        drop(owner);
    }

    #[test]
    fn overlay_layers_should_ignore_closed_layers() {
        let owner = Owner::new();
        owner.set();

        // A dismissible backdrop+trap overlay, dismissed but not yet repainted-away (still in the stack
        // with open=false): the query methods must ignore it, so a second input does not see a ghost.
        let layers = OverlayLayers::new();
        let open = RwSignal::new(true);
        push_layer(
            &layers,
            1,
            Rect::from_xywh(10.0, 10.0, 50.0, 50.0),
            true,
            true,
            true,
            open,
        );
        assert!(layers.dismiss_topmost(), "the overlay is dismissed");
        assert!(!open.get_untracked(), "its open signal is cleared");

        // The entry lingers until the next paint, but every query treats it as gone.
        assert!(
            layers.pointer_outside_all(Point::new(20.0, 20.0)),
            "a press over the closed overlay's old bounds is now 'outside'"
        );
        assert!(
            !layers.has_open_backdrop(),
            "the closed overlay's backdrop no longer blocks"
        );
        assert!(
            layers.topmost_trap().is_none(),
            "the closed overlay no longer traps focus"
        );

        drop(owner);
    }
}
