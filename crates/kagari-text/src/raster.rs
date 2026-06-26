//! Rasterize placed glyphs with swash into the mono atlas and emit
//! `MonochromeSprite`s (specs §5.1, T-M2-05). Each unique glyph is rasterized
//! once: the atlas (#19) owns the coverage pixels + LRU, and `TextSystem` caches
//! the per-glyph placement metrics so a cache hit needs no re-rasterization.

use std::hash::{Hash, Hasher};

use cosmic_text::fontdb;
use kagari_base::{Color, Corners, Rect};
use kagari_render::{Atlas, MonochromeSprite, RoundedRect};
use swash::scale::{Render, Source, image::Content};

use crate::shape::{PlacedGlyph, ShapedText, TextSystem};

/// Placement of a rasterized glyph relative to its pen origin (swash/zeno units):
/// `left`/`top` are the bitmap's offset from the origin, `w`/`h` its pixel size.
/// `w == 0` marks a glyph with no coverage (e.g. a space).
#[derive(Clone, Copy)]
pub(crate) struct GlyphMetrics {
    left: i32,
    top: i32,
    w: u32,
    h: u32,
}

impl GlyphMetrics {
    const EMPTY: Self = Self {
        left: 0,
        top: 0,
        w: 0,
        h: 0,
    };
}

/// Atlas/cache key: a glyph is identified by its face, glyph index, and integer
/// pixel size (no subpixel-offset variants in MVP, Q4). Weight is subsumed by
/// `font_id` (the face is already weight-specific; no synthetic bold in MVP).
fn glyph_key(font_id: fontdb::ID, glyph_id: u16, ppem: u32) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    font_id.hash(&mut hasher);
    glyph_id.hash(&mut hasher);
    ppem.hash(&mut hasher);
    hasher.finish()
}

/// The sprite bounds for a placed glyph, with the origin snapped to whole pixels
/// (Q4). `top` is the distance above the baseline, so it subtracts from the pen y.
fn glyph_bounds(x: f32, y: f32, m: GlyphMetrics) -> Rect {
    Rect::from_xywh(
        (x + m.left as f32).round(),
        (y - m.top as f32).round(),
        m.w as f32,
        m.h as f32,
    )
}

impl TextSystem {
    /// Rasterize `shaped`'s glyphs (swash → R8 coverage), insert them into `atlas`
    /// (cached; repeats reuse the coord), and push one `MonochromeSprite` per
    /// visible glyph into `out` (painter order 0, no clip — clipping/order are wired
    /// in later via core/layout).
    pub fn rasterize_into(
        &mut self,
        shaped: &ShapedText,
        color: Color,
        atlas: &mut Atlas,
        out: &mut Vec<MonochromeSprite>,
    ) {
        let no_clip = RoundedRect {
            rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
            radii: Corners::default(),
        };
        for glyph in &shaped.glyphs {
            let ppem = glyph.size_px.round().max(1.0) as u32;
            let key = glyph_key(glyph.font_id, glyph.glyph_id, ppem);
            let (metrics, coord) = match self.glyph_cache.get(&key).copied() {
                // Known empty glyph (e.g. a space): nothing to draw.
                Some(m) if m.w == 0 || m.h == 0 => continue,
                // Metrics cached; the atlas re-rasterizes only if the tile was evicted.
                Some(m) => {
                    let coord = atlas.get_or_insert(key, (m.w, m.h), || {
                        self.rasterize_glyph(glyph)
                            .map(|(_, bitmap)| bitmap)
                            .unwrap_or_default()
                    });
                    (m, coord)
                }
                // First encounter: rasterize once, cache metrics, hand the bitmap to the atlas.
                None => match self.rasterize_glyph(glyph) {
                    Some((m, bitmap)) if m.w > 0 && m.h > 0 => {
                        self.glyph_cache.insert(key, m);
                        let coord = atlas.get_or_insert(key, (m.w, m.h), move || bitmap);
                        (m, coord)
                    }
                    _ => {
                        self.glyph_cache.insert(key, GlyphMetrics::EMPTY);
                        continue;
                    }
                },
            };
            out.push(MonochromeSprite {
                bounds: glyph_bounds(glyph.x, glyph.y, metrics),
                tex: coord,
                color,
                content_mask: no_clip,
                order: 0,
            });
        }
    }

    /// Rasterize one glyph to an 8-bit (R8) coverage bitmap via swash. Returns the
    /// placement metrics and the `w*h` coverage bytes, or `None` if the face is
    /// missing or the glyph has no monochrome outline (e.g. a color/bitmap glyph).
    fn rasterize_glyph(&mut self, glyph: &PlacedGlyph) -> Option<(GlyphMetrics, Vec<u8>)> {
        let ppem = glyph.size_px.round().max(1.0);
        // The face is fixed by `font_id`; NORMAL weight avoids synthetic bold (MVP).
        let font = self
            .font_system
            .get_font(glyph.font_id, fontdb::Weight::NORMAL)?;
        let mut scaler = self
            .scale_context
            .builder(font.as_swash())
            .size(ppem)
            .hint(false)
            .build();
        // Default render format is `Alpha` ⇒ `Content::Mask` (one coverage byte/px).
        let image = Render::new(&[Source::Outline]).render(&mut scaler, glyph.glyph_id)?;
        if image.content != Content::Mask {
            return None;
        }
        let p = image.placement;
        Some((
            GlyphMetrics {
                left: p.left,
                top: p.top,
                w: p.width,
                h: p.height,
            },
            image.data,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::FontDb;

    #[test]
    fn glyph_bounds_should_snap_origin_to_integer() {
        let m = GlyphMetrics {
            left: 2,
            top: 12,
            w: 8,
            h: 10,
        };
        // Origin = (10.4 + 2, 30.6 - 12) rounded = (12, 19); size unchanged.
        let r = glyph_bounds(10.4, 30.6, m);
        assert_eq!((r.origin.x, r.origin.y), (12.0, 19.0));
        assert_eq!((r.size.w, r.size.h), (8.0, 10.0));
        assert_eq!(r.origin.x.fract(), 0.0);
        assert_eq!(r.origin.y.fract(), 0.0);
    }

    #[test]
    fn glyph_key_should_differ_by_glyph() {
        // FontDb is GPU-free, so resolving a real face id needs no device.
        let db = FontDb::new();
        let id = db
            .resolve("Noto Sans", fontdb::Weight::NORMAL)
            .expect("bundled Latin face resolves");
        assert_eq!(
            glyph_key(id, 7, 16),
            glyph_key(id, 7, 16),
            "stable for same input"
        );
        assert_ne!(
            glyph_key(id, 7, 16),
            glyph_key(id, 8, 16),
            "differs by glyph id"
        );
        assert_ne!(
            glyph_key(id, 7, 16),
            glyph_key(id, 7, 24),
            "differs by ppem"
        );
    }
}
