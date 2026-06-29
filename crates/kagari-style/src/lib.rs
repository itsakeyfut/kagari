#![forbid(unsafe_code)]
//! kagari-style — token system, Tailwind-style styling, theme, and hot-reload.
//!
//! The value-free token reference types (#38) — primitive scale steps (`SpacingStep`, `RadiusStep`,
//! …) and semantic color roles (`ColorRole`), plus the erased [`TokenRef`] — and the [`Style`]
//! record that `Styled` builder methods write. Tokens record a reference (e.g. `SpacingStep::S2`),
//! never a concrete value. The two-layer [`Theme`] (#39) is the data-driven RON token table that
//! resolution (#40) reads. This crate does not depend on kagari-render (compile isolation, design.md).

pub mod error;
// Dev theme hot-reload (#44): file watcher + RON loader. Optional `notify` dependency.
#[cfg(feature = "hot-reload")]
pub mod hot_reload;
// Inherent `impl Theme` resolution/validation methods only — no new public type to re-export.
mod resolve;
pub mod style;
pub mod styled;
pub mod theme;
pub mod token;

pub use error::StyleError;
#[cfg(feature = "hot-reload")]
pub use hot_reload::{ThemeWatcher, load_theme_file};
pub use style::{LayoutTokens, PaintTokens, Style};
pub use styled::Styled;
pub use theme::{HexColor, Primitives, SemanticRoles, Theme};
pub use token::{
    BlurStep, BorderWidthStep, ColorRole, FontSizeStep, FontWeightStep, LetterSpacingStep,
    LineHeightStep, OpacityStep, RadiusStep, ShadowStep, SpacingStep, TokenRef, ZIndexStep,
};
