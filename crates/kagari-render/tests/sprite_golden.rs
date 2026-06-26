//! MonochromeSprite golden (#19): insert a synthetic R8 coverage tile into the atlas
//! and render one colored sprite that samples it, proving the sprite pipeline
//! (atlas array sampling × color × content-mask) end to end.
//!
//! Reference is canonical to Mesa lavapipe (the CI `golden` job); on a non-canonical
//! adapter (local DX12 WARP) the pixel compare is skipped (render-only). Regenerate on
//! lavapipe via the `golden-update` CI job (`UPDATE_GOLDEN=1`).

mod common;

use kagari_base::{Color, Corners, Rect};
use kagari_render::{MonochromeSprite, RoundedRect, Scene};

#[test]
fn mono_sprite_should_match_golden() {
    let rendered = common::headless_render_with((64, 64), 1.0, |renderer| {
        // A 32×16 horizontal coverage ramp (0..255 left→right).
        let (tw, th) = (32u32, 16u32);
        let coord = renderer.atlas_mut().get_or_insert(1, (tw, th), || {
            let mut buf = vec![0u8; (tw * th) as usize];
            for y in 0..th {
                for x in 0..tw {
                    buf[(y * tw + x) as usize] = ((x * 255) / (tw - 1)) as u8;
                }
            }
            buf
        });

        let no_clip = RoundedRect {
            rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
            radii: Corners::default(),
        };
        let mut scene = Scene::new();
        scene.glyphs.push(MonochromeSprite {
            bounds: Rect::from_xywh(12.0, 24.0, 40.0, 16.0),
            tex: coord,
            color: Color::from_srgb([0.20, 0.70, 0.90, 1.0]),
            content_mask: no_clip,
            order: 0,
        });
        scene
    });

    let Some(r) = rendered else {
        eprintln!("skipping golden 'mono_sprite': no software adapter available");
        return;
    };
    if !r.canonical {
        eprintln!(
            "skipping golden compare 'mono_sprite': non-canonical rasterizer (goldens are lavapipe-canonical)"
        );
        return;
    }
    assert_golden!("mono_sprite", r.image);
}
