//! Pixel goldens for the kagari-core **paint pass** (#144, follows up #34): build an element tree,
//! run `render_tree` (build → layout → paint) into a `Scene`, render headlessly, and compare to a
//! committed reference. Unlike the GPU-free Scene-level tests in `paint.rs`, this verifies that a
//! declared `div`/`text` actually rasterizes to pixels — including the glyph-origin offset in
//! `Text::paint` (RK-007), which the `atlas: None` Scene tests cannot exercise.
//!
//! References are canonical to Mesa lavapipe (the CI `golden` job); on a non-canonical adapter (e.g.
//! local DX12 WARP) the shared harness renders-only and skips the comparison, so these never
//! false-fail locally. References are produced/regenerated on lavapipe via the `golden-update` CI job.
//!
//! Note: unlike the kagari-text goldens (which the gating `golden` job does NOT run), these run in
//! the gating job, so their references must be committed (via `golden-update`) before merge —
//! otherwise the gating `golden` job goes RED (a missing reference panics on lavapipe, RK-014).

use std::sync::Arc;

use kagari_base::{Color, Size};
use kagari_core::arena::Arena;
use kagari_core::damage::DamageState;
use kagari_core::element::IntoElement;
use kagari_core::paint::render_tree;
use kagari_core::{div, text};
use kagari_layout::LayoutTree;
use kagari_render::{Background, Scene};
use kagari_style::{ColorRole, Styled, Theme};
use kagari_text::{FontDb, TextSystem};

const SIZE: (u32, u32) = (96, 48);

/// Builds `root` against `theme` through the #34 paint pass into a `Scene`, seeding the renderer's
/// glyph atlas (text leaves rasterize into it). The viewport matches the headless target at scale 1.
fn paint_scene(
    renderer: &mut kagari_render::Renderer,
    mut root: kagari_core::element::AnyElement,
    theme: &Theme,
) -> Scene {
    let mut arena = Arena::new();
    let mut layout = LayoutTree::new();
    let mut text_system = TextSystem::new(FontDb::new());
    let mut scene = Scene::new();
    let damage = Arc::new(DamageState::default());
    let viewport = Size {
        w: SIZE.0 as f32,
        h: SIZE.1 as f32,
    };
    render_tree(
        &mut root,
        &mut arena,
        &mut layout,
        &mut text_system,
        Some(renderer.atlas_mut()),
        &mut scene,
        viewport,
        &damage,
        theme,
    )
    .expect("paint pass should lay out and paint");
    scene
}

#[test]
fn core_div_text_should_match_golden() {
    // A solid panel with a line of text: the basic "a declared div/text appears" pixel guard,
    // exercising the div quad + glyph rasterization at the laid-out bounds (RK-007).
    let theme = Theme::default();
    let panel = Background::Solid(Color::from_srgb([0.20, 0.40, 0.70, 1.0]));
    kagari_golden::assert_golden_with(
        env!("CARGO_MANIFEST_DIR"),
        "core_div_text",
        SIZE,
        1.0,
        |r| {
            let root = div()
                .size(Size { w: 80.0, h: 32.0 })
                .background(panel)
                .child(text("Hi"))
                .into_element();
            paint_scene(r, root, &theme)
        },
    );
}

#[test]
fn core_styled_panel_should_match_golden() {
    // Styling through the theme (#42/#45): padding (p_4), corner radius (rounded_lg), and the
    // semantic background (bg(Surface)) all resolve against the built-in light theme and reach pixels.
    let theme = Theme::light();
    kagari_golden::assert_golden_with(
        env!("CARGO_MANIFEST_DIR"),
        "core_styled_panel",
        SIZE,
        1.0,
        |r| {
            let root = div()
                .p_4()
                .rounded_lg()
                .bg(ColorRole::Surface)
                .child(text("Hi"))
                .into_element();
            paint_scene(r, root, &theme)
        },
    );
}
