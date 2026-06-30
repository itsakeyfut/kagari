//! PolychromeSprite golden (#55): insert a synthetic RGBA tile into the image atlas and
//! render one polychrome sprite that samples it with a white (identity) tint, proving the
//! polychrome pipeline (RGBA atlas array sampling × sRGB→linear decode × premultiply ×
//! tint × content-mask) end to end.
//!
//! Reference is canonical to Mesa lavapipe (the CI `golden` job); on a non-canonical
//! adapter (local DX12 WARP) the pixel compare is skipped (render-only). Regenerate on
//! lavapipe via the `golden-update` CI job (`UPDATE_GOLDEN=1`).

use kagari_base::{Color, Corners, Rect};
use kagari_render::{PolychromeSprite, RoundedRect, Scene};

#[test]
fn polychrome_sprite_should_match_golden() {
    let rendered = kagari_golden::headless_render_with((64, 64), 1.0, |renderer| {
        // A 2×2 RGBA tile: red, green / blue, white (sRGB bytes; the Srgb atlas format
        // decodes to linear on sample). Drawn over a 40×40 quad, it renders as a smooth
        // four-color blend — a regression in channel order / premultiply / sRGB shifts it.
        let (tw, th) = (2u32, 2u32);
        let coord = renderer.rgba_atlas_mut().get_or_insert(1, (tw, th), || {
            #[rustfmt::skip]
            let px: [u8; 16] = [
                255, 0, 0, 255,   0, 255, 0, 255, // red,   green
                0, 0, 255, 255,   255, 255, 255, 255, // blue, white
            ];
            px.to_vec()
        });

        let no_clip = RoundedRect {
            rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
            radii: Corners::default(),
        };
        let mut scene = Scene::new();
        scene.images.push(PolychromeSprite {
            bounds: Rect::from_xywh(12.0, 12.0, 40.0, 40.0),
            tex: coord,
            // Premultiplied-linear white = draw the image unmodified.
            tint: Color::new(1.0, 1.0, 1.0, 1.0),
            content_mask: no_clip,
            order: 0,
        });
        scene
    });

    let Some(r) = rendered else {
        eprintln!("skipping golden 'polychrome_sprite': no software adapter available");
        return;
    };
    if !r.canonical {
        eprintln!(
            "skipping golden compare 'polychrome_sprite': non-canonical rasterizer (goldens are lavapipe-canonical)"
        );
        return;
    }
    kagari_golden::assert_golden!("polychrome_sprite", r.image);
}

#[test]
fn tinted_polychrome_sprite_should_match_golden() {
    // The same tile under a red tint (premultiplied linear (1,0,0,1)): the per-channel
    // multiply keeps the red channel and zeroes green/blue, so the result differs from the
    // untinted image. Proves the shader's `premult * tint * mask` tint contribution (the
    // plain golden uses WHITE = identity, which would pass even if `tint` were dropped).
    let rendered = kagari_golden::headless_render_with((64, 64), 1.0, |renderer| {
        let (tw, th) = (2u32, 2u32);
        let coord = renderer.rgba_atlas_mut().get_or_insert(1, (tw, th), || {
            #[rustfmt::skip]
            let px: [u8; 16] = [
                255, 0, 0, 255,   0, 255, 0, 255, // red,   green
                0, 0, 255, 255,   255, 255, 255, 255, // blue, white
            ];
            px.to_vec()
        });

        let no_clip = RoundedRect {
            rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
            radii: Corners::default(),
        };
        let mut scene = Scene::new();
        scene.images.push(PolychromeSprite {
            bounds: Rect::from_xywh(12.0, 12.0, 40.0, 40.0),
            tex: coord,
            tint: Color::new(1.0, 0.0, 0.0, 1.0), // red multiply: keeps R, kills G/B
            content_mask: no_clip,
            order: 0,
        });
        scene
    });

    let Some(r) = rendered else {
        eprintln!("skipping golden 'tinted_polychrome_sprite': no software adapter available");
        return;
    };
    if !r.canonical {
        eprintln!(
            "skipping golden compare 'tinted_polychrome_sprite': non-canonical rasterizer (goldens are lavapipe-canonical)"
        );
        return;
    }
    kagari_golden::assert_golden!("tinted_polychrome_sprite", r.image);
}
