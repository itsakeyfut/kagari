//! Animation system (#59): [`Animated<T>`] — a spring/easing-driven, retargetable value.
//!
//! `Animated<T>` is **signal-backed** (wraps an [`RwSignal<T>`]) so a reactive paint prop that reads
//! [`get`](Animated::get) subscribes; the frame clock calls [`tick`](Animated::tick) between frames,
//! which advances the spring/easing and writes the new value into the signal — a synchronous write
//! (ADR 0001) that re-runs the subscribed prop effect and flags paint-damage (RK-009-safe: serial
//! with `render_tree`). While animating it registers a scheduler [`ActiveSources`] so the app drives
//! continuously; on convergence it snaps to the target, deregisters, and `tick` returns `false`.
//!
//! `Color` interpolates in the working **linear** space (the working color is linear-premultiplied).
//! The live app-loop drive (calling `tick` each `RedrawRequested`) lands with the deferred frame loop.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kagari_base::{Color, Point, Size};

use crate::reactive::prelude::*;
use crate::reactive::{RwSignal, use_context};
use crate::scheduler::{ActiveSources, Tickable, Ticker};

/// Spring rest thresholds, RELATIVE to the target's magnitude — the position distance and the velocity
/// magnitude are each compared against `REST_*_REL · max(1, |target|)`. A single *absolute* epsilon can't
/// serve both unit-scale values (`Color`, 0..1) and px-scale values (a scroll offset in the hundreds or
/// thousands): at large magnitudes f32 quantization pins the position a few ULPs short of the target and
/// leaves a small limit-cycle velocity, so a fixed absolute threshold is never met — the spring never
/// rests and pins the app to continuous redraw (RK-037-adjacent). Relative thresholds settle across
/// scales. The value snaps to the exact target on convergence, and convergence lands at the (sub-pixel)
/// quantization plateau, so the snap is imperceptible. `REST_SPEED_REL` is looser than `REST_DELTA_REL`
/// because the persistent limit-cycle velocity is larger (relative to scale) than the position residual.
const REST_DELTA_REL: f32 = 1e-5;
const REST_SPEED_REL: f32 = 1e-3;
/// Maximum spring integration sub-step (seconds) — keeps semi-implicit Euler stable for stiff springs.
const MAX_STEP: f32 = 1.0 / 240.0;
/// Clamp on a single frame's dt (seconds) so a huge hitch cannot explode the spring.
const MAX_FRAME: f32 = 1.0 / 30.0;

/// A value that can be spring/eased: a small vector space (add / sub / scale) with a magnitude for
/// convergence. Implemented for `f32`, [`Point`], [`Size`], and [`Color`] (interpolated in linear
/// space).
pub trait Animatable: Copy {
    /// Component-wise sum.
    fn add(self, other: Self) -> Self;
    /// Component-wise difference.
    fn sub(self, other: Self) -> Self;
    /// Scalar multiply.
    fn scale(self, factor: f32) -> Self;
    /// A magnitude/norm, for convergence thresholds.
    fn magnitude(self) -> f32;

    /// Linear interpolation from `self` to `other` by `t` (unclamped). For [`Color`] this is a
    /// linear-space lerp (the working color is already linear-premultiplied).
    fn lerp(self, other: Self, t: f32) -> Self {
        self.add(other.sub(self).scale(t))
    }
}

impl Animatable for f32 {
    fn add(self, other: Self) -> Self {
        self + other
    }
    fn sub(self, other: Self) -> Self {
        self - other
    }
    fn scale(self, factor: f32) -> Self {
        self * factor
    }
    fn magnitude(self) -> f32 {
        self.abs()
    }
}

impl Animatable for Point {
    fn add(self, other: Self) -> Self {
        Point::new(self.x + other.x, self.y + other.y)
    }
    fn sub(self, other: Self) -> Self {
        Point::new(self.x - other.x, self.y - other.y)
    }
    fn scale(self, factor: f32) -> Self {
        Point::new(self.x * factor, self.y * factor)
    }
    fn magnitude(self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }
}

impl Animatable for Size {
    fn add(self, other: Self) -> Self {
        Size::new(self.w + other.w, self.h + other.h)
    }
    fn sub(self, other: Self) -> Self {
        Size::new(self.w - other.w, self.h - other.h)
    }
    fn scale(self, factor: f32) -> Self {
        Size::new(self.w * factor, self.h * factor)
    }
    fn magnitude(self) -> f32 {
        (self.w * self.w + self.h * self.h).sqrt()
    }
}

