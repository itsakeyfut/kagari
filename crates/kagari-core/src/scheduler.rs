//! Frame scheduler (#36): hybrid driving — on-demand redraw on damage (#35), continuous while an
//! active source is registered, idle otherwise (zero redraw / GPU work, §1.6).
//!
//! The present mode is vsync (wgpu `Fifo`, configured in [`app`](crate::app)) and kept swappable:
//! low-latency present (Mailbox/Immediate) and a render-thread split are post-MVP (§1.6).

/// Tracks "active sources" — animations, media playback, or a live GPU canvas — that require
/// continuous per-vsync redraw. While any is registered the app drives continuously; when the last
/// one deregisters (and nothing is dirty) it returns to on-demand driving.
///
/// Phase 3 has no consumer yet: the registration API is a seam for the animation/playback layers
/// (`anim.rs`, post-MVP). A drop-guard registration handle is deferred to that consumer.
#[derive(Debug, Default)]
pub struct Scheduler {
    active_sources: usize,
}

impl Scheduler {
    /// Creates a scheduler with no active sources (purely on-demand).
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an active source. While the count is non-zero the app redraws every vsync.
    pub fn register_active_source(&mut self) {
        self.active_sources += 1;
    }

    /// Deregisters an active source, saturating at zero so an unbalanced deregister cannot
    /// underflow.
    pub fn unregister_active_source(&mut self) {
        self.active_sources = self.active_sources.saturating_sub(1);
    }

    /// Whether any active source is currently registered (drives continuous redraw).
    pub fn has_active_sources(&self) -> bool {
        self.active_sources > 0
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
}
