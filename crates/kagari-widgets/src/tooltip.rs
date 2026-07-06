//! The [`Tooltip`] widget (#80): a hover-delayed, auto-flipping label shown over a target element.
//!
//! On pointer-enter, a custom [`Tickable`] (`HoverDelay`) accumulates frame time; once the delay elapses
//! **while still hovered**, it opens an anchored [`overlay()`] bubble over the target. A pointer-leave hides
//! it. The bubble mounts via [`dyn_if`] (not `Overlay::open`), so it draws through the #219 overlay portal
//! (high z-band, escaping clips) **without touching focus** — a tooltip must never steal focus. Selection
//! of the delay/placement is via `.delay(..)` / `.placement(..)`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kagari_base::SharedString;
use kagari_core::element::dyn_if;
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, on_cleanup, use_context};
use kagari_core::{
    ActiveSources, AnchorHandle, AnyElement, IntoElement, Placement, Tickable, Ticker, div,
    overlay, text,
};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, label_px};

/// The default hover delay before a tooltip appears.
const DEFAULT_DELAY: Duration = Duration::from_millis(500);

/// Drives a [`Tooltip`]'s hover→show delay (#80): a [`Tickable`] registered on pointer-enter that
/// accumulates frame time and, once `delay` elapses **while still hovered**, opens the tooltip. A
/// pointer-leave (or the tooltip's removal) clears `hovering`, so the next tick cancels without showing.
/// Mirrors the spinner's `SpinnerDriver` (a signal-backed driver on the #253 per-window [`Ticker`]).
struct HoverDelay {
    delay: Duration,
    /// Accumulated hover time. Interior-mutable behind a `Mutex` since [`Tickable::tick`] takes `&self`.
    elapsed: Mutex<Duration>,
    /// `true` between enter and leave; cleared to cancel a pending show (leave / removal).
    hovering: Arc<AtomicBool>,
    /// `true` while a timer entry is live in the ticker — stops a rapid re-enter from registering a
    /// duplicate driver.
    pending: Arc<AtomicBool>,
    /// Opened when the delay elapses; drives the `dyn_if` that mounts the bubble.
    open: RwSignal<bool>,
    /// The continuous-redraw source held while the delay runs (released on finish).
    active: ActiveSources,
}

impl HoverDelay {
    /// Releases the timer: drops the pending guard + the continuous-redraw source, so the ticker prunes
    /// this entry and the window can return to idle.
    fn finish(&self) {
        self.pending.store(false, Ordering::Relaxed);
        self.active.unregister();
    }
}

impl Tickable for HoverDelay {
    fn tick(&self, dt: Duration) -> bool {
        // Cancelled: the pointer left (or the tooltip was removed) before the delay elapsed.
        if !self.hovering.load(Ordering::Relaxed) {
            self.finish();
            return false;
        }
        let elapsed = {
            let mut e = self.elapsed.lock().unwrap_or_else(|p| p.into_inner());
            *e += dt;
            *e
        };
        if elapsed >= self.delay {
            // Show, then release. The lock is dropped above before `set`, which fires effects synchronously.
            self.open.set(true);
            self.finish();
            return false;
        }
        true
    }
}

/// A hover-delayed, auto-flipping label shown over a target element (#80). Build with [`tooltip`]; chain
/// `.delay(..)` / `.placement(..)`. Returns `impl IntoElement`. The tooltip appears after a hover delay,
/// hides on leave, and never takes focus.
pub struct Tooltip {
    target: AnyElement,
    content: SharedString,
    delay: Duration,
    placement: Placement,
}

/// Wraps `target` with a hover tooltip showing `content` (#80). Appears after a 500ms hover delay (see
/// [`Tooltip::delay`]), anchored above the target (see [`Tooltip::placement`]).
pub fn tooltip(target: impl IntoElement, content: impl Into<SharedString>) -> Tooltip {
    Tooltip {
        target: target.into_element(),
        content: content.into(),
        delay: DEFAULT_DELAY,
        placement: Placement::Above,
    }
}

impl Tooltip {
    /// Sets the hover delay before the tooltip appears (default 500ms).
    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Sets the anchor placement relative to the target (default [`Placement::Above`]; the core clamps
    /// the tooltip on-screen and `Auto` flips it to fit).
    pub fn placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }
}

