//! Error type for kagari-style (#39).

/// A styling error. `#[non_exhaustive]` so variants can be added (e.g. resolution errors in #40)
/// without breaking downstream `match`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StyleError {
    /// A theme RON file failed to deserialize (syntax, type mismatch, or a malformed color).
    #[error("theme parse error: {0}")]
    ThemeParse(String),
}
