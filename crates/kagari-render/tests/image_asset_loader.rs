//! Asset loader orchestration (#56), exercised deterministically with an inline spawner against a
//! real (headless) RGBA atlas — complements the pixel-compare golden. Runs whenever a software adapter
//! is available (broader than the lavapipe-canonical golden); skips cleanly when none is.
//!
//! Covers the loader paths the unit tests can't reach without a GPU atlas: a valid image decodes and
//! `resolve`s to its own (non-placeholder) coord; garbage and an over-atlas-size image both fall back
//! to the placeholder.

use std::sync::Arc;

use kagari_render::{AssetLoader, AssetSource, AtlasCoord, LoadState};

/// Encode a `w*h` solid-color RGBA image to PNG bytes in memory.
fn solid_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let buf = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(buf)
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("encode test PNG");
    out.into_inner()
}

#[test]
fn loader_should_resolve_ready_image_and_placeholder_on_failure() {
    // A distinct sentinel placeholder coord (page 99) so a real tile (page 0) never equals it.
    let placeholder = AtlasCoord {
        page: 99,
        min: [0.0, 0.0],
        max: [1.0, 1.0],
    };

    let ok_png = solid_png(4, 4, [10, 200, 30, 255]);
    // Larger than the RGBA atlas inner_max (2046 for the 2048 layer) → skipped by the packer.
    let oversize_png = solid_png(2100, 2100, [255, 255, 255, 255]);

    let ran = kagari_golden::headless_render_with((16, 16), 1.0, |renderer| {
        let mut loader = AssetLoader::new(|job| job(), placeholder);
        // Inline spawner → each `load` decodes synchronously; `drain` then applies the results.
        let ok_id = loader.load(AssetSource::Bytes(Arc::from(
            ok_png.clone().into_boxed_slice(),
        )));
        let bad_id = loader.load(AssetSource::Bytes(Arc::from(&[0xDE, 0xAD, 0xBE, 0xEF][..])));
        let big_id = loader.load(AssetSource::Bytes(Arc::from(
            oversize_png.clone().into_boxed_slice(),
        )));
        loader.drain(renderer.rgba_atlas_mut());

        // Valid image → Ready, resolves to its own (non-placeholder) coord.
        assert!(
            matches!(loader.state(ok_id), Some(LoadState::Ready(_))),
            "a valid image decodes to Ready"
        );
        assert_ne!(
            loader.resolve(ok_id),
            placeholder,
            "a ready image resolves to its own atlas coord, not the placeholder"
        );
        // Garbage bytes → Failed → placeholder.
        assert_eq!(
            loader.state(bad_id),
            Some(LoadState::Failed),
            "undecodable bytes are Failed"
        );
        assert_eq!(
            loader.resolve(bad_id),
            placeholder,
            "a failed asset resolves to the placeholder"
        );
        // Over-atlas-size image decodes but cannot be atlased → Failed → placeholder (the #56 review fix).
        assert_eq!(
            loader.state(big_id),
            Some(LoadState::Failed),
            "an image too large for the atlas falls back to Failed, not Ready(degenerate)"
        );
        assert_eq!(
            loader.resolve(big_id),
            placeholder,
            "an over-atlas-size image resolves to the placeholder"
        );

        kagari_render::Scene::new()
    });

    if ran.is_none() {
        eprintln!("skipping loader test: no software adapter available");
    }
}
