//! Quad golden tests (#17): one per visual feature of the Quad shader, turning the
//! manual verification of #13/#14/#15 into automated regression. Each builds a small
//! `Scene` and compares the headless render to a committed reference.
//!
//! References are canonical to Mesa lavapipe (the CI `golden` job); on a non-canonical
//! adapter (e.g. local DX12 WARP) `assert_scene_golden` renders-only and skips the
//! comparison, so these never false-fail locally. Regenerate references on lavapipe via
//! the `golden-update` CI job (`UPDATE_GOLDEN=1`).

mod common;

use kagari_base::{Color, Corners, Edges, Point, Rect};
use kagari_render::{Background, Border, Quad, RoundedRect, Scene};

const SIZE: (u32, u32) = (64, 64);

/// A no-op clip far larger than any quad (the `harness_smoke` pattern).
fn no_clip() -> RoundedRect {
    RoundedRect {
        rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
        radii: Corners::default(),
    }
}

/// A single-quad scene with the common defaults; callers tweak the returned quad.
fn one_quad(quad: Quad) -> Scene {
    let mut scene = Scene::new();
    scene.quads.push(quad);
    scene
}

fn base_quad() -> Quad {
    Quad {
        bounds: Rect::from_xywh(8.0, 8.0, 48.0, 48.0),
        corner_radii: Corners::default(),
        bg: Background::Solid(Color::from_srgb([0.20, 0.55, 0.85, 1.0])),
        border: Border {
            widths: Edges::default(),
            color: Color::TRANSPARENT,
        },
        content_mask: no_clip(),
        order: 0,
    }
}

#[test]
fn rounded_quad_should_match_golden() {
    // Asymmetric per-corner radii.
    let mut scene = one_quad(Quad {
        corner_radii: Corners {
            tl: 4.0,
            tr: 12.0,
            br: 20.0,
            bl: 8.0,
        },
        ..base_quad()
    });
    common::assert_scene_golden("rounded_quad", &mut scene, SIZE, 1.0);
}

#[test]
fn per_edge_border_should_match_golden() {
    // Different border width per edge (top/right/bottom/left) + a contrasting color.
    let mut scene = one_quad(Quad {
        corner_radii: Corners {
            tl: 10.0,
            tr: 10.0,
            br: 10.0,
            bl: 10.0,
        },
        border: Border {
            widths: Edges {
                top: 8.0,
                right: 2.0,
                bottom: 12.0,
                left: 4.0,
            },
            color: Color::from_srgb([0.95, 0.85, 0.20, 1.0]),
        },
        ..base_quad()
    });
    common::assert_scene_golden("per_edge_border", &mut scene, SIZE, 1.0);
}

#[test]
fn linear_gradient_should_match_golden() {
    // 2-stop diagonal linear gradient interpolated in linear premultiplied space.
    let mut scene = one_quad(Quad {
        corner_radii: Corners {
            tl: 8.0,
            tr: 8.0,
            br: 8.0,
            bl: 8.0,
        },
        bg: Background::LinearGradient {
            start: Color::from_srgb([0.10, 0.20, 0.80, 1.0]),
            end: Color::from_srgb([0.90, 0.20, 0.50, 1.0]),
            start_point: Point::new(0.0, 0.0),
            end_point: Point::new(1.0, 1.0),
        },
        ..base_quad()
    });
    common::assert_scene_golden("linear_gradient", &mut scene, SIZE, 1.0);
}

#[test]
fn aa_edge_should_match_golden() {
    // A near-circle (corner radius = half the 48px extent) so the whole curved edge
    // exercises the analytic ~1px AA. (The issue says "rotated", but M1 has no
    // transform yet; a curved edge is the faithful AA demonstrator.)
    let mut scene = one_quad(Quad {
        corner_radii: Corners {
            tl: 24.0,
            tr: 24.0,
            br: 24.0,
            bl: 24.0,
        },
        ..base_quad()
    });
    common::assert_scene_golden("aa_edge", &mut scene, SIZE, 1.0);
}

#[test]
fn rounded_clip_should_match_golden() {
    // A square solid quad clipped by a smaller rounded content-mask: only the part
    // inside the rounded mask is visible, with an anti-aliased clip edge.
    let mut scene = one_quad(Quad {
        bounds: Rect::from_xywh(6.0, 6.0, 52.0, 52.0),
        bg: Background::Solid(Color::from_srgb([0.95, 0.65, 0.10, 1.0])),
        content_mask: RoundedRect {
            rect: Rect::from_xywh(16.0, 16.0, 32.0, 32.0),
            radii: Corners {
                tl: 14.0,
                tr: 14.0,
                br: 14.0,
                bl: 14.0,
            },
        },
        ..base_quad()
    });
    common::assert_scene_golden("rounded_clip", &mut scene, SIZE, 1.0);
}
