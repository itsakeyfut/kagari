//! Smoke test that exercises the golden harness end-to-end (#16): render a small
//! deterministic scene headlessly and compare it to a committed reference PNG. This
//! proves the readback + compare round-trip; the full Quad golden matrix is #17.

mod common;

use kagari_base::{Color, Corners, Edges, Rect};
use kagari_render::{Background, Border, Quad, RoundedRect, Scene};

/// A 50×50 scene with one rounded, uniformly-bordered solid quad — exercises the
/// SDF rounded rect + per-edge border + AA path on the dark offscreen clear color.
/// The 50px width is deliberately not 256-byte aligned (`50*4 = 200`), so the
/// readback row-padding/de-pad path is actually exercised.
fn smoke_scene() -> Scene {
    let no_clip = RoundedRect {
        rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
        radii: Corners::default(),
    };
    let mut scene = Scene::new();
    scene.quads.push(Quad {
        bounds: Rect::from_xywh(6.0, 6.0, 38.0, 38.0),
        corner_radii: Corners {
            tl: 10.0,
            tr: 10.0,
            br: 10.0,
            bl: 10.0,
        },
        bg: Background::Solid(Color::from_srgb([0.20, 0.60, 0.90, 1.0])),
        border: Border {
            widths: Edges {
                top: 4.0,
                right: 4.0,
                bottom: 4.0,
                left: 4.0,
            },
            color: Color::from_srgb([1.0, 1.0, 1.0, 1.0]),
        },
        content_mask: no_clip,
        order: 0,
    });
    scene
}

#[test]
fn headless_render_should_match_golden() {
    let mut scene = smoke_scene();
    let Some(img) = common::headless_render(&mut scene, (50, 50), 1.0) else {
        eprintln!(
            "skipping golden 'harness_smoke': no software adapter available (force_fallback_adapter)"
        );
        return;
    };
    assert_golden!("harness_smoke", img);
}
