//! Icon loader orchestration (#57), exercised deterministically against a real (headless) RGBA atlas —
//! complements the pixel-compare `icon_golden`. Runs whenever a software adapter is available (broader
//! than the lavapipe-canonical golden); skips cleanly when none is.
//!
//! Covers what the golden can't assert locally: `icon_coord` rasterizes into the atlas and returns a
//! non-degenerate coord, the same `(icon, px)` is cached (idempotent), and a different `px` is a distinct
//! tile.

use kagari_render::IconId;

#[test]
fn icon_coord_should_cache_and_vary_by_size() {
    let ran = kagari_golden::headless_render_with((16, 16), 1.0, |renderer| {
        let a = renderer.icon_coord(IconId::Check, 32);
        // Non-degenerate: a real tile has a non-empty uv rect (max != [0,0], min != max).
        assert_ne!(
            a.max,
            [0.0, 0.0],
            "a normal-size icon rasterizes to a real atlas tile"
        );
        assert_ne!(a.min, a.max, "the icon tile has a non-empty uv rect");

        // Idempotent: the same (icon, px) hits the cache and returns the same coord (no re-rasterize).
        let a2 = renderer.icon_coord(IconId::Check, 32);
        assert_eq!(a, a2, "same (icon, px) resolves to the same cached coord");

        // A different size is a distinct tile (re-rasterized at the new px).
        let b = renderer.icon_coord(IconId::Check, 48);
        assert_ne!(a, b, "a different px yields a distinct tile");

        // A different icon at the same size is also distinct.
        let c = renderer.icon_coord(IconId::Close, 32);
        assert_ne!(a, c, "a different icon yields a distinct tile");

        kagari_render::Scene::new()
    });

    if ran.is_none() {
        eprintln!("skipping icon_loader test: no software adapter available");
    }
}
