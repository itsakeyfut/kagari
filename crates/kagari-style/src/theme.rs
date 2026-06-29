//! The two-layer theme (#39, specs §3.4/§3.7): a primitive palette + scale tables (lower layer)
//! and a semantic role table (upper layer) that maps a [`ColorRole`] to a palette key.
//!
//! Themes are **data-driven**: every primitive (colors *and* scales) is RON data, so swapping the
//! theme re-resolves all token references with no recompile (§3.2). Colors are authored as sRGB hex
//! strings; the color-space tag is preserved (a [`TaggedColor`] tagged `Srgb`) and the conversion to
//! linear working space happens at resolve time (#40). Concrete light/dark values arrive in #45.

use std::collections::HashMap;

use kagari_base::{ColorSpace, Px, TaggedColor, Transfer};
use serde::Deserialize;

use crate::error::StyleError;
use crate::token::{ColorRole, FontSizeStep, RadiusStep, SpacingStep};

/// A two-layer theme: primitive palette + scales, plus the semantic role table.
///
/// `Default` is an empty theme (no palette/roles/scales) — used as the app's placeholder until the
/// reactive theme context (#43) and built-in light/dark themes (#45) land; every token then
/// resolves to its fallback.
#[derive(Clone, PartialEq, Debug, Default, Deserialize)]
pub struct Theme {
    pub primitives: Primitives,
    pub roles: SemanticRoles,
}

/// The lower layer: the raw color palette (keyed `"blue-500"` etc.) and the numeric scales.
#[derive(Clone, PartialEq, Debug, Default, Deserialize)]
pub struct Primitives {
    /// Color palette, keyed by `"<hue>-<shade>"` (e.g. `"blue-500"`); authored as sRGB hex.
    pub colors: HashMap<String, HexColor>,
    /// Spacing scale → logical px.
    #[serde(default)]
    pub spacing: HashMap<SpacingStep, Px>,
    /// Corner-radius scale → logical px.
    #[serde(default)]
    pub radius: HashMap<RadiusStep, Px>,
    /// Font-size scale → logical px.
    #[serde(default)]
    pub font_size: HashMap<FontSizeStep, Px>,
}

/// The upper layer: each semantic [`ColorRole`] points at a palette key (e.g. `Accent → "blue-500"`).
/// Reskinning is remapping this table (§3.4).
#[derive(Clone, PartialEq, Debug, Default, Deserialize)]
pub struct SemanticRoles {
    pub colors: HashMap<ColorRole, String>,
}

/// A palette color authored as an sRGB hex string (`"#rrggbb"` / `"#rrggbbaa"`). Deserializes into a
/// [`TaggedColor`] tagged `Srgb` (straight-alpha), preserving the color-space tag for the resolver.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct HexColor(pub TaggedColor);

impl<'de> Deserialize<'de> for HexColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let value = parse_hex(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid sRGB hex color: {s:?}")))?;
        Ok(HexColor(TaggedColor {
            value,
            space: ColorSpace::Srgb,
            transfer: Transfer::Srgb,
        }))
    }
}

/// Parses `#rrggbb` or `#rrggbbaa` into straight-alpha sRGB `[r, g, b, a]` in `0.0..=1.0`. Returns
/// `None` for any malformed input (wrong length, non-hex, non-ASCII) so the caller maps it to a
/// parse error instead of panicking.
fn parse_hex(s: &str) -> Option<[f32; 4]> {
    let h = s.strip_prefix('#')?;
    if !h.is_ascii() {
        return None;
    }
    let (r, g, b, a) = match h.len() {
        6 => (byte(h, 0)?, byte(h, 2)?, byte(h, 4)?, 255),
        8 => (byte(h, 0)?, byte(h, 2)?, byte(h, 4)?, byte(h, 6)?),
        _ => return None,
    };
    Some([
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ])
}

