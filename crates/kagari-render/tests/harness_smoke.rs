//! Smoke test that exercises the golden harness end-to-end (#16): render a small
//! deterministic scene headlessly and compare it to a committed reference PNG. This
//! proves the readback + compare round-trip; the full Quad golden matrix is #17.

mod common;

use kagari_base::{Color, Corners, Edges, Rect};
use kagari_render::{Background, Border, Quad, RoundedRect, Scene};

/// A 50×50 scene with a single opaque quad whose bounds extend beyond the frame, so
/// every visible pixel is interior (no antialiased edge in view). This makes the
/// golden **rasterizer-portable**: a uniform fill encodes to the same bytes (within
/// the `≤ 2` tolerance) on DX12 WARP and Vulkan lavapipe, so the CI job (#123) is
/// green and the harness plumbing (render → readback → compare, including the row
/// de-pad) is what's under test. AA / rounded / border / gradient / clip correctness
/// is verified by the rasterizer-canonical goldens in #17.
///
/// 50px is deliberately not 256-byte aligned (`50*4 = 200`), so the readback
/// row-padding/de-pad path is actually exercised.
fn smoke_scene() -> Scene {
    let no_clip = RoundedRect {
        rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
        radii: Corners::default(),
    };
    let mut scene = Scene::new();
    scene.quads.push(Quad {
        // Larger than the 50×50 frame → no visible quad edge → no AA in view.
        bounds: Rect::from_xywh(-10.0, -10.0, 70.0, 70.0),
        corner_radii: Corners::default(),
        bg: Background::Solid(Color::from_srgb([0.20, 0.60, 0.90, 1.0])),
        border: Border {
            widths: Edges::default(),
            color: Color::TRANSPARENT,
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
