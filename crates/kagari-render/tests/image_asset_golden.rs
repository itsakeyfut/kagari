//! Image asset loader golden (#56): decode an in-memory PNG through the `AssetLoader` (inline spawner),
//! drain it into the RGBA atlas, and render the resulting tile as a `PolychromeSprite` — proving the
//! decode → atlas → render path end to end. Uses an inline spawner so the decode is synchronous (no
//! thread-timing dependence); the only difference from production is the injected closure.
//!
//! Reference is canonical to Mesa lavapipe (the CI `golden` job); on a non-canonical adapter (local
//! DX12 WARP) the pixel compare is skipped. Regenerate on lavapipe via the `golden-update` CI job.

use std::sync::Arc;

use kagari_base::{Color, Corners, Rect};
use kagari_render::{AssetLoader, AssetSource, AtlasCoord, PolychromeSprite, RoundedRect, Scene};

/// Encode a 2×2 red/green/blue/white RGBA image to PNG bytes in memory (no committed fixture).
fn sample_png() -> Vec<u8> {
    #[rustfmt::skip]
    let px: [u8; 16] = [
        255, 0, 0, 255,   0, 255, 0, 255, // red,  green
        0, 0, 255, 255,   255, 255, 255, 255, // blue, white
    ];
    let buf = image::RgbaImage::from_raw(2, 2, px.to_vec()).expect("2x2 RGBA buffer");
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(buf)
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("encode test PNG");
    out.into_inner()
}

#[test]
fn image_asset_should_render() {
    let png = sample_png();

    let rendered = kagari_golden::headless_render_with((64, 64), 1.0, |renderer| {
        // Inline spawner → the decode runs synchronously inside `load`, so a single `drain` uploads it.
        // The placeholder coord is unused here (the image resolves Ready).
        let placeholder = AtlasCoord {
            page: 0,
            min: [0.0, 0.0],
            max: [0.0, 0.0],
        };
        let mut loader = AssetLoader::new(|job| job(), placeholder);
        let id = loader.load(AssetSource::Bytes(Arc::from(
            png.clone().into_boxed_slice(),
        )));
        loader.drain(renderer.rgba_atlas_mut());
        let coord = loader.resolve(id);

        let no_clip = RoundedRect {
            rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
            radii: Corners::default(),
        };
        let mut scene = Scene::new();
        scene.images.push(PolychromeSprite {
            bounds: Rect::from_xywh(12.0, 12.0, 40.0, 40.0),
            tex: coord,
            tint: Color::new(1.0, 1.0, 1.0, 1.0), // identity: draw the decoded image as-is
            content_mask: no_clip,
            order: 0,
        });
        scene
    });

    let Some(r) = rendered else {
        eprintln!("skipping golden 'image_asset': no software adapter available");
        return;
    };
    if !r.canonical {
        eprintln!(
            "skipping golden compare 'image_asset': non-canonical rasterizer (goldens are lavapipe-canonical)"
        );
        return;
    }
    kagari_golden::assert_golden!("image_asset", r.image);
}
