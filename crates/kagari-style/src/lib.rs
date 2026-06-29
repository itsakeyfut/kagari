#![forbid(unsafe_code)]
//! kagari-style — token system, Tailwind-style styling, theme, and hot-reload.
//!
//! So far (#38): the value-free token reference types — primitive scale steps (`SpacingStep`,
//! `RadiusStep`, …) and semantic color roles (`ColorRole`), plus the erased [`TokenRef`] — and the
//! [`Style`] record that `Styled` builder methods write and the resolver reads. Tokens record a
//! reference (e.g. `SpacingStep::S2`), never a concrete value; resolution against a `Theme` arrives
//! in #39/#40. This crate does not depend on kagari-render (compile isolation, design.md).

pub mod style;
pub mod token;

pub use style::{LayoutTokens, PaintTokens, Style};
pub use token::{
    BlurStep, BorderWidthStep, ColorRole, FontSizeStep, FontWeightStep, LetterSpacingStep,
    LineHeightStep, OpacityStep, RadiusStep, ShadowStep, SpacingStep, TokenRef, ZIndexStep,
};
