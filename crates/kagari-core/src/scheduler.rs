//! Frame scheduler (#36): hybrid driving — on-demand redraw on damage (#35), continuous while an
//! active source is registered, idle otherwise (zero redraw / GPU work, §1.6).
//!
//! The present mode is vsync (wgpu `Fifo`, configured in [`app`](crate::app)) and kept swappable:
//! low-latency present (Mailbox/Immediate) and a render-thread split are post-MVP (§1.6).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A cloneable handle to the active-source count, shared with continuous drivers (animations, media
/// playback, a live GPU canvas) that need per-vsync redraw. Registering keeps the app on continuous
/// redraw; the last deregister returns it to on-demand.
///
/// The `Animated<T>` driver (`anim.rs`, #59) holds one — captured from the app-root context — and
/// registers while animating / deregisters on convergence. [`Scheduler`] reads it for
/// [`has_active_sources`](Scheduler::has_active_sources). Cloneable so a shared driver can register
/// without a `&mut Scheduler`.
#[derive(Clone, Default, Debug)]
pub struct ActiveSources(Arc<AtomicUsize>);

impl ActiveSources {
    /// A handle with no active sources.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one active source (the app drives continuously while the count is non-zero).
    pub fn register(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    /// Deregisters one active source, saturating at zero so an unbalanced deregister cannot
    /// underflow.
    pub fn unregister(&self) {
        let _ = self
            .0
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |c| {
                Some(c.saturating_sub(1))
            });
    }

    /// The current active-source count.
    pub fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }

    /// Whether any active source is registered.
    pub fn any(&self) -> bool {
        self.count() > 0
    }
}

/// Tracks "active sources" — animations, media playback, or a live GPU canvas — that require
/// continuous per-vsync redraw. While any is registered the app drives continuously; when the last
/// one deregisters (and nothing is dirty) it returns to on-demand driving.
///
/// The shared [`ActiveSources`] handle ([`active_sources`](Self::active_sources)) lets a cloned
/// driver (an `Animated<T>`) register without a `&mut Scheduler`.
#[derive(Debug, Default)]
pub struct Scheduler {
    active: ActiveSources,
}

impl Scheduler {
    /// Creates a scheduler with no active sources (purely on-demand).
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an active source. While the count is non-zero the app redraws every vsync.
    pub fn register_active_source(&mut self) {
        self.active.register();
    }

    /// Deregisters an active source, saturating at zero so an unbalanced deregister cannot
    /// underflow.
    pub fn unregister_active_source(&mut self) {
        self.active.unregister();
    }

    /// Whether any active source is currently registered (drives continuous redraw).
    pub fn has_active_sources(&self) -> bool {
        self.active.any()
    }

    /// A cloneable handle to the active-source count, for continuous drivers (animations, #59) to
    /// register/deregister without a `&mut Scheduler`. Provide it into the app-root reactive context
    /// so `Animated<T>::new` can capture it.
    pub fn active_sources(&self) -> ActiveSources {
        self.active.clone()
    }
}

/// A per-frame-driven source (#253) — an [`Animated<T>`](crate::anim::Animated) — advanced by the app's
/// redraw each frame while it runs. `tick(dt)` returns `false` once settled, so the [`Ticker`] drops it.
pub trait Tickable: Send + Sync {
    /// Advances by `dt`; `false` once settled.
    fn tick(&self, dt: Duration) -> bool;
}

/// A per-window registry of running [`Tickable`]s (#253): the "deferred frame loop" the anim/scroll docs
/// reference. A driver registers itself when it starts animating ([`register`](Self::register)); the app
/// calls [`tick_all`](Self::tick_all) each redraw to advance them, dropping any that have settled — so
/// when the last one settles the active-source count drops to zero and the window returns to idle.
///
/// Cloneable + `Send + Sync` so it is provided into the per-window reactive context and captured by
/// `Animated<T>::new`.
#[derive(Clone, Default)]
pub struct Ticker(Arc<Mutex<Vec<Box<dyn Tickable>>>>);

impl Ticker {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a running tickable (called when an animation starts from rest). No-op on a poisoned
    /// lock — a driver panicking mid-tick leaves the registry usable.
    pub fn register(&self, tickable: Box<dyn Tickable>) {
        if let Ok(mut running) = self.0.lock() {
            running.push(tickable);
        }
    }

    /// Advances every registered tickable by `dt`, dropping any that have settled. The set is **drained
    /// out of the lock before ticking**: a `tick` writes a signal whose effect may re-enter
    /// [`register`](Self::register) (a chained animation), which would deadlock a held lock. Survivors are
    /// re-appended ahead of anything registered mid-tick.
    pub fn tick_all(&self, dt: Duration) {
        let mut running = match self.0.lock() {
            Ok(mut guard) => std::mem::take(&mut *guard),
            Err(_) => return,
        };
        running.retain_mut(|t| t.tick(dt));
        if let Ok(mut guard) = self.0.lock() {
            running.append(&mut guard);
            *guard = running;
        }
    }
}

