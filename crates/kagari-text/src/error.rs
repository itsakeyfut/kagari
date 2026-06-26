//! Error types for kagari-text (specs §5; thiserror, no anyhow in library crates).

/// Errors from font resolution, shaping, and text editing.
#[derive(Debug, thiserror::Error)]
pub enum TextError {
    /// No face matched the requested family (after bundled + system fonts).
    #[error("no font matched for: {0:?}")]
    NoFontMatch(String),
    /// cosmic-text shaping failed (used from #21 onward).
    #[error("shaping failed: {0}")]
    Shaping(String),
}
