//! SVG icon rasterization (#57, specs §2.10): rasterize a bundled named SVG to a target pixel size with
//! `resvg`, un-premultiply to straight-alpha sRGB, ready for the RGBA atlas (#55) as a `PolychromeSprite`.
//!
//! **Glyph-style synchronous rasterize-on-miss**: [`Renderer::icon_coord`](crate::Renderer::icon_coord)
//! keys the atlas by `(icon, px)` and rasterizes inside `Atlas::get_or_insert` on a cache miss (like a
//! glyph), so an icon is available the frame it is first requested. A size change is a different key → a
//! re-rasterized tile (crisp at any size / DPI). Bundled named icons only; runtime SVG-file loading is a
//! follow-up.
//!
//! **Parse once, render per size (#250)**: the SVG parse ([`parse_svg`]) is the heaviest, `px`-independent
//! part, so [`IconCache`] parses each [`IconId`] once and re-renders the cached [`usvg::Tree`]
//! ([`rasterize_tree`]) per `px` — otherwise a size-animated / zoomed icon re-parses every frame.
//!
//! Bundled icons are **monochrome white** shapes (stroked or filled) on transparent ground: the polychrome `tint` *multiplies*
//! the sampled texel (#55), so a white icon is tinted to any theme color (a black icon could not be), and
//! `tint = WHITE` draws it white.

use std::collections::HashMap;
use std::collections::hash_map::{DefaultHasher, Entry};
use std::hash::{Hash, Hasher};

use resvg::{tiny_skia, usvg};

use crate::assets::DecodedImage;
use crate::error::RenderError;

/// A bundled icon. `#[non_exhaustive]` so the set can grow without a breaking change.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum IconId {
    /// A close/dismiss "✕".
    Close,
    /// A confirm "✓".
    Check,
    /// A downward chevron "⌄".
    ChevronDown,
    /// A vertical drag-handle grip "⠿" (two columns of dots), used as a reorder drag source.
    GripVertical,
}

/// All bundled icons — for tests that enumerate the set (test-only until a non-test consumer needs it).
#[cfg(test)]
const ALL_ICONS: &[IconId] = &[
    IconId::Close,
    IconId::Check,
    IconId::ChevronDown,
    IconId::GripVertical,
];

/// The inline SVG source for a bundled icon (24×24 viewBox, monochrome white shapes — stroked paths or
/// filled — on a transparent ground, tintable via the polychrome `tint`).
fn icon_svg(id: IconId) -> &'static str {
    match id {
        IconId::Close => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M6 6L18 18M18 6L6 18" stroke="white" stroke-width="2" stroke-linecap="round" fill="none"/></svg>"##
        }
        IconId::Check => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M5 13l5 5L19 7" stroke="white" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/></svg>"##
        }
        IconId::ChevronDown => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M6 9l6 6 6-6" stroke="white" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" fill="none"/></svg>"##
        }
        IconId::GripVertical => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><g fill="white"><circle cx="9" cy="6" r="1.6"/><circle cx="9" cy="12" r="1.6"/><circle cx="9" cy="18" r="1.6"/><circle cx="15" cy="6" r="1.6"/><circle cx="15" cy="12" r="1.6"/><circle cx="15" cy="18" r="1.6"/></g></svg>"##
        }
    }
}

/// The atlas key for `(icon, px)` — namespaced (discriminator `2`) so it cannot collide with the asset
/// loader's `AssetId` hashes (discriminators `0`/`1`) or the placeholder key in the shared RGBA atlas.
pub(crate) fn icon_key(icon: IconId, px: u32) -> u64 {
    let mut h = DefaultHasher::new();
    2u8.hash(&mut h);
    icon.hash(&mut h);
    px.hash(&mut h);
    h.finish()
}