impl IntoElement for Tooltip {
    fn into_element(self) -> AnyElement {
        let Tooltip {
            target,
            content,
            delay,
            placement,
        } = self;

        let handle = AnchorHandle::new();
        let open = RwSignal::new(false);
        let hovering = Arc::new(AtomicBool::new(false));
        let pending = Arc::new(AtomicBool::new(false));

        // The scheduler handles for the hover-delay timer (a GPU-free test provides these and drives
        // `tick_all`; absent — no App context — the tooltip simply never shows, the correct headless
        // degradation). Captured at build, mirroring the spinner.
        let ticker = use_context::<Ticker>();
        let active = use_context::<ActiveSources>();

        // Cancel a pending delay if the tooltip is removed mid-hover (its owner scope is disposed).
        {
            let hovering = Arc::clone(&hovering);
            on_cleanup(move || hovering.store(false, Ordering::Relaxed));
        }

        let enter_hovering = Arc::clone(&hovering);
        let enter_pending = Arc::clone(&pending);
        let leave_hovering = Arc::clone(&hovering);
        let bubble_handle = handle.clone();

        div()
            .anchor_ref(&handle)
            // Pointer-enter: arm the hover state and, unless a timer is already pending, register the
            // delay driver (+ a continuous-redraw source so the frame loop advances it).
            .on_mouse_enter(move |_ev, _cx| {
                enter_hovering.store(true, Ordering::Relaxed);
                // Both the ticker and an active source are required to time the delay (the source keeps
                // the frame loop advancing); require both, like the spinner (RK-033) — a ticker without a
                // source would leave `pending` stuck true (registered but never advanced).
                if let (Some(ticker), Some(active)) = (&ticker, &active) {
                    if !enter_pending.swap(true, Ordering::Relaxed) {
                        active.register();
                        ticker.register(Box::new(HoverDelay {
                            delay,
                            elapsed: Mutex::new(Duration::ZERO),
                            hovering: Arc::clone(&enter_hovering),
                            pending: Arc::clone(&enter_pending),
                            open,
                            active: active.clone(),
                        }));
                    }
                }
            })
            // Pointer-leave: disarm + hide. A pending timer sees `hovering == false` next tick and
            // cancels; a shown tooltip unmounts (`open → false`).
            .on_mouse_leave(move |_ev, _cx| {
                leave_hovering.store(false, Ordering::Relaxed);
                open.set(false);
            })
            .child(target)
            // The bubble mounts only while `open` (via `dyn_if`, driven by the frame-loop reconcile,
            // #278). An inverse-surface label (dark bubble on light, light on dark — contrast-safe on any
            // background, RK-030), anchored over the target. No `Overlay::open` → never touches focus.
            .child(dyn_if(
                move || open.get(),
                move || {
                    let content = content.clone();
                    overlay(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(ColorRole::Text)
                            .shadow_sm()
                            .child(
                                text(content)
                                    .color_role(ColorRole::Surface)
                                    .size(label_px(ControlSize::Sm)),
                            ),
                    )
                    .anchor(&bubble_handle, placement)
                },
            ))
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use kagari_base::{Color, Point, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::FocusRegistry;
    use kagari_core::overlay::OverlayRegistry;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{DispatchState, HitTest, Modifiers, MouseEvent, MouseKind, dispatch_mouse};
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 200.0, h: 200.0 };
    /// The overlay portal's high painter-order band (mirrors `overlay.rs`): a shown tooltip's bubble
    /// paints here, so its presence tells us the tooltip is visible.
    const OVERLAY_BAND: u32 = 1 << 28;

    fn red() -> Background {
        Background::Solid(Color::new(1.0, 0.0, 0.0, 1.0))
    }

    /// A tooltip over a 40x20 red target, plus a fresh render harness.
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

    fn harness(tip: Tooltip) -> Harness {
        Harness {
            root: tip.into_element(),
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            reg: OverlayRegistry::new(),
            damage: Arc::new(DamageState::default()),
            theme: Theme::light(),
        }
    }

    /// Renders one frame (populating the hit-test + overlay registry) and returns the scene.
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

    /// A shown tooltip paints its bubble in the overlay band.
    fn tooltip_shown(scene: &Scene) -> bool {
        scene.quads.iter().any(|q| q.order >= OVERLAY_BAND)
    }

    fn move_to(h: &mut Harness, x: f32, y: f32, state: &mut DispatchState) {
        let ev = MouseEvent::new(MouseKind::Move, Point::new(x, y), Modifiers::default());
        dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, state);
    }

