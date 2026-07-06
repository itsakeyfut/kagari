//! The [`Spinner`] widget (#87): an indeterminate loading indicator — an `Accent` segment that bounces
//! across a `Border` track. `Animated` is settling-only (it converges to a target), so a *looping*
//! spinner is driven by a small custom [`Tickable`] (`SpinnerDriver`) registered on the #253 per-window
//! [`Ticker`] + a scheduler active source (continuous redraw while visible). The driver stops — releasing
//! the active source — when the spinner's owner scope is disposed (`on_cleanup`), so a removed spinner no
//! longer drives redraws. The segment moves via a **paint-time** [`transform`] offset (sub-pixel smooth,
//! no per-frame relayout — RK-035 / #266).

use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kagari_base::{Point, Size};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, on_cleanup, rx, use_context};
use kagari_core::{ActiveSources, AnyElement, IntoElement, Tickable, Ticker, div, transform};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, SPINNER_PERIOD, spinner_dims};

/// Drives a [`Spinner`]'s phase each frame (#87): a [`Tickable`] on the per-window ticker that advances a
/// `0.0..1.0` phase (wrapping) until its spinner's owner scope is disposed (`alive` flips `false`), at
/// which point it releases the active source and returns `false` so the ticker prunes it.
struct SpinnerDriver {
    phase: RwSignal<f32>,
    alive: Arc<AtomicBool>,
    active: ActiveSources,
}

impl Tickable for SpinnerDriver {
    fn tick(&self, dt: Duration) -> bool {
        if !self.alive.load(Ordering::Relaxed) {
            // The spinner was removed: release the continuous-redraw source and let the ticker drop us.
            self.active.unregister();
            return false;
        }
        let next = (self.phase.get_untracked() + dt.as_secs_f32() / SPINNER_PERIOD).rem_euclid(1.0);
        self.phase.set(next);
        true
    }
}

/// The eased bounce position for `phase` (`0.0..1.0`) — a smooth `0 → 1 → 0` (cosine), so the segment
/// accelerates away from and decelerates back to each end (no abrupt reversal).
fn bounce(phase: f32) -> f32 {
    0.5 - 0.5 * (phase * TAU).cos()
}

/// An indeterminate spinner (#87). Build with [`spinner`]; chain `.size(..)`. Returns `impl IntoElement`.
pub struct Spinner {
    size: ControlSize,
}

/// Creates a medium indeterminate spinner.
pub fn spinner() -> Spinner {
    Spinner {
        size: ControlSize::Md,
    }
}

impl Spinner {
    /// Sets the spinner size on the shared control scale (D3).
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }
}

impl IntoElement for Spinner {
    fn into_element(self) -> AnyElement {
        let (track_w, track_h, seg_w) = spinner_dims(self.size);
        let (track_w, track_h, seg_w) = (track_w.0, track_h.0, seg_w.0);
        let travel = track_w - seg_w;

        let phase = RwSignal::new(0.0_f32);
        let alive = Arc::new(AtomicBool::new(true));
        // Drive the phase live via the per-window ticker (a GPU-free test provides these and drives
        // `tick_all`). Absent (no App context) → the spinner builds static. Stop the driver when this
        // owner scope is disposed (the spinner removed) so it stops driving redraws.
        if let (Some(ticker), Some(active)) =
            (use_context::<Ticker>(), use_context::<ActiveSources>())
        {
            active.register();
            ticker.register(Box::new(SpinnerDriver {
                phase,
                alive: Arc::clone(&alive),
                active,
            }));
            on_cleanup(move || alive.store(false, Ordering::Relaxed));
        }

        // The segment slides via a paint-time transform offset (smooth; the layout is unchanged). It stays
        // within `[0, travel]` → inside the track, so no clipping is needed.
        let segment = transform(
            div()
                .size(Size {
                    w: seg_w,
                    h: track_h,
                })
                .bg(ColorRole::Accent)
                .rounded_full(),
        )
        .offset(rx(move || Point::new(bounce(phase.get()) * travel, 0.0)));
        div()
            .size(Size {
                w: track_w,
                h: track_h,
            })
            .bg(ColorRole::Border)
            .rounded_full()
            .child(segment)
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use kagari_base::Size as BSize;
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_layout::LayoutTree;
    use kagari_render::{Background, Scene};
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 200.0, h: 100.0 };

    #[test]
    fn spinner_should_animate() {
        // With a `Ticker` in context, driving it moves the segment: its painted x advances from the
        // track's left edge (phase 0 → bounce 0) as the phase wraps forward.
        let owner = Owner::new();
        owner.set();
        let ticker = Ticker::new();
        let active = ActiveSources::new();
        provide_context(ticker.clone());
        provide_context(active.clone());
        let theme = Theme::light();
        let accent = Background::Solid(theme.resolve_color(ColorRole::Accent));

        let mut root = spinner().into_element();
        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let damage = std::sync::Arc::new(DamageState::default());
        let seg_x = |root: &mut AnyElement,
                     arena: &mut Arena,
                     layout: &mut LayoutTree,
                     text: &mut TextSystem| {
            let mut scene = Scene::new();
            render_tree(
                root, arena, layout, text, None, None, None, None, None, None, &mut scene,
                VIEWPORT, &damage, &theme,
            )
            .unwrap();
            scene
                .quads
                .iter()
                .find(|q| q.bg == accent)
                .map_or(0.0, |q| q.bounds.origin.x)
        };

        assert_eq!(
            active.count(),
            1,
            "a visible spinner registers a continuous-redraw source"
        );
        let x0 = seg_x(&mut root, &mut arena, &mut layout, &mut text);
        for _ in 0..6 {
            ticker.tick_all(Duration::from_millis(16));
        }
        let x1 = seg_x(&mut root, &mut arena, &mut layout, &mut text);
        assert!(
            x1 > x0,
            "the segment slides as the ticker drives the phase: x0={x0} x1={x1}"
        );
        drop(owner);
    }

    #[test]
    fn spinner_should_stop_when_dropped() {
        // A spinner built in a child owner registers a redraw source; disposing that owner (the spinner
        // "removed") stops the driver, which releases the source on its next tick.
        let owner = Owner::new();
        owner.set();
        let ticker = Ticker::new();
        let active = ActiveSources::new();
        provide_context(ticker.clone());
        provide_context(active.clone());

        let child = owner.child();
        let _el = child.with(|| spinner().into_element());
        assert_eq!(active.count(), 1, "the visible spinner drives redraws");

        child.cleanup(); // dispose the owner scope → `on_cleanup` flips `alive`
        ticker.tick_all(Duration::from_millis(16)); // the driver observes it, unregisters, and is pruned
        assert_eq!(
            active.count(),
            0,
            "a removed spinner releases its redraw source"
        );
        drop(owner);
    }
}
