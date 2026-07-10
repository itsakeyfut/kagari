//! IME preedit golden (#25): render an uncommitted preedit string inline with a solid
//! underline (whole) and a dotted underline (active segment), end to end.
//!
//! The reference is canonical to Mesa lavapipe (the CI `golden` job); on a
//! non-canonical adapter (local DX12 WARP) the pixel compare is skipped (render-only).
//! Regenerate on lavapipe via the `golden-update` CI job (`UPDATE_GOLDEN=1`). The
//! harness lives here because kagari-render cannot depend on kagari-text.

use kagari_base::{Color, Point, Px};
use kagari_render::Scene;
use kagari_text::{FontDb, ImeEvent, ImeState, TextStyle, TextSystem, fontdb};

#[test]
fn preedit_underline_should_match_golden() {
    let rendered = kagari_golden::headless_render_with((112, 48), 1.0, |renderer| {
        let mut text = TextSystem::new(FontDb::new());
        let style = TextStyle {
            family: "Noto Sans JP".into(),
            size: Px(32.0),
            weight: fontdb::Weight::NORMAL,
            line_height: None,
        };
        let mut ime = ImeState::new();
        // Composing "日本語" with the middle character "本" (bytes 3..6) active.
        ime.apply(ImeEvent::Preedit {
            text: "日本語".into(),
            cursor: Some((3, 6)),
        });

        let mut scene = Scene::new();
        ime.emit_preedit(
            &mut text,
            &style,
            Point::new(8.0, 6.0),
            Color::from_srgb([1.0, 1.0, 1.0, 1.0]),
            renderer.atlas_mut(),
            &mut scene,
            0,
            None,
        );
        // Verified independently of lavapipe: glyphs plus a solid + a dotted underline.
        assert_eq!(scene.glyphs.len(), 3, "日 本 語 → three glyphs");
        assert_eq!(
            scene.underlines.len(),
            2,
            "solid (whole) + dotted (active segment)"
        );
        scene
    });

    let Some(r) = rendered else {
        eprintln!("skipping golden 'preedit_underline': no software adapter available");
        return;
    };
    if !r.canonical {
        eprintln!(
            "skipping golden compare 'preedit_underline': non-canonical rasterizer (goldens are lavapipe-canonical)"
        );
        return;
    }
    kagari_golden::assert_golden!("preedit_underline", r.image);
}