    fn target() -> impl IntoElement {
        div().size(BSize { w: 40.0, h: 20.0 }).background(red())
    }

    #[test]
    fn tooltip_should_show_after_hover_delay() {
        let owner = Owner::new();
        owner.set();
        let ticker = Ticker::new();
        provide_context(ticker.clone());
        provide_context(ActiveSources::new());

        let mut h = harness(tooltip(target(), "Hi").delay(Duration::from_millis(500)));
        let mut state = DispatchState::default();

        // Frame 1: idle — the bubble is not shown.
        let s0 = frame(&mut h);
        assert!(!tooltip_shown(&s0), "hidden before any hover");

        // Hover the target (its center) — arms the delay timer.
        move_to(&mut h, 20.0, 10.0, &mut state);

        // Before the delay elapses: still hidden.
        ticker.tick_all(Duration::from_millis(200));
        let s_mid = frame(&mut h);
        assert!(!tooltip_shown(&s_mid), "hidden before the delay elapses");

        // Past the delay (200 + 400 = 600 >= 500): the bubble shows.
        ticker.tick_all(Duration::from_millis(400));
        let s1 = frame(&mut h);
        assert!(tooltip_shown(&s1), "shown after the hover delay");

        drop(owner);
    }

    #[test]
    fn tooltip_should_hide_on_leave() {
        let owner = Owner::new();
        owner.set();
        let ticker = Ticker::new();
        provide_context(ticker.clone());
        provide_context(ActiveSources::new());

        let mut h = harness(tooltip(target(), "Hi").delay(Duration::from_millis(100)));
        let mut state = DispatchState::default();
        frame(&mut h);

        // Hover + elapse the delay → shown.
        move_to(&mut h, 20.0, 10.0, &mut state);
        ticker.tick_all(Duration::from_millis(150));
        assert!(tooltip_shown(&frame(&mut h)), "shown while hovering");

        // Move off the target → leave → hidden.
        move_to(&mut h, 150.0, 150.0, &mut state);
        assert!(
            !tooltip_shown(&frame(&mut h)),
            "hidden after the pointer leaves"
        );

        drop(owner);
    }

    #[test]
    fn tooltip_should_not_steal_focus() {
        let owner = Owner::new();
        owner.set();
        let ticker = Ticker::new();
        provide_context(ticker.clone());
        provide_context(ActiveSources::new());
        let reg = FocusRegistry::new();
        provide_context(reg.clone());

        // Something else holds focus before (and while) the tooltip shows.
        let focused = reg.handle();
        focused.focus();

        let mut h = harness(tooltip(target(), "Hi").delay(Duration::from_millis(100)));
        let mut state = DispatchState::default();
        frame(&mut h);
        move_to(&mut h, 20.0, 10.0, &mut state);
        ticker.tick_all(Duration::from_millis(150));
        assert!(tooltip_shown(&frame(&mut h)), "the tooltip is shown");

        assert_eq!(
            reg.current().get_untracked(),
            Some(focused.id()),
            "focus is unchanged — a tooltip never takes focus (no Overlay::open focus machinery)"
        );

        drop(owner);
    }

    #[test]
    fn tooltip_should_cancel_delay_on_removal() {
        // RK-037: removing the tooltip mid-hover (its owner scope disposed) must cancel the pending delay
        // and release the continuous-redraw source, so the window can return to idle.
        let owner = Owner::new();
        owner.set();
        let ticker = Ticker::new();
        provide_context(ticker.clone());
        let active = ActiveSources::new();
        provide_context(active.clone());

        // Build the tooltip under a child owner (as a conditional mount would), then hover to arm it.
        let child = owner.child();
        let mut h =
            child.with(|| harness(tooltip(target(), "Hi").delay(Duration::from_millis(500))));
        let mut state = DispatchState::default();
        frame(&mut h);
        move_to(&mut h, 20.0, 10.0, &mut state);
        ticker.tick_all(Duration::from_millis(100));
        assert!(
            active.any(),
            "an active source is held while the delay runs"
        );

        // Remove the tooltip mid-delay: `on_cleanup` clears `hovering`, so the next tick cancels.
        child.cleanup();
        ticker.tick_all(Duration::from_millis(50));
        assert!(
            !active.any(),
            "the active source is released when the tooltip is removed mid-delay"
        );

        drop(owner);
    }
}
