//! The [`Dialog`] widget (#82): a modal whose visibility is a controlled `RwSignal<bool>`.
//!
//! While `open` is true, a full-viewport **backdrop** + **centered** content render on the #219 substrate:
//! focus is **trapped** within the dialog (Tab cycles inside), **returned** to the prior node on close, and
//! Esc / a backdrop press closes it (sets `open` false). It is [`Popover`](crate::Popover)'s modal
//! counterpart — the interactive `.open()` path with `.backdrop()` + `.trap_focus()`.
//!
//! The content mounts via `dyn_if(open)` **inside** the overlay (so its focusables register only while open,
//! and the overlay's open-state effect can restore focus on close). The enter transition is a **scale-in**
//! (0.9→1.0 about the content's center) via [`Animated`] + [`transform`] — a paint-time effect (no relayout).

use kagari_core::element::dyn_if;
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{Animated, AnyElement, IntoElement, TransformOrigin, overlay, transform};

use crate::control::CONTROL_SPRING;

/// The scale a dialog's content grows from on open (to `1.0`) — a subtle scale-in.
const DIALOG_SCALE_FROM: f32 = 0.9;

/// A dialog's content **builder**: called to build the modal content each time it opens (a closure, so its
/// focusables mount/unmount with the open state — needed for the focus trap — and each open re-runs the
/// scale-in).
type DialogContent = Box<dyn Fn() -> AnyElement>;

/// A modal dialog bound to a `bool` signal (#82). Build with [`dialog`]; chain `.dismiss_on_outside(..)`.
/// Returns `impl IntoElement`. While `open` is true a backdrop + centered content render, focus is trapped,
/// and Esc / a backdrop press closes it.
pub struct Dialog {
    open: RwSignal<bool>,
    content: DialogContent,
    dismiss_on_outside: bool,
}

/// Creates a modal dialog whose visibility is controlled by `open`, showing `content` (a builder — rebuilt
/// each time it opens) centered over a backdrop. Focus is trapped while open and returned on close (#219).
pub fn dialog<C: IntoElement>(open: RwSignal<bool>, content: impl Fn() -> C + 'static) -> Dialog {
    Dialog {
        open,
        content: Box::new(move || content().into_element()),
        dismiss_on_outside: true,
    }
}

impl Dialog {
    /// Whether Esc / a backdrop press dismisses the dialog (default `true`; #219). Set `false` for a dialog
    /// dismissed only by toggling `open` yourself (e.g. a forced confirmation).
    pub fn dismiss_on_outside(mut self, dismiss: bool) -> Self {
        self.dismiss_on_outside = dismiss;
        self
    }
}

impl IntoElement for Dialog {
    fn into_element(self) -> AnyElement {
        let Dialog {
            open,
            content,
            dismiss_on_outside,
        } = self;

        overlay(dyn_if(
            move || open.get(),
            move || {
                // Scale-in enter transition: a fresh `Animated` per open grows the content 0.9→1.0 about its
                // own center (paint-time via `transform`, no relayout — RK-035). Settles then stops (RK-037);
                // disposed when the dialog closes (dyn_if unmount). Absent a `Ticker` (a GPU-free test) the
                // content simply stays at the start scale — still fully rendered.
                let scale = Animated::new(DIALOG_SCALE_FROM, CONTROL_SPRING);
                scale.set_target(1.0);
                transform(content())
                    .scale(rx(move || scale.get()))
                    .origin(TransformOrigin::CENTER)
            },
        ))
        // Centered in the viewport (#287); a full-viewport backdrop + focus-trap make it modal; `.open()`
        // paint-gates + saves/restores focus + registers the dismiss/trap layer. dyn_if is INSIDE so the
        // overlay stays mounted and its open-state effect restores focus on close.
        .center()
        .open(open)
        .backdrop(true)
        .trap_focus(true)
        .dismiss_on_outside(dismiss_on_outside)
        .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use kagari_base::{Color, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::FocusRegistry;
    use kagari_core::overlay::{OverlayLayers, OverlayRegistry};
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{ActiveSources, HitTest, Ticker, div};
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 200.0, h: 200.0 };
    const OVERLAY_BAND: u32 = 1 << 28;

    fn content_bg() -> Background {
        Background::Solid(Color::new(0.0, 0.0, 1.0, 1.0))
    }

    fn content() -> impl IntoElement {
        div()
            .size(BSize { w: 80.0, h: 60.0 })
            .background(content_bg())
    }

    struct Harness {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        hit: HitTest,
        reg: OverlayRegistry,
        damage: Arc<DamageState>,
        theme: Theme,
    }

    fn harness(root: AnyElement) -> Harness {
        Harness {
            root,
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            reg: OverlayRegistry::new(),
            damage: Arc::new(DamageState::default()),
            theme: Theme::light(),
        }
    }

    fn frame(h: &mut Harness) -> Scene {
        let mut scene = Scene::new();
        render_tree(
            &mut h.root,
            &mut h.arena,
            &mut h.layout,
            &mut h.text,
            None,
            Some(&mut h.hit),
            Some(&mut h.reg),
            None,
            None,
            None,
            &mut scene,
            VIEWPORT,
            &h.damage,
            &h.theme,
        )
        .unwrap();
        scene
    }

    /// The dialog's backdrop scrim quad: full-viewport, in the overlay band, painted beneath the content.
    fn backdrop_quad(scene: &Scene) -> Option<&kagari_render::Quad> {
        scene
            .quads
            .iter()
            .find(|q| q.order >= OVERLAY_BAND && q.bounds.size == BSize { w: 200.0, h: 200.0 })
    }

    fn has_content(scene: &Scene) -> bool {
        scene
            .quads
            .iter()
            .any(|q| q.order >= OVERLAY_BAND && q.bg == content_bg())
    }

    #[test]
    fn dialog_should_render_backdrop_when_open() {
        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());

        let open = RwSignal::new(false);
        let mut h = harness(dialog(open, content).into_element());

        // Closed: nothing in the overlay band.
        let closed = frame(&mut h);
        assert!(!has_content(&closed), "closed dialog renders no content");
        assert!(
            backdrop_quad(&closed).is_none(),
            "closed dialog renders no backdrop"
        );

        // Open: a full-viewport backdrop + the centered content.
        open.set(true);
        let shown = frame(&mut h);
        assert!(has_content(&shown), "open dialog renders its content");
        assert!(
            backdrop_quad(&shown).is_some(),
            "open dialog renders a full-viewport backdrop"
        );

        // Closed again: both gone.
        open.set(false);
        assert!(!has_content(&frame(&mut h)), "closing hides the content");

        drop(owner);
    }