/// Whether the app should request a redraw this turn: something is dirty (#35 damage) or an active
/// source is animating. Idle (neither) → no redraw, no GPU work (§1.6).
///
/// This is the redraw-decision predicate, kept free of winit/GPU state so it is unit-testable; the
/// app calls it from `about_to_wait` and only then issues `request_redraw`.
pub fn should_redraw(dirty: bool, active_sources: bool) -> bool {
    dirty || active_sources
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticker_should_drop_settled_and_keep_running() {
        // A tickable that settles (returns false) after `settle_after` ticks; a shared counter records
        // every tick so the test can prove the settled one is dropped (no longer ticked).
        struct Src {
            ticks: Arc<AtomicUsize>,
            own: AtomicUsize,
            settle_after: usize,
        }
        impl Tickable for Src {
            fn tick(&self, _dt: Duration) -> bool {
                self.ticks.fetch_add(1, Ordering::SeqCst);
                (self.own.fetch_add(1, Ordering::SeqCst) + 1) < self.settle_after
            }
        }

        let ticks = Arc::new(AtomicUsize::new(0));
        let ticker = Ticker::new();
        // A settles on its first tick; B keeps running.
        ticker.register(Box::new(Src {
            ticks: ticks.clone(),
            own: AtomicUsize::new(0),
            settle_after: 1,
        }));
        ticker.register(Box::new(Src {
            ticks: ticks.clone(),
            own: AtomicUsize::new(0),
            settle_after: 999,
        }));

        let dt = Duration::from_millis(16);
        ticker.tick_all(dt); // A ticks (→drop) + B ticks = 2
        ticker.tick_all(dt); // only B ticks = 3
        assert_eq!(
            ticks.load(Ordering::SeqCst),
            3,
            "the settled tickable is dropped after its first tick; the runner keeps ticking"
        );
    }

    #[test]
    fn ticker_tick_all_should_allow_reentrant_register() {
        // The drain-out-of-lock design is load-bearing: a `tick` may write a signal whose effect
        // re-enters `register` (a chained animation). Prove `tick_all` does not deadlock and the
        // re-registered tickable is retained for the next frame.
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        struct Leaf(Arc<AtomicUsize>);
        impl Tickable for Leaf {
            fn tick(&self, _dt: Duration) -> bool {
                self.0.fetch_add(1, Ordering::SeqCst);
                true // keeps running
            }
        }
        struct Chain {
            ticker: Ticker,
            leaf_ticks: Arc<AtomicUsize>,
            fired: AtomicBool,
        }
        impl Tickable for Chain {
            fn tick(&self, _dt: Duration) -> bool {
                if !self.fired.swap(true, Ordering::SeqCst) {
                    // Re-enter `register` from inside `tick_all` — must not deadlock.
                    self.ticker
                        .register(Box::new(Leaf(self.leaf_ticks.clone())));
                }
                false // settles immediately
            }
        }

        let ticker = Ticker::new();
        let leaf_ticks = Arc::new(AtomicUsize::new(0));
        ticker.register(Box::new(Chain {
            ticker: ticker.clone(),
            leaf_ticks: leaf_ticks.clone(),
            fired: AtomicBool::new(false),
        }));

        let dt = Duration::from_millis(16);
        ticker.tick_all(dt); // Chain ticks: registers Leaf, settles → dropped. No deadlock.
        assert_eq!(
            leaf_ticks.load(Ordering::SeqCst),
            0,
            "the Leaf registered mid-tick is not ticked in the same frame"
        );
        ticker.tick_all(dt); // Leaf ticks next frame (retained across the re-entrant register)
        assert_eq!(
            leaf_ticks.load(Ordering::SeqCst),
            1,
            "the re-registered Leaf is retained and ticked next frame"
        );
    }

    #[test]
    fn scheduler_should_request_redraw_only_when_dirty_or_active() {
        assert!(
            !should_redraw(false, false),
            "idle: neither dirty nor active"
        );
        assert!(
            should_redraw(true, false),
            "a dirty state requests a redraw"
        );
        assert!(
            should_redraw(false, true),
            "an active source drives a redraw"
        );
        assert!(should_redraw(true, true), "dirty and active both request");
    }

    #[test]
    fn scheduler_should_track_active_source_registration() {
        let mut scheduler = Scheduler::new();
        assert!(
            !scheduler.has_active_sources(),
            "fresh scheduler is on-demand"
        );

        scheduler.register_active_source();
        assert!(scheduler.has_active_sources());

        // A second registration keeps it active until *both* deregister.
        scheduler.register_active_source();
        scheduler.unregister_active_source();
        assert!(
            scheduler.has_active_sources(),
            "one active source still remains"
        );

        scheduler.unregister_active_source();
        assert!(
            !scheduler.has_active_sources(),
            "back to on-demand once none remain"
        );
    }

    #[test]
    fn unregister_active_source_should_saturate_at_zero() {
        let mut scheduler = Scheduler::new();
        // An unbalanced deregister must not underflow (saturating_sub).
        scheduler.unregister_active_source();
        assert!(!scheduler.has_active_sources());
    }

    #[test]
    fn active_sources_should_share_count_across_clones() {
        // A cloned `ActiveSources` handle shares the same atomic count with the scheduler, so a
        // driver (an `Animated<T>`) can register without a `&mut Scheduler`.
        let scheduler = Scheduler::new();
        let a = scheduler.active_sources();
        let b = a.clone();

        a.register();
        assert_eq!(b.count(), 1, "clones share the count");
        assert!(
            scheduler.has_active_sources(),
            "the scheduler sees a driver's registration"
        );

        b.register();
        assert_eq!(scheduler.active_sources().count(), 2);

        a.unregister();
        b.unregister();
        assert!(
            !scheduler.has_active_sources(),
            "back to on-demand once none remain"
        );

        // Saturating: an unbalanced deregister does not underflow.
        a.unregister();
        assert_eq!(a.count(), 0);
    }
}