/// Parse an SVG source into a reusable [`usvg::Tree`]. This is the heaviest, **`px`-independent** part of
/// rasterization (#250), so callers cache the tree and re-render it per size. Parse failure →
/// [`RenderError::AssetDecode`] (panic-free); `label` describes the source for the error.
pub(crate) fn parse_svg(svg: &str, label: &str) -> Result<usvg::Tree, RenderError> {
    let opt = usvg::Options::default();
    usvg::Tree::from_data(svg.as_bytes(), &opt).map_err(|_| RenderError::AssetDecode {
        path: label.to_string(),
    })
}

/// Render an already-parsed [`usvg::Tree`] to a `px`×`px` straight-alpha sRGB RGBA tile via
/// resvg/tiny_skia. resvg renders **sRGB premultiplied**; the atlas + polychrome shader expect **straight**
/// sRGB (the shader premultiplies in linear, #55), so we `demultiply` each pixel here. A zero `px` →
/// [`RenderError::AssetDecode`] (panic-free); `label` describes the source for the error.
pub(crate) fn rasterize_tree(
    tree: &usvg::Tree,
    px: u32,
    label: &str,
) -> Result<DecodedImage, RenderError> {
    let size = tree.size();
    let mut pixmap = tiny_skia::Pixmap::new(px, px).ok_or_else(|| RenderError::AssetDecode {
        path: label.to_string(),
    })?;
    // Fit the SVG's viewBox uniformly into the px×px tile (keep aspect; bundled icons are square).
    let scale = (px as f32 / size.width()).min(px as f32 / size.height());
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut rgba = Vec::with_capacity((px as usize) * (px as usize) * 4);
    for p in pixmap.pixels() {
        let c = p.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Ok(DecodedImage {
        rgba,
        width: px,
        height: px,
    })
}

/// Caches the parsed [`usvg::Tree`] per [`IconId`] (#250) so a bundled icon is parsed **once** and only
/// re-rendered per `px`. The parse (the `px`-independent, heaviest part of resvg) otherwise runs on every
/// atlas miss — each DPI, and every frame of a size-animated / zoomed icon. Owned by the
/// [`Renderer`](crate::Renderer); CPU-side, so it survives device-loss recovery (the atlas re-uploads its
/// tiles; the parsed trees stay valid).
#[derive(Default)]
pub(crate) struct IconCache {
    trees: HashMap<IconId, usvg::Tree>,
}

impl IconCache {
    /// An empty cache.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The parsed tree for `icon`, parsing (and caching) it on the first request. A parse failure is
    /// propagated and **not** cached (a bundled icon never fails, so a re-attempt is a non-issue).
    fn tree(&mut self, icon: IconId) -> Result<&usvg::Tree, RenderError> {
        match self.trees.entry(icon) {
            Entry::Occupied(e) => Ok(e.into_mut()),
            Entry::Vacant(e) => {
                let tree = parse_svg(icon_svg(icon), "icon")?;
                Ok(e.insert(tree))
            }
        }
    }

    /// Rasterize a bundled `icon` at `px`, reusing the cached parsed tree. See [`rasterize_tree`].
    pub(crate) fn rasterize(&mut self, icon: IconId, px: u32) -> Result<DecodedImage, RenderError> {
        let tree = self.tree(icon)?;
        rasterize_tree(tree, px, "icon")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The center pixel's RGBA of a `px`×`px` decoded tile.
    fn center(d: &DecodedImage) -> [u8; 4] {
        let c = d.width / 2;
        let i = ((c * d.width + c) * 4) as usize;
        [d.rgba[i], d.rgba[i + 1], d.rgba[i + 2], d.rgba[i + 3]]
    }

    #[test]
    fn rasterize_tree_should_produce_rgba() {
        // A fully-covered green square: the center texel is opaque green.
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="rgb(10,200,30)"/></svg>"##;
        let tree = parse_svg(svg, "test").expect("parse the SVG");
        let d = rasterize_tree(&tree, 16, "test").expect("rasterize the tree");
        assert_eq!((d.width, d.height), (16, 16));
        assert_eq!(d.rgba.len(), 16 * 16 * 4);
        let [r, g, b, a] = center(&d);
        assert_eq!(a, 255, "covered center is opaque");
        assert!(
            r < 40 && (180..=220).contains(&g) && b < 60,
            "center is ~green (10,200,30), got ({r},{g},{b})"
        );
    }

    #[test]
    fn rasterize_tree_should_unpremultiply() {
        // A 50%-alpha red fill: tiny_skia stores it premultiplied (r≈100), but the decoded tile must be
        // straight alpha (r≈200) — proving the demultiply. (Premultiplied red would be ~100.)
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="rgb(200,0,0)" fill-opacity="0.5"/></svg>"##;
        let tree = parse_svg(svg, "test").expect("parse the SVG");
        let d = rasterize_tree(&tree, 16, "test").expect("rasterize the tree");
        let [r, _g, _b, a] = center(&d);
        assert!((110..=150).contains(&a), "alpha ~128 (50%), got {a}");
        assert!(
            r >= 180,
            "straight red ~200 after demultiply (premultiplied would be ~100), got {r}"
        );
    }

    #[test]
    fn parse_svg_should_error_on_garbage() {
        let result = parse_svg("not an svg <<<", "garbage");
        assert!(
            matches!(result, Err(RenderError::AssetDecode { .. })),
            "invalid SVG maps to AssetDecode (no panic)"
        );
    }

    #[test]
    fn icon_key_should_differ_by_icon_and_px() {
        // Deterministic and distinct per (icon, px) — so the shared RGBA atlas never aliases two icons
        // or two sizes. (Namespaced by discriminator 2; see the doc on `icon_key`.)
        assert_eq!(
            icon_key(IconId::Check, 32),
            icon_key(IconId::Check, 32),
            "same (icon, px) → same key (deterministic)"
        );
        assert_ne!(
            icon_key(IconId::Check, 32),
            icon_key(IconId::Close, 32),
            "different icon → different key"
        );
        assert_ne!(
            icon_key(IconId::Check, 32),
            icon_key(IconId::Check, 48),
            "different px → different key (re-rasterized tile)"
        );
    }

    #[test]
    fn every_bundled_icon_should_rasterize() {
        let mut cache = IconCache::new();
        for &icon in ALL_ICONS {
            let d = cache
                .rasterize(icon, 24)
                .unwrap_or_else(|_| panic!("{icon:?} should rasterize"));
            assert_eq!((d.width, d.height), (24, 24));
            assert!(
                d.rgba.chunks_exact(4).any(|p| p[3] > 0),
                "{icon:?} has some non-transparent pixels"
            );
        }
    }

    #[test]
    fn icon_cache_should_parse_tree_once_across_sizes() {
        // The #250 core: two sizes of the same icon parse the tree once (cached), then re-render per px.
        let mut cache = IconCache::new();
        let a = cache.rasterize(IconId::Check, 16).expect("rasterize @16");
        let b = cache.rasterize(IconId::Check, 32).expect("rasterize @32");
        assert_eq!((a.width, b.width), (16, 32), "each size renders at its px");
        assert_eq!(
            cache.trees.len(),
            1,
            "the tree is parsed once and reused across sizes"
        );
    }

    #[test]
    fn icon_cache_should_parse_per_icon() {
        let mut cache = IconCache::new();
        cache.rasterize(IconId::Check, 24).expect("check");
        cache.rasterize(IconId::Close, 24).expect("close");
        cache.rasterize(IconId::Check, 24).expect("check again");
        assert_eq!(
            cache.trees.len(),
            2,
            "one cached tree per distinct icon (a repeat request adds nothing)"
        );
    }

    #[test]
    fn icon_cache_rasterize_should_match_uncached() {
        // The refactor is output-preserving: the cached path equals a fresh parse + render.
        let mut cache = IconCache::new();
        let cached = cache.rasterize(IconId::Check, 24).expect("cached");
        let tree = parse_svg(icon_svg(IconId::Check), "icon").expect("parse");
        let direct = rasterize_tree(&tree, 24, "icon").expect("direct");
        assert_eq!(
            cached.rgba, direct.rgba,
            "the cached rasterize equals a fresh parse + render"
        );
    }
}