/// Parses the two ASCII-hex digits of `h` at byte offset `i` (caller guarantees `h.is_ascii()`).
fn byte(h: &str, i: usize) -> Option<u8> {
    u8::from_str_radix(&h[i..i + 2], 16).ok()
}

impl Theme {
    /// Deserializes a theme from RON. A syntax/type error or a malformed color becomes
    /// [`StyleError::ThemeParse`].
    pub fn from_ron(ron: &str) -> Result<Theme, StyleError> {
        ron::from_str(ron).map_err(|e| StyleError::ThemeParse(e.to_string()))
    }

    /// Two-layer color lookup: semantic role → palette key → palette entry (sRGB, pre-linear). The
    /// resolver (#40) converts this to a linear working [`Color`](kagari_base::Color).
    pub fn role_color(&self, role: ColorRole) -> Option<TaggedColor> {
        let key = self.roles.colors.get(&role)?;
        self.primitives.colors.get(key).map(|hex| hex.0)
    }

    /// Spacing scale lookup (logical px), or `None` if the step is not defined by this theme.
    pub fn spacing(&self, step: SpacingStep) -> Option<Px> {
        self.primitives.spacing.get(&step).copied()
    }

    /// Corner-radius scale lookup (logical px).
    pub fn radius(&self, step: RadiusStep) -> Option<Px> {
        self.primitives.radius.get(&step).copied()
    }

    /// Font-size scale lookup (logical px).
    pub fn font_size(&self, step: FontSizeStep) -> Option<Px> {
        self.primitives.font_size.get(&step).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r##"(
        primitives: (
            colors: {
                "blue-500": "#3b82f6",
                "slate-900": "#0f172a",
            },
            spacing: { S4: 16.0 },
        ),
        roles: (
            colors: {
                Accent: "blue-500",
                Surface: "slate-900",
            },
        ),
    )"##;

    #[test]
    fn theme_from_ron_should_load_palette_and_roles() {
        let theme = Theme::from_ron(FIXTURE).expect("fixture parses");
        assert!(theme.primitives.colors.contains_key("blue-500"));
        assert_eq!(
            theme.roles.colors.get(&ColorRole::Accent),
            Some(&"blue-500".to_string())
        );
        assert_eq!(theme.spacing(SpacingStep::S4), Some(Px(16.0)));
    }

    #[test]
    fn theme_role_should_resolve_to_palette_entry() {
        let theme = Theme::from_ron(FIXTURE).unwrap();
        // accent → "blue-500" → #3b82f6, tagged sRGB (straight alpha, not yet linear).
        let accent = theme
            .role_color(ColorRole::Accent)
            .expect("accent resolves");
        assert_eq!(accent.space, ColorSpace::Srgb);
        assert_eq!(accent.transfer, Transfer::Srgb);
        assert_eq!(accent.value[0], 0x3b as f32 / 255.0);
        assert_eq!(accent.value[3], 1.0);
        // An unmapped role resolves to None.
        assert_eq!(theme.role_color(ColorRole::FocusRing), None);
    }

    #[test]
    fn theme_from_ron_invalid_hex_should_error() {
        let bad = r##"( primitives: ( colors: { "x": "#zz" } ), roles: ( colors: {} ) )"##;
        assert!(matches!(
            Theme::from_ron(bad),
            Err(StyleError::ThemeParse(_))
        ));
    }

    #[test]
    fn hex_color_should_parse_rrggbb_and_rrggbbaa() {
        assert_eq!(parse_hex("#000000"), Some([0.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_hex("#ffffff"), Some([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(parse_hex("#00000080"), Some([0.0, 0.0, 0.0, 128.0 / 255.0]));
        // Malformed inputs do not panic; they return None.
        assert_eq!(parse_hex("3b82f6"), None); // no '#'
        assert_eq!(parse_hex("#abc"), None); // wrong length
        assert_eq!(parse_hex("#gggggg"), None); // non-hex
    }
}
