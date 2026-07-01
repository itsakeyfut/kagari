//! Frame scheduler (#36): hybrid driving — on-demand redraw on damage (#35), continuous while an
//! active source is registered, idle otherwise (zero redraw / GPU work, §1.6).
//!
//! The present mode is vsync (wgpu `Fifo`, configured in [`app`](crate::app)) and kept swappable:
//! low-latency present (Mailbox/Immediate) and a render-thread split are post-MVP (§1.6).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
