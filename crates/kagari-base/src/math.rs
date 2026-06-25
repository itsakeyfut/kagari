//! CPU-side scalar math helpers (`f32`).

/// Linear interpolation from `a` to `b` by `t` (not clamped).
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Clamp `x` into `[lo, hi]`. Requires `lo <= hi` (panics otherwise, like
/// [`f32::clamp`]).
pub fn clamp(x: f32, lo: f32, hi: f32) -> f32 {
    x.clamp(lo, hi)
}

/// The fraction of the way `v` lies from `a` to `b` (inverse of [`lerp`]);
/// returns 0 when `a == b`.
pub fn inverse_lerp(a: f32, b: f32, v: f32) -> f32 {
    if a == b { 0.0 } else { (v - a) / (b - a) }
}

/// Hermite smoothstep: 0 at/below `edge0`, 1 at/above `edge1`, smooth in between.
/// Degenerate `edge0 == edge1` behaves as a step at that edge.
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge0 == edge1 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_should_interpolate_endpoints_and_midpoint() {
        assert_eq!(lerp(0.0, 10.0, 0.0), 0.0);
        assert_eq!(lerp(0.0, 10.0, 1.0), 10.0);
        assert_eq!(lerp(0.0, 10.0, 0.5), 5.0);
    }

    #[test]
    fn clamp_should_bound_to_range() {
        assert_eq!(clamp(5.0, 0.0, 3.0), 3.0);
        assert_eq!(clamp(-1.0, 0.0, 3.0), 0.0);
        assert_eq!(clamp(2.0, 0.0, 3.0), 2.0);
    }

    #[test]
    fn inverse_lerp_should_invert_lerp() {
        assert_eq!(inverse_lerp(0.0, 10.0, 5.0), 0.5);
        assert_eq!(inverse_lerp(2.0, 2.0, 2.0), 0.0);
    }

    #[test]
    fn smoothstep_should_clamp_outside_and_ease_inside() {
        assert_eq!(smoothstep(0.0, 1.0, -0.5), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 1.5), 1.0);
        assert_eq!(smoothstep(0.0, 1.0, 0.0), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 1.0), 1.0);
        assert_eq!(smoothstep(0.0, 1.0, 0.5), 0.5);
    }

    #[test]
    fn smoothstep_should_step_for_equal_edges() {
        assert_eq!(smoothstep(1.0, 1.0, 0.5), 0.0);
        assert_eq!(smoothstep(1.0, 1.0, 1.0), 1.0);
        assert_eq!(smoothstep(1.0, 1.0, 2.0), 1.0);
    }
}
