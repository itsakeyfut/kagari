//! Text shaping via cosmic-text: line breaking, wrapping, bidi, and font
//! fallback into placed glyphs plus an intrinsic size (specs §5.1), with a
//! result cache so unchanged runs are not re-shaped (specs §5.4). Rasterization
//! is #22.

use std::collections::HashMap;

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Wrap, fontdb};
use kagari_base::{Px, SharedString, Size};

use crate::font::FontDb;

/// Line height applied when `TextStyle::line_height` is `None`, as a multiple of
/// the font size (a common default until per-style line height is wired through).
const DEFAULT_LINE_HEIGHT_RATIO: f32 = 1.2;
/// Locale used when the system locale cannot be detected (affects Han-unified
/// glyph selection when multiple CJK fonts are present).
const FALLBACK_LOCALE: &str = "en-US";

/// How a run of text should be shaped.
#[derive(Clone, Debug)]
pub struct TextStyle {
    /// Requested font family (resolved against the database, with fallback).
    pub family: SharedString,
    /// Font size in logical px.
    pub size: Px,
    /// Font weight (fontdb's type, shared with cosmic-text's `Attrs`).
    pub weight: fontdb::Weight,
    /// Line height in logical px; `None` resolves to `size * 1.2` at shape time.
    pub line_height: Option<Px>,
}

/// A single glyph placed by the shaper, in logical px relative to the text origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedGlyph {
    /// Face the glyph was shaped from (for rasterization in #22).
    pub font_id: fontdb::ID,
    /// Glyph index within `font_id`.
    pub glyph_id: u16,
    /// X offset within the line.
    pub x: f32,
    /// Y offset (baseline-relative within the laid-out text).
    pub y: f32,
    /// Font size of this glyph in px.
    pub size_px: f32,
    /// Byte offset of the glyph's cluster in the original text.
    pub cluster: usize,
}

/// Per-line layout metrics (for measure / future cursor positioning).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineInfo {
    /// Y offset to the top of the line.
    pub top: f32,
    /// Line height in px.
    pub height: f32,
    /// Laid-out width of the line in px.
    pub width: f32,
}

/// The result of shaping: placed glyphs, the intrinsic size, and per-line info.
#[derive(Clone, Debug)]
pub struct ShapedText {
    /// Placed glyphs in layout order.
    pub glyphs: Vec<PlacedGlyph>,
    /// Intrinsic size (max line width × total height) for measure.
    pub size: Size,
    /// Per-line metrics.
    pub lines: Vec<LineInfo>,
}

/// Cache key for shaped results. Owns the source text so distinct strings never
/// collide (a hash-only key could return the wrong glyphs); `f32` inputs are keyed
/// by their bit pattern so the key is `Hash + Eq`.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ShapeKey {
    text: String,
    family: SharedString,
    size_bits: u32,
    line_height_bits: Option<u32>,
    weight: u16,
    wrap_bits: Option<u32>,
}

impl ShapeKey {
    fn new(text: &str, style: &TextStyle, wrap: Option<f32>) -> Self {
        Self {
            text: text.to_string(),
            family: style.family.clone(),
            size_bits: style.size.0.to_bits(),
            line_height_bits: style.line_height.map(|p| p.0.to_bits()),
            weight: style.weight.0,
            wrap_bits: wrap.map(f32::to_bits),
        }
    }
}

/// Owns the cosmic-text `FontSystem`, a shape-result cache, and the swash
/// rasterization state (#22). `scale_context` and `glyph_cache` are used by
/// `raster.rs`; the atlas itself (the glyph pixels + LRU) lives in the renderer.
pub struct TextSystem {
    pub(crate) font_system: FontSystem,
    cache: HashMap<ShapeKey, ShapedText>,
    pub(crate) scale_context: swash::scale::ScaleContext,
    pub(crate) glyph_cache: HashMap<u64, crate::raster::GlyphMetrics>,
}

impl TextSystem {
    /// Build from the #20 font database, using the detected system locale (falling
    /// back to `en-US`). The database's bundled-first ordering is preserved.
    pub fn new(font_db: FontDb) -> Self {
        let locale = sys_locale::get_locale().unwrap_or_else(|| FALLBACK_LOCALE.to_string());
        Self::with_locale(font_db, locale)
    }

