//! Value-free token reference types (#38, specs §3.2/§3.4/§3.5).
//!
//! A token records *which* step or semantic role was requested (e.g. `SpacingStep::S2`), never a
//! concrete px/color — those are resolved from the current `Theme` at layout/paint (#40). Each
//! scale is an explicit enum so a typo (`gap_999`) is a compile error and completion works (§3.3).
//! `.` in Tailwind names becomes `p` (e.g. `0.5` → `S0p5`), since Rust idents cannot contain `.`.
//!
//! The token enums are `#[non_exhaustive]`: scale steps and semantic roles are expected to grow, so
//! downstream crates must not match them exhaustively (a new variant is then a non-breaking change).

/// Spacing scale (Tailwind-faithful, incl. fractional). Resolved to logical px by the theme.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum SpacingStep {
    S0,
    SPx,
    S0p5,
    S1,
    S1p5,
    S2,
    S2p5,
    S3,
    S3p5,
    S4,
    S5,
    S6,
    S7,
    S8,
    S9,
    S10,
    S11,
    S12,
    S14,
    S16,
    S20,
    S24,
    S28,
    S32,
    S36,
    S40,
    S44,
    S48,
    S52,
    S56,
    S60,
    S64,
    S72,
    S80,
    S96,
}

/// Corner-radius scale.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum RadiusStep {
    None,
    Sm,
    Md,
    Lg,
    Xl,
    Xl2,
    Xl3,
    Full,
}

/// Font-size scale.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum FontSizeStep {
    Xs,
    Sm,
    Base,
    Lg,
    Xl,
    Xl2,
    Xl3,
    Xl4,
    Xl5,
    Xl6,
    Xl7,
    Xl8,
    Xl9,
}

/// Font-weight scale (100..900).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum FontWeightStep {
    Thin,
    ExtraLight,
    Light,
    Normal,
    Medium,
    SemiBold,
    Bold,
    ExtraBold,
    Black,
}

/// Line-height (leading) scale.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum LineHeightStep {
    None,
    Tight,
    Snug,
    Normal,
    Relaxed,
    Loose,
}

/// Letter-spacing (tracking) scale.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum LetterSpacingStep {
    Tighter,
    Tight,
    Normal,
    Wide,
    Wider,
    Widest,
}

/// Box-shadow scale.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum ShadowStep {
    None,
    Sm,
    Base,
    Md,
    Lg,
    Xl,
    Xl2,
    Inner,
}

/// Border-width scale (logical px steps).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum BorderWidthStep {
    B0,
    B1,
    B2,
    B4,
    B8,
}

/// Opacity scale (percent steps).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum OpacityStep {
    O0,
    O5,
    O10,
    O20,
    O25,
    O30,
    O40,
    O50,
    O60,
    O70,
    O75,
    O80,
    O90,
    O95,
    O100,
}

/// Stacking (z-index) scale.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum ZIndexStep {
    Z0,
    Z10,
    Z20,
    Z30,
    Z40,
    Z50,
    Auto,
}

/// Blur scale.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum BlurStep {
    None,
    Sm,
    Base,
    Md,
    Lg,
    Xl,
    Xl2,
    Xl3,
}

/// Semantic color role (upper token layer, §3.4/§3.5). Public components reference roles, not raw
/// palette colors, so a theme swap reskins everything by remapping the role table.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum ColorRole {
    Surface,
    SurfaceRaised,
    SurfaceOverlay,
    Bg,
    Text,
    TextMuted,
    TextSubtle,
    Border,
    BorderStrong,
    Accent,
    AccentFg,
    Danger,
    Warning,
    Success,
    Info,
    FocusRing,
}

/// An erased reference to any single token. The element's [`Style`](crate::style::Style) keeps
/// type-specific fields; `TokenRef` is the uniform form the resolver (#40) and future dynamic
/// styling use, with `From<…Step>`/`From<ColorRole>` to build it without naming the variant.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum TokenRef {
    Spacing(SpacingStep),
    Radius(RadiusStep),
    FontSize(FontSizeStep),
    Shadow(ShadowStep),
    Color(ColorRole),
}

impl From<SpacingStep> for TokenRef {
    fn from(step: SpacingStep) -> Self {
        TokenRef::Spacing(step)
    }
}

impl From<RadiusStep> for TokenRef {
    fn from(step: RadiusStep) -> Self {
        TokenRef::Radius(step)
    }
}

impl From<FontSizeStep> for TokenRef {
    fn from(step: FontSizeStep) -> Self {
        TokenRef::FontSize(step)
    }
}

impl From<ShadowStep> for TokenRef {
    fn from(step: ShadowStep) -> Self {
        TokenRef::Shadow(step)
    }
}

impl From<ColorRole> for TokenRef {
    fn from(role: ColorRole) -> Self {
        TokenRef::Color(role)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_ref_from_spacing_step_should_tag_spacing() {
        assert_eq!(
            TokenRef::from(SpacingStep::S2),
            TokenRef::Spacing(SpacingStep::S2)
        );
    }

    #[test]
    fn token_ref_from_color_role_should_tag_color() {
        let r: TokenRef = ColorRole::Accent.into();
        assert_eq!(r, TokenRef::Color(ColorRole::Accent));
    }
}