impl Animatable for Color {
    fn add(self, o: Self) -> Self {
        Color::new(self.r + o.r, self.g + o.g, self.b + o.b, self.a + o.a)
    }
    fn sub(self, o: Self) -> Self {
        Color::new(self.r - o.r, self.g - o.g, self.b - o.b, self.a - o.a)
    }
    fn scale(self, f: f32) -> Self {
        Color::new(self.r * f, self.g * f, self.b * f, self.a * f)
    }
    fn magnitude(self) -> f32 {
        (self.r * self.r + self.g * self.g + self.b * self.b + self.a * self.a).sqrt()
    }
}

/// An easing curve mapping normalized time `[0, 1]` to eased progress `[0, 1]`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[non_exhaustive]
pub enum Easing {
    /// Constant rate.
    #[default]
    Linear,
    /// Slow start (quadratic).
    EaseIn,
    /// Slow end (quadratic).
    EaseOut,
    /// Slow start and end (quadratic).
    EaseInOut,
}

impl Easing {
    /// Maps normalized time `t` (`[0, 1]`) to eased progress (`[0, 1]`).
    pub(crate) fn apply(self, t: f32) -> f32 {
        match self {
            Easing::Linear => t,
            Easing::EaseIn => t * t,
            Easing::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Easing::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    1.0 - 2.0 * (1.0 - t) * (1.0 - t)
                }
            }
        }
    }
}

/// How an [`Animated`] value advances toward its target.
#[derive(Clone, Copy, PartialEq, Debug)]
#[non_exhaustive]
pub enum AnimationSpec {
    /// A physical spring: `accel = -stiffness·(x − target) − damping·velocity`. Retargeting mid-flight
    /// preserves velocity, so it is interruptible without a jump.
    ///
    /// `stiffness` and `damping` must both be finite and `> 0` — otherwise the spring never reaches
    /// the convergence threshold and would pin the app to continuous redraw. A non-finite integration
    /// (a `NaN`/`Inf` parameter) is snapped to the target defensively, and a debug build asserts the
    /// precondition, but an undamped (`damping <= 0`) spring oscillates forever: pass a damped spec.
    Spring { stiffness: f32, damping: f32 },
    /// A fixed-duration eased tween. Retargeting restarts the tween from the current value.
    Easing { duration: Duration, curve: Easing },
}

/// The non-signal animation state (behind a `Mutex` since `tick`/`set_target` take `&self`).
struct Inner<T> {
    spec: AnimationSpec,
    target: T,
    /// Spring velocity in `T`-space (unused for easing).
    velocity: T,
    /// Easing start value (unused for spring).
    start: T,
    /// Easing elapsed time (unused for spring).
    elapsed: Duration,
    /// Whether a scheduler active source is currently registered.
    animating: bool,
    /// Whether a live tick entry for this animation is in the [`Ticker`] (#253). Distinct from
    /// `animating`: a `snap_to` clears `animating` but leaves the entry pending (the `Ticker` has no
    /// remove — it is pruned when `tick` next returns `false`). This flag stops a retarget in that window
    /// from registering a **duplicate** entry that would double-drive the value.
    ticker_registered: bool,
}

/// A spring/easing-driven, retargetable value, signal-backed for reactive reads (#59). Build with
/// [`new`](Self::new), aim with [`set_target`](Self::set_target), read (tracked) with
/// [`get`](Self::get), and advance each frame with [`tick`](Self::tick).
#[derive(Clone)]
pub struct Animated<T: Animatable + Send + Sync + 'static> {
    value: RwSignal<T>,
    inner: Arc<Mutex<Inner<T>>>,
    /// The scheduler active-source handle captured from context at build (`None` if unprovided).
    active: Option<ActiveSources>,
    /// The per-window ticker captured from context at build (#253); `None` if unprovided (no App / a
    /// test). On the rest→animating transition the animation registers itself so the app's redraw drives
    /// its [`tick`](Self::tick) each frame.
    ticker: Option<Ticker>,
}