    #[test]
    fn dialog_should_trap_focus() {
        let owner = Owner::new();
        owner.set();
        let layers = OverlayLayers::new();
        provide_context(layers.clone());

        let open = RwSignal::new(true);
        let mut h = harness(dialog(open, content).into_element());
        assert!(has_content(&frame(&mut h)), "the dialog is open");

        assert!(
            layers.topmost_trap().is_some(),
            "the open modal dialog registers a focus-trap layer (Tab is confined within it)"
        );

        drop(owner);
    }

    #[test]
    fn dialog_backdrop_click_should_close() {
        let owner = Owner::new();
        owner.set();
        let layers = OverlayLayers::new();
        provide_context(layers.clone());

        let open = RwSignal::new(true);
        let mut h = harness(dialog(open, content).into_element());
        assert!(has_content(&frame(&mut h)), "the dialog is open");

        // Esc / a backdrop press routes to `dismiss_topmost`, clearing the layer's `open` signal.
        assert!(
            layers.dismiss_topmost(),
            "the dismissible layer is dismissed"
        );
        assert!(
            !open.get_untracked(),
            "Esc / backdrop press sets the controlled open signal false (#219)"
        );
        assert!(
            !has_content(&frame(&mut h)),
            "the closed dialog renders no content"
        );

        drop(owner);
    }

    #[test]
    fn dialog_content_click_should_reach_handler() {
        // Regression (#289 / RK-040): a click on an interactive element inside the dialog content (mounted
        // via dyn_if, through the transform + centered overlay) must reach its handler — e.g. a "Close"
        // button that flips `open`. Before #289 the click died at the dyn node and did nothing.
        use kagari_base::Point;
        use kagari_core::{
            DispatchState, Modifiers, MouseButton, MouseEvent, MouseKind, dispatch_mouse,
        };

        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());

        let open = RwSignal::new(true);
        let mut h = harness(
            dialog(open, move || {
                div()
                    .size(BSize { w: 80.0, h: 60.0 })
                    .background(content_bg())
                    .on_click(move |_, _| open.set(false))
            })
            .into_element(),
        );
        assert!(has_content(&frame(&mut h)), "the dialog is open");

        // The content is centered → its center is the viewport center (also the scale pivot), so a click
        // there lands inside it at any scale.
        let mut dispatch = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(kind, Point::new(100.0, 100.0), Modifiers::default());
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut dispatch);
        }
        assert!(
            !open.get_untracked(),
            "clicking a button inside the dialog fires its handler (closes the dialog)"
        );

        drop(owner);
    }

    #[test]
    fn dialog_close_should_return_focus() {
        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());
        let reg = FocusRegistry::new();
        provide_context(reg.clone());

        let trigger = reg.handle();
        let other = reg.handle();
        trigger.focus();

        let open = RwSignal::new(false);
        let mut h = harness(dialog(open, content).into_element());
        frame(&mut h); // build → the open-state effect is registered (prev = false)

        open.set(true);
        other.focus();
        assert!(other.is_focused(), "focus moved while the dialog is open");

        open.set(false);
        assert_eq!(
            reg.current().get_untracked(),
            Some(trigger.id()),
            "closing the dialog returns focus to the pre-open trigger"
        );

        drop(owner);
    }

    #[test]
    fn dialog_open_should_drive_scale_in() {
        let owner = Owner::new();
        owner.set();
        provide_context(OverlayLayers::new());
        let ticker = Ticker::new();
        provide_context(ticker.clone());
        let active = ActiveSources::new();
        provide_context(active.clone());

        let open = RwSignal::new(true);
        let mut h = harness(dialog(open, content).into_element());

        // Open → the content mounts and its scale-in `Animated` registers a continuous-redraw source.
        assert!(has_content(&frame(&mut h)), "the dialog is open");
        assert!(
            active.any(),
            "the scale-in animation drives a redraw source while running"
        );

        // Drive the ticker to convergence → the animation settles and releases the source.
        for _ in 0..600 {
            ticker.tick_all(std::time::Duration::from_millis(16));
        }
        assert!(
            !active.any(),
            "the scale-in settles at 1.0 and releases the redraw source"
        );

        drop(owner);
    }
}
