//! The recorded [`Style`] on an element (#38, specs §3.6).
//!
//! `Styled` builder methods (#41) write token references here; the resolver (#40) reads them and
//! turns them into concrete px/colors against the current `Theme`. Fields are typed per property
//! (e.g. `gap: Option<SpacingStep>`) so an invalid pairing cannot be recorded; `None` = unset.
//!
//! This covers the **token-bearing** style props only. Structural layout choices
//! (flex-direction / align / justify / position) are not theme-resolved tokens and live in
//! kagari-layout (a sibling crate); they are wired when `Styled` is implemented on elements (#42).

use crate::token::{
    BorderWidthStep, ColorRole, FontSizeStep, FontWeightStep, OpacityStep, RadiusStep, ShadowStep,
    SpacingStep,
};

/// A recorded style: layout tokens (→ taffy) and paint tokens (→ render scene).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Style {
    pub layout: LayoutTokens,
    pub paint: PaintTokens,
}

/// Layout-affecting tokens on the spacing scale (gap / padding / margin / size). Resolved to
/// logical px by the theme at `request_layout`.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct LayoutTokens {
    pub gap: Option<SpacingStep>,
    pub gap_x: Option<SpacingStep>,
    pub gap_y: Option<SpacingStep>,
    pub p: Option<SpacingStep>,
    pub px: Option<SpacingStep>,
    pub py: Option<SpacingStep>,
    pub pt: Option<SpacingStep>,
    pub pr: Option<SpacingStep>,
    pub pb: Option<SpacingStep>,
    pub pl: Option<SpacingStep>,
    pub m: Option<SpacingStep>,
    pub mx: Option<SpacingStep>,
    pub my: Option<SpacingStep>,
    pub mt: Option<SpacingStep>,
    pub mr: Option<SpacingStep>,
    pub mb: Option<SpacingStep>,
    pub ml: Option<SpacingStep>,
    pub w: Option<SpacingStep>,
    pub h: Option<SpacingStep>,
    pub min_w: Option<SpacingStep>,
    pub max_w: Option<SpacingStep>,
    pub min_h: Option<SpacingStep>,
    pub max_h: Option<SpacingStep>,
}

/// Appearance tokens (background / text / border / radius / shadow / opacity / font). Resolved at
/// `paint`; colors are semantic [`ColorRole`]s (§3.4).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct PaintTokens {
    pub bg: Option<ColorRole>,
    pub text: Option<ColorRole>,
    pub border_color: Option<ColorRole>,
    pub border_width: Option<BorderWidthStep>,
    pub rounded: Option<RadiusStep>,
    pub shadow: Option<ShadowStep>,
    pub opacity: Option<OpacityStep>,
    pub font_size: Option<FontSizeStep>,
    pub font_weight: Option<FontWeightStep>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_default_should_leave_all_props_unset() {
        let style = Style::default();
        assert_eq!(style.layout.gap, None);
        assert_eq!(style.layout.p, None);
        assert_eq!(style.layout.w, None);
        assert_eq!(style.paint.bg, None);
        assert_eq!(style.paint.rounded, None);
    }

    #[test]
    fn styled_record_should_store_token() {
        // The shape #41's macro writes: a setter records a typed step/role into the field.
        let mut style = Style::default();
        style.layout.gap = Some(SpacingStep::S4);
        style.paint.bg = Some(ColorRole::Accent);
        assert_eq!(style.layout.gap, Some(SpacingStep::S4));
        assert_eq!(style.paint.bg, Some(ColorRole::Accent));
    }
}
