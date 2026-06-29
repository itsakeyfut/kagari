//! The [`Styled`] trait (#41): Tailwind-style builder methods that record **value-free** tokens
//! into a [`Style`] (§3.3) — `gap_2()` records `SpacingStep::S2`, never a px value; resolution
//! against a theme happens later (#40/#42).
//!
//! Implemented on **elements only** (not components — rules/design.md §1); `Div` gets it in #42.
//! The methods are macro-generated **default** methods that record a token and return `self`, so an
//! implementor supplies only [`style_mut`](Styled::style_mut). Method names are built by the
//! in-house [`kagari_macros::idents`] concat macro (pure `macro_rules!` cannot concatenate
//! identifiers like `gap` + `_2`). All static — reactive props are a separate path (#146).

use kagari_macros::idents;

use crate::style::Style;
use crate::token::{
    BorderWidthStep, ColorRole, FontSizeStep, FontWeightStep, OpacityStep, RadiusStep, ShadowStep,
    SpacingStep,
};

/// Generates the spacing setters for a single layout `field`: one `fn <field><suffix>(self) -> Self`
/// per `(suffix, variant)` step, recording `SpacingStep::<variant>` into `style_mut().layout.<field>`.
macro_rules! spacing_field {
    ( $field:ident, [ $( ($sfx:ident, $var:ident) ),* $(,)? ] ) => {
        idents! {
            $(
                fn [<$field $sfx>](mut self) -> Self {
                    self.style_mut().layout.$field = Some(SpacingStep::$var);
                    self
                }
            )*
        }
    };
}

/// Cross-products the spacing scale (written once) over the layout fields: for each field it
/// delegates to [`spacing_field!`], passing the scale list verbatim as one token tree. Two-stage
/// because a single `macro_rules!` `$()*` cannot mix two independent repetition counts (fields ×
/// steps); here the outer expansion repeats only over `fields` and the steps blob is opaque to it.
macro_rules! spacing_setters {
    ( steps: $steps:tt, fields: [ $( $field:ident ),* $(,)? ] $(,)? ) => {
        $(
            spacing_field!($field, $steps);
        )*
    };
}

/// Generates per-step setters for one `paint.<field>` of enum `$enum`, with method names
/// `<prefix><suffix>` recording `$enum::<variant>`.
macro_rules! paint_step_setters {
    ( $field:ident, $enum:ident, $prefix:ident, [ $( ($sfx:ident, $var:ident) ),* $(,)? ] ) => {
        idents! {
            $(
                fn [<$prefix $sfx>](mut self) -> Self {
                    self.style_mut().paint.$field = Some($enum::$var);
                    self
                }
            )*
        }
    };
}

/// Tailwind-style styling recorded as value-free tokens (§3.3). Implement on elements only; an
/// implementor provides [`style_mut`](Self::style_mut) and inherits all builder methods.
pub trait Styled: Sized {
    /// Mutable access to the element's recorded [`Style`] (the only required method).
    fn style_mut(&mut self) -> &mut Style;

    // Layout: spacing scale (gap / padding / margin / size), full Tailwind steps incl. fractional.
    spacing_setters! {
        steps: [
            (_0, S0), (_px, SPx), (_0p5, S0p5), (_1, S1), (_1p5, S1p5), (_2, S2), (_2p5, S2p5),
            (_3, S3), (_3p5, S3p5), (_4, S4), (_5, S5), (_6, S6), (_7, S7), (_8, S8), (_9, S9),
            (_10, S10), (_11, S11), (_12, S12), (_14, S14), (_16, S16), (_20, S20), (_24, S24),
            (_28, S28), (_32, S32), (_36, S36), (_40, S40), (_44, S44), (_48, S48), (_52, S52),
            (_56, S56), (_60, S60), (_64, S64), (_72, S72), (_80, S80), (_96, S96),
        ],
        fields: [
            gap, gap_x, gap_y,
            p, px, py, pt, pr, pb, pl,
            m, mx, my, mt, mr, mb, ml,
            w, h, min_w, max_w, min_h, max_h,
        ],
    }

