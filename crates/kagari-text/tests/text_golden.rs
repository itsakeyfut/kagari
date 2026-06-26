//! Text-rendering goldens (#22): shape + swash-rasterize bundled-font text into the
//! mono atlas and render the emitted `MonochromeSprite`s end to end.
//!
//! The reference is canonical to Mesa lavapipe (the CI `golden` job); on a
//! non-canonical adapter (local DX12 WARP) the pixel compare is skipped
//! (render-only). Regenerate on lavapipe via the `golden-update` CI job
//! (`UPDATE_GOLDEN=1`). The harness lives here because kagari-render cannot depend
//! on kagari-text (render→text dependency direction).

mod common;

use kagari_base::{Color, Px};
use kagari_render::Scene;
use kagari_text::{FontDb, TextStyle, TextSystem, fontdb};

fn style(family: &'static str, size: f32) -> TextStyle {
    TextStyle {
        family: family.into(),
        size: Px(size),
        weight: fontdb::Weight::NORMAL,
        line_height: None,
    }
}

#[test]
fn japanese_text_should_match_golden() {
    let rendered = common::headless_render_with((112, 48), 1.0, |renderer| {
        let mut text = TextSystem::new(FontDb::new());
        let shaped = text.shape("日本語", &style("Noto Sans JP", 32.0), None);
        let mut scene = Scene::new();
        text.rasterize_into(
            &shaped,
            // White text on the dark clear color so coverage is clearly visible.
            Color::from_srgb([1.0, 1.0, 1.0, 1.0]),
            renderer.atlas_mut(),
            &mut scene.glyphs,
        );
        // Verified independently of lavapipe: the three kanji each rasterize to a
        // non-degenerate sprite (the pixel golden additionally runs on CI).
        assert_eq!(scene.glyphs.len(), 3, "日 本 語 → three visible glyphs");
        for sprite in &scene.glyphs {
            assert!(
                sprite.bounds.size.w > 0.0 && sprite.bounds.size.h > 0.0,
                "each glyph sprite has non-degenerate bounds"
            );
        }
        scene
    });

    let Some(r) = rendered else {
        eprintln!("skipping golden 'jp_text': no software adapter available");
        return;
    };
    if !r.canonical {
        eprintln!(
            "skipping golden compare 'jp_text': non-canonical rasterizer (goldens are lavapipe-canonical)"
        );
        return;
    }
    assert_golden!("jp_text", r.image);
}

#[test]
fn rasterize_into_should_reuse_coord_for_repeated_glyph() {
    // Adapter-gated: the assertions run inside `build`, which only executes when a
    // software adapter is available (otherwise `headless_render_with` returns None).
    let rendered = common::headless_render_with((8, 8), 1.0, |renderer| {
        let mut text = TextSystem::new(FontDb::new());
        let shaped = text.shape("AA", &style("Noto Sans", 16.0), None);
        let mut scene = Scene::new();
        text.rasterize_into(
            &shaped,
            Color::from_srgb([1.0, 1.0, 1.0, 1.0]),
            renderer.atlas_mut(),
            &mut scene.glyphs,
        );
        assert_eq!(scene.glyphs.len(), 2, "two visible 'A' glyphs");
        assert_eq!(
            scene.glyphs[0].tex, scene.glyphs[1].tex,
            "a repeated glyph must reuse the cached atlas coord"
        );
        scene
    });
    if rendered.is_none() {
        eprintln!(
            "skipping 'rasterize_into_should_reuse_coord_for_repeated_glyph': no software adapter"
        );
    }
}
