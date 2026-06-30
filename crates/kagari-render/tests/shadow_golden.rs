//! Shadow golden (#155): the analytic erf-Gaussian blurred rounded-rect drop shadow rendered end to
//! end, proving the shadow pipeline (blur falloff × rounded corners × premultiplied color).
//!
//! Reference is canonical to Mesa lavapipe (the CI `golden` job); on a non-canonical adapter (e.g.
//! local DX12 WARP) `assert_scene_golden` renders-only and skips the comparison. Regenerate the
//! reference on lavapipe via the `golden-update` CI job (`UPDATE_GOLDEN=1`); it is committed
//! separately (RK-014 — the gating `golden` job reds until it lands).

use kagari_base::{Color, Corners, Point, Rect};
use kagari_render::{RoundedRect, Scene, Shadow};

const SIZE: (u32, u32) = (64, 64);

fn no_clip() -> RoundedRect {
    RoundedRect {
        rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
        radii: Corners::default(),
    }
}

#[test]
fn drop_shadow_should_match_golden() {
    // A soft drop shadow offset down from a small rounded box, centered so the blur stays inside the
    // 64² target. The shader is color-agnostic, so a **light** premultiplied color is used here (not
    // Tailwind's black) for high contrast against the dark offscreen clear — the Gaussian falloff is
    // then strongly captured, making the golden a discriminating regression guard. (The black
    // Tailwind shadow values are exercised by the style/core tests.) Straight (1,1,1,0.6) →
    // premultiplied (0.6,0.6,0.6,0.6).
    let mut scene = Scene::new();
    scene.shadows.push(Shadow {
        bounds: Rect::from_xywh(18.0, 16.0, 28.0, 28.0),
        corner_radii: Corners {
            tl: 6.0,
            tr: 6.0,
            br: 6.0,
            bl: 6.0,
        },
        offset: Point::new(0.0, 4.0),
        blur: 8.0,
        spread: 0.0,
        color: Color::new(0.6, 0.6, 0.6, 0.6),
        content_mask: no_clip(),
        order: 0,
    });
    kagari_golden::assert_scene_golden(
        env!("CARGO_MANIFEST_DIR"),
        "drop_shadow",
        &mut scene,
        SIZE,
        1.0,
    );
}