    // Paint: per-step scales.
    paint_step_setters!(
        rounded,
        RadiusStep,
        rounded,
        [
            (_none, None),
            (_sm, Sm),
            (_md, Md),
            (_lg, Lg),
            (_xl, Xl),
            (_2xl, Xl2),
            (_3xl, Xl3),
            (_full, Full)
        ]
    );
    paint_step_setters!(
        shadow,
        ShadowStep,
        shadow,
        [
            (_none, None),
            (_sm, Sm),
            (_base, Base),
            (_md, Md),
            (_lg, Lg),
            (_xl, Xl),
            (_2xl, Xl2),
            (_inner, Inner)
        ]
    );
    paint_step_setters!(
        opacity,
        OpacityStep,
        opacity,
        [
            (_0, O0),
            (_5, O5),
            (_10, O10),
            (_20, O20),
            (_25, O25),
            (_30, O30),
            (_40, O40),
            (_50, O50),
            (_60, O60),
            (_70, O70),
            (_75, O75),
            (_80, O80),
            (_90, O90),
            (_95, O95),
            (_100, O100)
        ]
    );
    // `border_w_*` (width) — distinct from the `border(ColorRole)` color setter below.
    paint_step_setters!(
        border_width,
        BorderWidthStep,
        border_w,
        [(_0, B0), (_1, B1), (_2, B2), (_4, B4), (_8, B8)]
    );
    // Font size uses the Tailwind `text-*` prefix; distinct idents from the `text(ColorRole)` setter.
    paint_step_setters!(
        font_size,
        FontSizeStep,
        text,
        [
            (_xs, Xs),
            (_sm, Sm),
            (_base, Base),
            (_lg, Lg),
            (_xl, Xl),
            (_2xl, Xl2),
            (_3xl, Xl3),
            (_4xl, Xl4),
            (_5xl, Xl5),
            (_6xl, Xl6),
            (_7xl, Xl7),
            (_8xl, Xl8),
            (_9xl, Xl9)
        ]
    );
    paint_step_setters!(
        font_weight,
        FontWeightStep,
        font,
        [
            (_thin, Thin),
            (_extralight, ExtraLight),
            (_light, Light),
            (_normal, Normal),
            (_medium, Medium),
            (_semibold, SemiBold),
            (_bold, Bold),
            (_extrabold, ExtraBold),
            (_black, Black)
        ]
    );

    // Paint: semantic colors (take a role; the only setters with an argument).
    /// Sets the background color (semantic role).
    fn bg(mut self, role: ColorRole) -> Self {
        self.style_mut().paint.bg = Some(role);
        self
    }
    /// Sets the text color (semantic role).
    fn text(mut self, role: ColorRole) -> Self {
        self.style_mut().paint.text = Some(role);
        self
    }
    /// Sets the border color (semantic role).
    fn border(mut self, role: ColorRole) -> Self {
        self.style_mut().paint.border_color = Some(role);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Probe {
        style: Style,
    }
    impl Styled for Probe {
        fn style_mut(&mut self) -> &mut Style {
            &mut self.style
        }
    }

    #[test]
    fn gap_2_should_record_spacing_step() {
        assert_eq!(
            Probe::default().gap_2().style.layout.gap,
            Some(SpacingStep::S2)
        );
    }

    #[test]
    fn p_4_should_record_padding() {
        assert_eq!(Probe::default().p_4().style.layout.p, Some(SpacingStep::S4));
    }

    #[test]
    fn gap_0p5_should_record_fractional_step() {
        assert_eq!(
            Probe::default().gap_0p5().style.layout.gap,
            Some(SpacingStep::S0p5)
        );
    }

    #[test]
    fn rounded_lg_should_record_radius() {
        assert_eq!(
            Probe::default().rounded_lg().style.paint.rounded,
            Some(RadiusStep::Lg)
        );
    }

    #[test]
    fn bg_should_record_color_role() {
        assert_eq!(
            Probe::default().bg(ColorRole::Accent).style.paint.bg,
            Some(ColorRole::Accent)
        );
    }

    #[test]
    fn text_color_and_text_size_should_not_collide() {
        let probe = Probe::default().text(ColorRole::Text).text_xl();
        assert_eq!(probe.style.paint.text, Some(ColorRole::Text));
        assert_eq!(probe.style.paint.font_size, Some(FontSizeStep::Xl));
    }
}