    /// Build with an explicit locale (controls Han-unified glyph selection).
    pub fn with_locale(font_db: FontDb, locale: String) -> Self {
        Self {
            font_system: FontSystem::new_with_locale_and_db(locale, font_db.into_database()),
            cache: HashMap::new(),
            scale_context: swash::scale::ScaleContext::new(),
            glyph_cache: HashMap::new(),
        }
    }

    /// Shape `text` with `style`, wrapping at `wrap` px if given. Identical
    /// `(text, style, wrap)` inputs reuse the cached result.
    pub fn shape(&mut self, text: &str, style: &TextStyle, wrap: Option<f32>) -> ShapedText {
        let key = ShapeKey::new(text, style, wrap);
        if let Some(cached) = self.cache.get(&key) {
            return cached.clone();
        }
        let shaped = self.shape_uncached(text, style, wrap);
        self.cache.insert(key, shaped.clone());
        shaped
    }

    fn shape_uncached(&mut self, text: &str, style: &TextStyle, wrap: Option<f32>) -> ShapedText {
        let size_px = style.size.0;
        let line_height = style
            .line_height
            .map_or(size_px * DEFAULT_LINE_HEIGHT_RATIO, |p| p.0);
        let mut buffer = Buffer::new(
            &mut self.font_system,
            Metrics {
                font_size: size_px,
                line_height,
            },
        );

        let mut attrs = Attrs::new();
        attrs.family = Family::Name(&style.family);
        attrs.weight = style.weight;

        {
            let mut borrowed = buffer.borrow_with(&mut self.font_system);
            // Japanese has no inter-word spaces, so word-only wrapping would
            // overflow; WordOrGlyph falls back to glyph breaks when needed.
            borrowed.set_wrap(if wrap.is_some() {
                Wrap::WordOrGlyph
            } else {
                Wrap::None
            });
            borrowed.set_size(wrap, None);
            borrowed.set_text(text, &attrs, Shaping::Advanced, None);
            borrowed.shape_until_scroll(false);
        }

        let mut glyphs = Vec::new();
        let mut lines = Vec::new();
        let mut width = 0f32;
        let mut height = 0f32;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            height = height.max(run.line_top + run.line_height);
            lines.push(LineInfo {
                top: run.line_top,
                height: run.line_height,
                width: run.line_w,
            });
            for glyph in run.glyphs {
                // Logical position per cosmic-text's `LayoutGlyph::physical`: the
                // EM-unit offsets scale by font size, and `y_offset` subtracts.
                let x_off = glyph.font_size * glyph.x_offset;
                let y_off = glyph.font_size * glyph.y_offset;
                glyphs.push(PlacedGlyph {
                    font_id: glyph.font_id,
                    glyph_id: glyph.glyph_id,
                    x: glyph.x + x_off,
                    y: run.line_y + glyph.y - y_off,
                    size_px: glyph.font_size,
                    cluster: glyph.start,
                });
            }
        }

        ShapedText {
            glyphs,
            size: Size::new(width, height),
            lines,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> TextStyle {
        TextStyle {
            family: "Noto Sans".into(),
            size: Px(16.0),
            weight: fontdb::Weight::NORMAL,
            line_height: None,
        }
    }

    #[test]
    fn shape_mixed_japanese_should_return_size() {
        let mut system = TextSystem::new(FontDb::new());
        let shaped = system.shape("Hello 日本語", &style(), None);
        assert!(!shaped.glyphs.is_empty());
        assert!(shaped.size.w > 0.0 && shaped.size.h > 0.0);
    }

    #[test]
    fn shape_should_cache_identical_input() {
        let mut system = TextSystem::new(FontDb::new());
        let style = style();
        let _ = system.shape("日本語 test", &style, None);
        assert_eq!(system.cache.len(), 1);
        // Identical input must reuse the cached result, not add an entry.
        let _ = system.shape("日本語 test", &style, None);
        assert_eq!(system.cache.len(), 1);
        // A different string is a distinct key.
        let _ = system.shape("different", &style, None);
        assert_eq!(system.cache.len(), 2);
    }
}