impl<T: Animatable + Send + Sync + 'static> Animated<T> {
    /// Creates an animated value at `initial` with the given `spec`. Captures the scheduler
    /// [`ActiveSources`] from the current reactive context (the app root provides it); must be called
    /// under a reactive owner (the signal needs one — like any reactive value).
    pub fn new(initial: T, spec: AnimationSpec) -> Self {
        if let AnimationSpec::Spring { stiffness, damping } = spec {
            debug_assert!(
                stiffness.is_finite() && damping.is_finite() && stiffness > 0.0 && damping > 0.0,
                "a spring needs finite stiffness > 0 and damping > 0 to converge"
            );
        }
        Self {
            value: RwSignal::new(initial),
            inner: Arc::new(Mutex::new(Inner {
                spec,
                target: initial,
                velocity: initial.sub(initial), // zero in T-space
                start: initial,
                elapsed: Duration::ZERO,
                animating: false,
                ticker_registered: false,
            })),
            active: use_context::<ActiveSources>(),
            ticker: use_context::<Ticker>(),
        }
    }

    /// Registers this animation as running — a scheduler active source (continuous redraw) when it was at
    /// rest (`start_active`), plus a [`Ticker`] entry when one is not already live (`register_ticker`).
    /// Called after releasing the `inner` lock (#253).
    fn register_running(&self, start_active: bool, register_ticker: bool) {
        if start_active {
            if let Some(active) = &self.active {
                active.register();
            }
        }
        if register_ticker {
            if let Some(ticker) = &self.ticker {
                // Store a **ticker-less** clone: the entry only needs to `tick` (advance the shared
                // `inner`/value and release the active source), never to re-register. Keeping the `Ticker`
                // here would make the registry `Vec` strongly reference the very `Arc<Mutex<Vec>>` that
                // holds it — a self-cycle that leaks the whole registry (Vec + entries + inners) if the
                // window closes mid-animation.
                let mut running = self.clone();
                running.ticker = None;
                ticker.register(Box::new(running));
            }
        }
    }

    /// The current value — a **tracked** read (a reactive prop reading this re-runs when `tick`
    /// advances the animation).
    pub fn get(&self) -> T {
        self.value.get()
    }

    /// The current value **without** subscribing — for imperative reads (e.g. reading the momentary
    /// position) that must not create a reactive dependency.
    pub fn get_untracked(&self) -> T {
        self.value.get_untracked()
    }

    /// The current target the animation is heading toward — for retargeting by a relative delta
    /// (accumulate on the destination, not the momentary value, so deltas don't drop mid-flight).
    /// Falls back to the current value on a poisoned lock (never panics).
    pub fn target(&self) -> T {
        match self.inner.lock() {
            Ok(inner) => inner.target,
            Err(_) => self.value.get_untracked(),
        }
    }

    /// Retargets the animation. A spring keeps its velocity (interruptible, no jump); an easing tween
    /// restarts from the current value. Starting (from rest) registers a scheduler active source.
    pub fn set_target(&self, target: T) {
        let current = self.value.get_untracked();
        let (start_active, register_ticker) = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            inner.target = target;
            if let AnimationSpec::Easing { .. } = inner.spec {
                inner.start = current;
                inner.elapsed = Duration::ZERO;
            }
            let start_active = !inner.animating;
            inner.animating = true;
            let register_ticker = !inner.ticker_registered;
            inner.ticker_registered = true;
            (start_active, register_ticker)
        };
        self.register_running(start_active, register_ticker);
    }

    /// Seeds the spring with an initial `velocity` (a fling / throw) and aims at `target`, registering
    /// the active source. Unlike [`set_target`](Self::set_target), which preserves the running
    /// velocity, this *sets* the velocity — a release velocity carries the motion forward before it
    /// settles. A no-op for an easing spec (velocity is meaningless there; use `set_target`).
    pub fn fling(&self, velocity: T, target: T) {
        let (start_active, register_ticker) = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            if !matches!(inner.spec, AnimationSpec::Spring { .. }) {
                return;
            }
            inner.target = target;
            inner.velocity = velocity;
            let start_active = !inner.animating;
            inner.animating = true;
            let register_ticker = !inner.ticker_registered;
            inner.ticker_registered = true;
            (start_active, register_ticker)
        };
        self.register_running(start_active, register_ticker);
    }

    /// Snaps immediately to `value` without animating: stops a running spring/tween, clears velocity,
    /// deregisters the active source, and writes the value. For instant moves (e.g. restoring a saved
    /// position) where an animation would be wrong.
    pub fn snap_to(&self, value: T) {
        let was_animating = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            inner.target = value;
            inner.velocity = value.sub(value); // zero in T-space
            inner.start = value;
            inner.elapsed = Duration::ZERO;
            let was = inner.animating;
            inner.animating = false;
            was
        };
        // Write after releasing the lock so a subscribed effect can't re-enter and deadlock.
        self.value.set(value);
        if was_animating {
            if let Some(active) = &self.active {
                active.unregister();
            }
        }
    }

    /// Advances the animation by `dt`, writing the new value into the signal. Returns `false` once
    /// converged (the value is snapped to the target and the active source is deregistered).
    pub fn tick(&self, dt: Duration) -> bool {
        let (new_value, converged) = {
            let Ok(mut inner) = self.inner.lock() else {
                return false;
            };
            if !inner.animating {
                // A settled/snapped entry the `Ticker` is about to prune (this `tick` returns `false`):
                // clear the flag so a later retarget registers a fresh entry.
                inner.ticker_registered = false;
                return false;
            }
            let x = self.value.get_untracked();
            match inner.spec {
                AnimationSpec::Spring { stiffness, damping } => {
                    let (nx, nv) =
                        integrate_spring(x, inner.target, inner.velocity, stiffness, damping, dt);
                    // A non-finite integration (a NaN/Inf spring parameter) can never satisfy the
                    // convergence threshold; treat it as converged so the value snaps to the target
                    // and the active source is released rather than pinning continuous redraw forever.
                    let non_finite = !nx.magnitude().is_finite() || !nv.magnitude().is_finite();
                    let scale = inner.target.magnitude().max(1.0);
                    let done = non_finite
                        || (nx.sub(inner.target).magnitude() < REST_DELTA_REL * scale
                            && nv.magnitude() < REST_SPEED_REL * scale);
                    inner.velocity = if done {
                        inner.target.sub(inner.target)
                    } else {
                        nv
                    };
                    if done {
                        inner.animating = false;
                        // Converged: the `Ticker` will drop this entry (`tick` returns `false`).
                        inner.ticker_registered = false;
                    }
                    (if done { inner.target } else { nx }, done)
                }
                AnimationSpec::Easing { duration, curve } => {
                    inner.elapsed = inner.elapsed.saturating_add(dt);
                    let d = duration.as_secs_f32().max(f32::EPSILON);
                    let t = (inner.elapsed.as_secs_f32() / d).clamp(0.0, 1.0);
                    let done = inner.elapsed >= duration;
                    if done {
                        inner.animating = false;
                        // Converged: the `Ticker` will drop this entry (`tick` returns `false`).
                        inner.ticker_registered = false;
                    }
                    let value = if done {
                        inner.target
                    } else {
                        inner.start.lerp(inner.target, curve.apply(t))
                    };
                    (value, done)
                }
            }
        };
        // Write the value AFTER releasing the lock so a subscribed effect can't re-enter and deadlock.
        self.value.set(new_value);
        if converged {
            if let Some(active) = &self.active {
                active.unregister();
            }
        }
        !converged
    }
}

