//! Property-based tests for kagari-base operations (panic-freedom and round-trips).
//! Known-value unit tests live inline in each module.

use kagari_base::{Color, Edges, Point, Rect};
use proptest::prelude::*;

/// Tolerance for properties that accumulate a little floating-point rounding.
const EPS: f32 = 0.1;

/// Arbitrary `f32`, explicitly including NaN and the infinities.
fn any_f32() -> impl Strategy<Value = f32> {
    prop_oneof![
        any::<f32>(),
        Just(f32::NAN),
        Just(f32::INFINITY),
        Just(f32::NEG_INFINITY),
    ]
}

prop_compose! {
    fn arb_rect()(
        x in -1.0e4f32..1.0e4,
        y in -1.0e4f32..1.0e4,
        w in 0.0f32..1.0e4,
        h in 0.0f32..1.0e4,
    ) -> Rect {
        Rect::from_xywh(x, y, w, h)
    }
}

prop_compose! {
    fn arb_nonempty_rect()(
        x in -1.0e4f32..1.0e4,
        y in -1.0e4f32..1.0e4,
        w in 0.1f32..1.0e4,
        h in 0.1f32..1.0e4,
    ) -> Rect {
        Rect::from_xywh(x, y, w, h)
    }
}

/// Standard sRGB OETF (linear → encoded); the inverse of the decode under test.
fn srgb_encode(c: f32) -> f32 {
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

proptest! {
    #[test]
    fn rect_ops_should_never_panic(
        ax in any_f32(), ay in any_f32(), aw in any_f32(), ah in any_f32(),
        bx in any_f32(), by in any_f32(), bw in any_f32(), bh in any_f32(),
        px in any_f32(), py in any_f32(),
    ) {
        let a = Rect::from_xywh(ax, ay, aw, ah);
        let b = Rect::from_xywh(bx, by, bw, bh);
        let _ = a.contains(Point::new(px, py));
        let _ = a.intersect(b);
        let _ = a.union(b);
        let _ = a.inset(Edges { top: ay, right: bw, bottom: bh, left: ax });
    }

    #[test]
    fn union_should_bound_both(a in arb_nonempty_rect(), b in arb_nonempty_rect()) {
        // Half-open `contains` excludes the far edge, so assert coordinate extents
        // rather than `contains(corner)`.
        let u = a.union(b);
        prop_assert!(u.origin.x <= a.origin.x && u.origin.x <= b.origin.x);
        prop_assert!(u.origin.y <= a.origin.y && u.origin.y <= b.origin.y);
        prop_assert!(u.origin.x + u.width() >= a.origin.x + a.width() - EPS);
        prop_assert!(u.origin.x + u.width() >= b.origin.x + b.width() - EPS);
        prop_assert!(u.origin.y + u.height() >= a.origin.y + a.height() - EPS);
        prop_assert!(u.origin.y + u.height() >= b.origin.y + b.height() - EPS);
    }

    #[test]
    fn intersect_should_be_subset(a in arb_rect(), b in arb_rect()) {
        if let Some(r) = a.intersect(b) {
            prop_assert!(r.origin.x >= a.origin.x && r.origin.x >= b.origin.x);
            prop_assert!(r.origin.y >= a.origin.y && r.origin.y >= b.origin.y);
            prop_assert!(r.origin.x + r.width() <= a.origin.x + a.width() + EPS);
            prop_assert!(r.origin.x + r.width() <= b.origin.x + b.width() + EPS);
            prop_assert!(r.origin.y + r.height() <= a.origin.y + a.height() + EPS);
            prop_assert!(r.origin.y + r.height() <= b.origin.y + b.height() + EPS);
        }
    }

    #[test]
    fn color_from_srgb_should_round_trip(v in 0.0f32..=1.0) {
        // alpha = 1 so premultiply is a no-op; `.r` is the decoded linear value.
        let c = Color::from_srgb([v, v, v, 1.0]);
        prop_assert!((srgb_encode(c.r) - v).abs() < 1e-4);
    }

    #[test]
    fn premultiply_should_round_trip(
        r in 0.0f32..=1.0, g in 0.0f32..=1.0, b in 0.0f32..=1.0, a in 0.001f32..=1.0,
    ) {
        let round = Color::new(r, g, b, a).premultiply().unpremultiply();
        prop_assert!((round.r - r).abs() < 1e-4);
        prop_assert!((round.g - g).abs() < 1e-4);
        prop_assert!((round.b - b).abs() < 1e-4);
        prop_assert!((round.a - a).abs() < 1e-6);
    }
}
