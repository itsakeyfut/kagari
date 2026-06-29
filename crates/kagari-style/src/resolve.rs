//! Lazy token resolution (#40, specs §3.2): turn a token reference into a concrete value against
//! the current [`Theme`], with colors decoded sRGB→linear working space.
//!
//! These run on the per-frame paint hot path, so they are **total** (no `Result`) and **silent**
//! (no per-call logging). Completeness is enforced once at load by [`Theme::validate`]; for a
//! validated theme the fallback below is unreachable. As defense-in-depth, an unmapped color role
//! falls back to a **debug-visible magenta** (release: transparent) so a bug that slips past
//! validation is obvious on screen in dev without shipping a loud color to users.

use kagari_base::{Color, ColorSpace, Px};

use crate::error::StyleError;
use crate::theme::Theme;
use crate::token::{
    BorderWidthStep, ColorRole, FontSizeStep, OpacityStep, RadiusStep, SpacingStep,
};

/// kagari's MVP working space (linear-Rec709, kagari-base §6.1). Colors resolve into it.
const WORKING_SPACE: ColorSpace = ColorSpace::Rec709;

/// Fallback for an unmapped color role: opaque magenta in debug builds (loud "missing token" on
/// screen, the game-engine convention), transparent in release (never ship magenta to users).
fn missing_color() -> Color {
    if cfg!(debug_assertions) {
        Color::from_srgb([1.0, 0.0, 1.0, 1.0])
    } else {
        Color::TRANSPARENT
    }
}

impl Theme {
    /// Resolves a semantic [`ColorRole`] to a linear working-space [`Color`] (two-layer: role →
    /// palette → sRGB → linear, premultiplied). Total; an unmapped role — which a theme that passed
    /// [`validate`](Self::validate) never has — yields a debug-visible sentinel (magenta in debug,
    /// transparent in release).
    pub fn resolve_color(&self, role: ColorRole) -> Color {
        match self.role_color(role) {
            Some(tagged) => tagged.to_working(WORKING_SPACE),
            None => missing_color(),
        }
    }

    /// Resolves a spacing step to logical px (`Px(0.0)` if the theme omits it).
    pub fn resolve_spacing(&self, step: SpacingStep) -> Px {
        self.spacing(step).unwrap_or(Px(0.0))
    }

    /// Resolves a corner-radius step to logical px (`Px(0.0)` if omitted).
    pub fn resolve_radius(&self, step: RadiusStep) -> Px {
        self.radius(step).unwrap_or(Px(0.0))
    }

    /// Resolves a font-size step to logical px (`Px(0.0)` if omitted).
    pub fn resolve_font_size(&self, step: FontSizeStep) -> Px {
        self.font_size(step).unwrap_or(Px(0.0))
    }

    /// Resolves an opacity step to a fraction in `0.0..=1.0`. Fallback **1.0** (fully opaque) — a
    /// `0.0` fallback would silently hide content.
    pub fn resolve_opacity(&self, step: OpacityStep) -> f32 {
        self.primitives.opacity.get(&step).copied().unwrap_or(1.0)
    }

    /// Resolves a border-width step to logical px (`Px(0.0)` = no border if omitted).
    pub fn resolve_border_width(&self, step: BorderWidthStep) -> Px {
        self.primitives
            .border_width
            .get(&step)
            .copied()
            .unwrap_or(Px(0.0))
    }

