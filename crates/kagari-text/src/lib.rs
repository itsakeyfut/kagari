#![forbid(unsafe_code)]
//! kagari-text — text shaping, rasterization, IME, and editing on cosmic-text/swash.

pub mod error;
pub mod font;
pub mod shape;

pub use error::TextError;
pub use font::FontDb;
pub use shape::{LineInfo, PlacedGlyph, ShapedText, TextStyle, TextSystem};
