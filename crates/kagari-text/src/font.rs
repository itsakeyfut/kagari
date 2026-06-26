//! Font discovery (fontdb) and the CJK-aware fallback chain (specs §5.1).
//!
//! Bundled OFL faces (a Latin Noto Sans and a Japanese subset of Noto Sans JP)
//! load first so resolution is deterministic across machines; the system fonts
//! load afterwards and cover anything the bundled subsets miss. `FontDb` is kept
//! independent of cosmic-text shaping so it is unit-testable on its own — #21
//! builds the shaping `FontSystem` from this `fontdb::Database`.

use std::sync::Arc;

use cosmic_text::fontdb;

use crate::error::TextError;

/// Bundled Latin face (Noto Sans subset, OFL — see `assets/fonts/OFL.txt`).
const LATIN_FONT: &[u8] = include_bytes!("../assets/fonts/NotoSans-Regular.subset.ttf");
/// Bundled Japanese face (Noto Sans JP subset, OFL — see `assets/fonts/OFL.txt`).
const CJK_FONT: &[u8] = include_bytes!("../assets/fonts/NotoSansJP-Regular.subset.ttf");

/// A loaded font database with a resolved bundled-first fallback order.
pub struct FontDb {
    db: fontdb::Database,
    /// Fallback order: `[latin-bundled, cjk-bundled, system…]`. The bundled faces
    /// lead so Japanese coverage is guaranteed before falling through to system.
    fallback: Vec<fontdb::ID>,
}

impl FontDb {
    /// Build the database: bundled faces first (deterministic), then system fonts.
    pub fn new() -> Self {
        let mut db = fontdb::Database::new();
        let latin = db
            .load_font_source(fontdb::Source::Binary(Arc::new(LATIN_FONT)))
            .to_vec();
        let cjk = db
            .load_font_source(fontdb::Source::Binary(Arc::new(CJK_FONT)))
            .to_vec();
        db.load_system_fonts();

        // INVARIANT: the bundled faces are valid single-face TTFs embedded at
        // compile time, so each load yields at least one face id (asserted by the
        // unit tests that resolve them).
        let latin_id = *latin
            .first()
            .expect("bundled Latin font has at least one face");
        let cjk_id = *cjk.first().expect("bundled CJK font has at least one face");

        let mut fallback = vec![latin_id, cjk_id];
        fallback.extend(
            db.faces()
                .map(|f| f.id)
                .filter(|id| *id != latin_id && *id != cjk_id),
        );

        Self { db, fallback }
    }

    /// Resolve a family name + weight to a concrete face. Because bundled faces
    /// load first, a bundled match wins over a system one of the same family.
    pub fn resolve(&self, family: &str, weight: fontdb::Weight) -> Result<fontdb::ID, TextError> {
        let query = fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            weight,
            stretch: fontdb::Stretch::Normal,
            style: fontdb::Style::Normal,
        };
        self.db
            .query(&query)
            .ok_or_else(|| TextError::NoFontMatch(family.to_string()))
    }

    /// The bundled-first fallback order (`latin-bundled, cjk-bundled, system…`).
    pub fn fallback_chain(&self) -> &[fontdb::ID] {
        &self.fallback
    }

    /// Borrow the underlying database (#21 builds a cosmic-text `FontSystem` from it).
    pub fn database(&self) -> &fontdb::Database {
        &self.db
    }
}

impl Default for FontDb {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_resolve_should_find_japanese_family() {
        let db = FontDb::new();
        // The bundled Noto Sans JP subset must resolve regardless of system fonts.
        assert!(db.resolve("Noto Sans JP", fontdb::Weight::NORMAL).is_ok());
    }

    #[test]
    fn fallback_chain_should_include_cjk() {
        let db = FontDb::new();
        let cjk = db
            .resolve("Noto Sans JP", fontdb::Weight::NORMAL)
            .expect("bundled CJK face resolves");
        assert!(db.fallback_chain().contains(&cjk));
    }
}