    /// Validates that every [`ColorRole`] resolves to a palette color (mapped, and its palette key
    /// exists). Call once at load — for a theme that passes, the resolution hot path never hits its
    /// fallback. Returns the first role that does not resolve.
    pub fn validate(&self) -> Result<(), StyleError> {
        for &role in &ColorRole::ALL {
            if self.role_color(role).is_none() {
                return Err(StyleError::UnknownRole(role));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{HexColor, Primitives, SemanticRoles};
    use kagari_base::{TaggedColor, Transfer};
    use std::collections::HashMap;

    fn srgb_hex(value: [f32; 4]) -> HexColor {
        HexColor(TaggedColor {
            value,
            space: ColorSpace::Srgb,
            transfer: Transfer::Srgb,
        })
    }

    /// A theme mapping every `ColorRole` to one palette entry (so `validate` passes), plus one
    /// spacing/radius/font-size entry each for the scale-resolution tests.
    fn complete_theme() -> Theme {
        let mut colors = HashMap::new();
        colors.insert("base".to_string(), srgb_hex([0.5, 0.5, 0.5, 1.0]));
        let mut roles = HashMap::new();
        for &role in &ColorRole::ALL {
            roles.insert(role, "base".to_string());
        }
        let mut spacing = HashMap::new();
        spacing.insert(SpacingStep::S4, Px(16.0));
        let mut radius = HashMap::new();
        radius.insert(RadiusStep::Lg, Px(8.0));
        let mut font_size = HashMap::new();
        font_size.insert(FontSizeStep::Base, Px(16.0));
        Theme {
            primitives: Primitives {
                colors,
                spacing,
                radius,
                font_size,
                ..Primitives::default()
            },
            roles: SemanticRoles { colors: roles },
        }
    }

    #[test]
    fn resolve_color_should_convert_srgb_to_linear() {
        let theme = complete_theme(); // "base" = sRGB 0.5 grey
        let c = theme.resolve_color(ColorRole::Accent);
        // sRGB 0.5 decodes to ~0.214 linear; alpha 1, premultiplied (RGB unchanged).
        assert!(
            (c.r - 0.214).abs() < 0.01,
            "sRGB 0.5 → ~0.214 linear (got {})",
            c.r
        );
        assert_eq!(c.a, 1.0);
    }

    #[test]
    fn resolve_spacing_should_return_theme_value() {
        assert_eq!(complete_theme().resolve_spacing(SpacingStep::S4), Px(16.0));
    }

    #[test]
    fn resolve_spacing_missing_should_fall_back_to_zero() {
        // S8 is not defined by the test theme.
        assert_eq!(complete_theme().resolve_spacing(SpacingStep::S8), Px(0.0));
    }

    #[test]
    fn resolve_radius_should_return_theme_value_or_zero() {
        let theme = complete_theme();
        assert_eq!(theme.resolve_radius(RadiusStep::Lg), Px(8.0));
        assert_eq!(theme.resolve_radius(RadiusStep::Sm), Px(0.0)); // undefined → fallback
    }

    #[test]
    fn resolve_font_size_should_return_theme_value_or_zero() {
        let theme = complete_theme();
        assert_eq!(theme.resolve_font_size(FontSizeStep::Base), Px(16.0));
        assert_eq!(theme.resolve_font_size(FontSizeStep::Xl), Px(0.0)); // undefined → fallback
    }

    #[test]
    fn validate_should_accept_complete_theme() {
        assert!(complete_theme().validate().is_ok());
    }

    #[test]
    fn validate_should_reject_unmapped_role() {
        let mut theme = complete_theme();
        theme.roles.colors.remove(&ColorRole::Accent);
        assert!(matches!(
            theme.validate(),
            Err(StyleError::UnknownRole(ColorRole::Accent))
        ));
    }

    #[test]
    fn resolve_opacity_should_return_fraction_or_opaque() {
        let light = Theme::light();
        assert_eq!(light.resolve_opacity(OpacityStep::O50), 0.5);
        // An undefined step (empty theme) falls back to fully opaque, not invisible.
        assert_eq!(Theme::default().resolve_opacity(OpacityStep::O50), 1.0);
    }

    #[test]
    fn resolve_border_width_should_return_px_or_zero() {
        assert_eq!(
            Theme::light().resolve_border_width(BorderWidthStep::B2),
            Px(2.0)
        );
        assert_eq!(
            Theme::default().resolve_border_width(BorderWidthStep::B2),
            Px(0.0)
        );
    }
}
