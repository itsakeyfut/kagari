//! Underline golden tests (#23): one solid and one dotted band, proving the
//! underline pipeline (SDF band AA × style threshold × content-mask clip) end to end.
//!
//! References are canonical to Mesa lavapipe (the CI `golden` job); on a non-canonical
//! adapter (e.g. local DX12 WARP) `assert_scene_golden` renders-only and skips the
//! comparison. Regenerate references on lavapipe via the `golden-update` CI job
//! (`UPDATE_GOLDEN=1`).

mod common;

use kagari_base::{Color, Corners, Rect};
use kagari_render::{RoundedRect, Scene, Underline, UnderlineStyle};

const SIZE: (u32, u32) = (64, 24);

fn no_clip() -> RoundedRect {
    RoundedRect {
        rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
        radii: Corners::default(),
    }
}

fn one_underline(style: UnderlineStyle) -> Scene {
    let mut scene = Scene::new();
    scene.underlines.push(Underline {
        rect: Rect::from_xywh(8.0, 11.0, 48.0, 3.0),
        color: Color::from_srgb([0.20, 0.70, 0.90, 1.0]),
        style,
        thickness: 3.0,
        content_mask: no_clip(),
        order: 0,
    });
    scene
}

#[test]
fn solid_underline_should_match_golden() {
    let mut scene = one_underline(UnderlineStyle::Solid);
    common::assert_scene_golden("solid_underline", &mut scene, SIZE, 1.0);
}

#[test]
fn dotted_underline_should_match_golden() {
    let mut scene = one_underline(UnderlineStyle::Dotted);
    common::assert_scene_golden("dotted_underline", &mut scene, SIZE, 1.0);
}
