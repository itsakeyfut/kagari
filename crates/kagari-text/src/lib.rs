#![forbid(unsafe_code)]
//! kagari-text — text shaping, rasterization, IME, and editing on cosmic-text/swash.

pub mod error;
pub mod font;
mod raster;
pub mod shape;

pub use error::TextError;
pub use font::FontDb;
pub use shape::{LineInfo, PlacedGlyph, ShapedText, TextStyle, TextSystem};

/// Re-exported so consumers can name the font id/weight types that appear in
/// `TextStyle`, `PlacedGlyph`, and `FontDb` (cosmic-text and kagari-text share one
/// `fontdb`).
pub use cosmic_text::fontdb;