impl<T: Animatable + Send + Sync + 'static> Tickable for Animated<T> {
    fn tick(&self, dt: Duration) -> bool {
        Animated::tick(self, dt)
    }
}

/// Sub-stepped semi-implicit (symplectic) Euler for a damped spring: advances position `x` and
/// velocity `v` toward `target` by `dt`, splitting `dt` into `MAX_STEP` sub-steps for stability and
/// clamping a huge frame to `MAX_FRAME`.
fn integrate_spring<T: Animatable>(
    mut x: T,
    target: T,
    mut v: T,
    stiffness: f32,
    damping: f32,
    dt: Duration,
) -> (T, T) {
    let total = dt.as_secs_f32().min(MAX_FRAME);
    let steps = (total / MAX_STEP).ceil().max(1.0) as u32;
    let h = total / steps as f32;
    for _ in 0..steps {
        // accel = -stiffness*(x - target) - damping*v
        let accel = x.sub(target).scale(-stiffness).add(v.scale(-damping));
        v = v.add(accel.scale(h)); // symplectic: update velocity first,
        x = x.add(v.scale(h)); // then position with the new velocity.
    }
    (x, v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactive::provide_context;
    use reactive_graph::owner::Owner;

    /// A near-critically-damped spring (settles smoothly).
    fn spring() -> AnimationSpec {
        AnimationSpec::Spring {
            stiffness: 170.0,
            damping: 26.0,
        }
    }

    fn frame() -> Duration {
        Duration::from_secs_f32(1.0 / 60.0)
    }

    #[test]
    fn animated_value_should_converge_to_target() {
        // Synchronous RwSignal → hang-free; keep the owner alive (RK-003/005).
        let owner = Owner::new();
        owner.set();

        let anim = Animated::new(0.0_f32, spring());
        anim.set_target(100.0);
        let mut ticks = 0;
        while anim.tick(frame()) {
            ticks += 1;
            assert!(ticks < 10_000, "the spring must converge in bounded time");
        }
        assert_eq!(anim.get(), 100.0, "converges (and snaps) to the target");
        assert!(!anim.tick(frame()), "tick stays false once converged");

        drop(owner);
    }

    #[test]
    fn animated_should_converge_at_large_magnitude() {
        // Regression (#321): a px-scale spring (a scroll offset in the thousands) must still rest. A single
        // *absolute* rest epsilon is unreachable at large magnitudes — f32 quantization floors the position
        // a few ULPs short and leaves a limit-cycle velocity above the epsilon — so the spring would never
        // rest and would pin the app to continuous redraw. Scale-relative thresholds fix it: a spring to
        // 10000 converges (tick returns false), snaps to the exact target, and releases the active source.
        let owner = Owner::new();
        owner.set();

        let sources = ActiveSources::new();
        provide_context(sources.clone());
        let anim = Animated::new(0.0_f32, spring());
        anim.set_target(10_000.0);
        assert_eq!(sources.count(), 1, "set_target registers an active source");

        let mut ticks = 0;
        while anim.tick(frame()) {
            ticks += 1;
            assert!(
                ticks < 10_000,
                "a large-magnitude spring must converge in bounded time"
            );
        }
        assert_eq!(
            anim.get(),
            10_000.0,
            "converges and snaps to the exact large target"
        );
        assert_eq!(
            sources.count(),
            0,
            "convergence releases the active source (no idle redraw)"
        );

        drop(owner);
    }

    #[test]
    fn animated_spring_should_retarget_without_jump() {
        let owner = Owner::new();
        owner.set();

        let anim = Animated::new(0.0_f32, spring());
        anim.set_target(100.0);
        for _ in 0..10 {
            anim.tick(frame());
        }
        let before = anim.get();
        assert!(
            before > 0.0 && before < 100.0,
            "mid-flight value is between the start and the target ({before})"
        );

        // Retargeting must not jump the value — set_target only changes the target.
        anim.set_target(50.0);
        assert_eq!(anim.get(), before, "retargeting does not jump the value");

        let mut ticks = 0;
        while anim.tick(frame()) {
            ticks += 1;
            assert!(ticks < 10_000);
        }
        assert_eq!(anim.get(), 50.0, "converges to the retargeted value");

        drop(owner);
    }

    #[test]
    fn animatable_point_and_size_should_use_euclidean_norm() {
        // The multi-component `magnitude` (sqrt of the sum of squares) drives the convergence
        // threshold; `f32` alone never exercises it.
        assert_eq!(Point::new(3.0, 4.0).magnitude(), 5.0);
        assert_eq!(Size::new(3.0, 4.0).magnitude(), 5.0);
        assert_eq!(
            Animatable::lerp(Point::new(0.0, 0.0), Point::new(10.0, 20.0), 0.5),
            Point::new(5.0, 10.0),
            "the midpoint is the component-wise average"
        );
    }

    #[test]
    fn animated_point_should_converge_to_target() {
        let owner = Owner::new();
        owner.set();

        // Drive a multi-component type end-to-end through the spring/signal path.
        let anim = Animated::new(Point::new(0.0, 0.0), spring());
        anim.set_target(Point::new(100.0, -50.0));
        let mut ticks = 0;
        while anim.tick(frame()) {
            ticks += 1;
            assert!(ticks < 10_000, "the spring must converge in bounded time");
        }
        assert_eq!(
            anim.get(),
            Point::new(100.0, -50.0),
            "converges (and snaps) to the target"
        );

        drop(owner);
    }

    #[test]
    fn easing_curves_should_map_endpoints_and_midpoints() {
        for curve in [
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
        ] {
            assert_eq!(curve.apply(0.0), 0.0, "{curve:?} starts at 0");
            assert_eq!(curve.apply(1.0), 1.0, "{curve:?} ends at 1");
        }
        assert_eq!(Easing::Linear.apply(0.5), 0.5);
        assert_eq!(Easing::EaseIn.apply(0.5), 0.25, "quadratic ease-in");
        assert_eq!(Easing::EaseOut.apply(0.5), 0.75, "quadratic ease-out");
        assert_eq!(Easing::EaseInOut.apply(0.5), 0.5, "symmetric midpoint");
        assert_eq!(
            Easing::EaseInOut.apply(0.25) + Easing::EaseInOut.apply(0.75),
            1.0,
            "ease-in-out is symmetric about the midpoint"
        );
    }

    #[test]
    fn animated_spring_should_stay_bounded_under_a_huge_dt() {
        let owner = Owner::new();
        owner.set();

        // A stiff spring advanced by an enormous dt must not diverge/NaN: the frame is clamped to
        // MAX_FRAME and sub-stepped at MAX_STEP for stability (a raw Euler step would explode).
        let anim = Animated::new(
            0.0_f32,
            AnimationSpec::Spring {
                stiffness: 2000.0,
                damping: 20.0,
            },
        );
        anim.set_target(100.0);
        anim.tick(Duration::from_secs(5));
        let v = anim.get();
        assert!(
            v.is_finite(),
            "the spring stays finite under a huge dt ({v})"
        );
        assert!(
            v.abs() < 500.0,
            "the spring stays bounded (no divergence): {v}"
        );

        drop(owner);
    }

    #[test]
    fn animated_color_should_interpolate_in_linear_space() {
        // The working color is linear-premultiplied, so a component lerp IS a linear-space lerp.
        let black = Color::new(0.0, 0.0, 0.0, 1.0);
        let white = Color::new(1.0, 1.0, 1.0, 1.0);
        assert_eq!(
            Animatable::lerp(black, white, 0.5),
            Color::new(0.5, 0.5, 0.5, 1.0),
            "the midpoint is the linear average"
        );
    }

    #[test]
    fn animated_should_advance_via_context_ticker() {
        // The #253 path: with a `Ticker` in context, `set_target` registers the animation and the
        // ticker's `tick_all` (the app's frame loop) drives it to the target — no manual `tick()`.
        let owner = Owner::new();
        owner.set();

        let ticker = Ticker::new();
        provide_context(ticker.clone());
        let anim = Animated::new(0.0_f32, spring());
        anim.set_target(100.0);
        assert_eq!(
            anim.get_untracked(),
            0.0,
            "not advanced until the ticker runs"
        );

        ticker.tick_all(frame());
        assert!(
            anim.get_untracked() > 0.0,
            "the ticker advances the animation toward its target"
        );

        let mut ticks = 0;
        while (anim.get_untracked() - 100.0).abs() > 0.1 {
            ticker.tick_all(frame());
            ticks += 1;
            assert!(
                ticks < 10_000,
                "the context ticker drives it in bounded time"
            );
        }
        // Settled: the ticker dropped the converged animation, so further ticks leave it at the target.
        for _ in 0..5 {
            ticker.tick_all(frame());
        }
        assert!(
            (anim.get_untracked() - 100.0).abs() <= 0.1,
            "the ticker drove it to the target and leaves it there once settled: {}",
            anim.get_untracked()
        );

        drop(owner);
    }

    #[test]
    fn animated_ticker_drive_should_release_active_source_on_settle() {
        // The app wiring (#253): both a `Ticker` and an `ActiveSources` in context. Driving to settle
        // via the ticker must release the active source so the window returns to idle. Guards the
        // QA-bug class — the two must be provided *and* stay in sync (the ticker's prune ↔ the count).
        let owner = Owner::new();
        owner.set();

        let ticker = Ticker::new();
        let sources = ActiveSources::new();
        provide_context(ticker.clone());
        provide_context(sources.clone());

        let anim = Animated::new(0.0_f32, spring());
        anim.set_target(100.0);
        assert_eq!(sources.count(), 1, "set_target registers an active source");

        let mut ticks = 0;
        while sources.count() > 0 {
            ticker.tick_all(frame());
            ticks += 1;
            assert!(
                ticks < 10_000,
                "the ticker drives it to settle in bounded time"
            );
        }
        assert!(
            (anim.get_untracked() - 100.0).abs() <= 0.1,
            "reached the target before releasing"
        );
        // Idle: a further frame keeps the count at zero (no redraw would be requested).
        ticker.tick_all(frame());
        assert_eq!(sources.count(), 0, "stays released once settled");

        drop(owner);
    }

    #[test]
    fn animated_snap_then_retarget_should_not_double_register() {
        // Regression (#253): `snap_to` clears `animating` but leaves the ticker entry pending prune; a
        // retarget before the next `tick_all` must NOT register a *second* entry, or `tick_all` would
        // advance the shared value twice per frame (2× speed). Verify one frame advances by a single
        // spring step.

        // Reference: one spring step from 0 → 100 with no ticker (a plain manual tick).
        let reference = {
            let owner = Owner::new();
            owner.set();
            let a = Animated::new(0.0_f32, spring());
            a.set_target(100.0);
            a.tick(frame());
            let v = a.get_untracked();
            drop(owner);
            v
        };

        let owner = Owner::new();
        owner.set();
        let ticker = Ticker::new();
        provide_context(ticker.clone());

        let anim = Animated::new(0.0_f32, spring());
        anim.set_target(100.0); // registers ticker entry #1
        anim.snap_to(0.0); // clears `animating`; entry #1 still pending prune
        anim.set_target(100.0); // retarget before any tick_all — must NOT push a duplicate

        ticker.tick_all(frame()); // a single entry → exactly one spring step
        let advanced = anim.get_untracked();
        assert!(
            (advanced - reference).abs() < 1e-3,
            "snap+retarget advances by a single tick, not doubled: got {advanced}, single-step ref {reference}"
        );

        drop(owner);
    }

    #[test]
    fn animated_should_register_and_unregister_active_source() {
        let owner = Owner::new();
        owner.set();

        let sources = ActiveSources::new();
        provide_context(sources.clone()); // `Animated::new` captures this from context
        let anim = Animated::new(0.0_f32, spring());
        assert_eq!(sources.count(), 0, "idle before a target is set");

        anim.set_target(100.0);
        assert_eq!(sources.count(), 1, "set_target registers an active source");

        let mut ticks = 0;
        while anim.tick(frame()) {
            ticks += 1;
            assert!(ticks < 10_000);
        }
        assert_eq!(
            sources.count(),
            0,
            "convergence deregisters the active source"
        );

        drop(owner);
    }

    #[test]
    fn animated_easing_should_reach_target_at_duration() {
        let owner = Owner::new();
        owner.set();

        let anim = Animated::new(
            0.0_f32,
            AnimationSpec::Easing {
                duration: Duration::from_millis(100),
                curve: Easing::Linear,
            },
        );
        anim.set_target(10.0);
        let dt = Duration::from_millis(20);
        // 100ms / 20ms: four ticks still animating, the fifth reaches the duration → converged.
        for _ in 0..4 {
            assert!(anim.tick(dt), "still animating before the duration");
        }
        assert!(
            !anim.tick(dt),
            "converges when elapsed reaches the duration"
        );
        assert_eq!(anim.get(), 10.0, "reaches the target exactly");

        drop(owner);
    }

    #[test]
    fn animated_fling_should_carry_velocity_then_settle() {
        let owner = Owner::new();
        owner.set();

        // Fling from 0 toward a target ahead, seeded with a positive velocity: the value carries
        // forward (moves this frame) and settles exactly at the target on convergence.
        let anim = Animated::new(0.0_f32, spring());
        anim.fling(200.0, 50.0);
        assert!(anim.tick(frame()), "still animating after the first tick");
        assert!(
            anim.get() > 0.0,
            "the seeded velocity moves the value forward ({})",
            anim.get()
        );

        let mut ticks = 0;
        while anim.tick(frame()) {
            ticks += 1;
            assert!(ticks < 10_000);
        }
        assert_eq!(anim.get(), 50.0, "the fling settles at the target");

        drop(owner);
    }

    #[test]
    fn animated_snap_to_should_stop_without_animating() {
        let owner = Owner::new();
        owner.set();

        let sources = ActiveSources::new();
        provide_context(sources.clone());
        let anim = Animated::new(0.0_f32, spring());
        anim.set_target(100.0);
        assert_eq!(sources.count(), 1, "set_target starts an animation");

        // snap_to jumps to the value immediately and deregisters — no further ticks animate.
        anim.snap_to(42.0);
        assert_eq!(anim.get(), 42.0, "snap_to writes the value immediately");
        assert_eq!(sources.count(), 0, "snap_to deregisters the active source");
        assert!(!anim.tick(frame()), "snapped: no animation to advance");
        assert_eq!(anim.get(), 42.0, "the value stays put after snapping");

        drop(owner);
    }
}
