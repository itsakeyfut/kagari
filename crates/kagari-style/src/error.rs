//! Error type for kagari-style (#39).

use crate::token::ColorRole;

/// A styling error. `#[non_exhaustive]` so variants can be added without breaking downstream `match`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StyleError {
    /// A theme RON file failed to deserialize (syntax, type mismatch, or a malformed color).
    #[error("theme parse error: {0}")]
    ThemeParse(String),
    /// A theme does not resolve a semantic role to a palette color (unmapped, or its palette key is
    /// missing). Raised by [`Theme::validate`](crate::theme::Theme::validate) at load — not on the
    /// resolution hot path.
    #[error("theme does not define semantic role: {0:?}")]
    UnknownRole(ColorRole),
    /// A theme file could not be read from disk (e.g. missing path, permissions). Raised by the
    /// dev hot-reload loader (#44); the parsed-content failure is [`ThemeParse`](Self::ThemeParse).
    #[error("theme file read error: {0}")]
    ThemeFile(String),
    /// The dev hot-reload file watcher could not be set up (e.g. the path does not exist). Raised by
    /// `ThemeWatcher::new` (#44); a *later* reload that fails to parse keeps the current theme.
    #[error("theme watch error: {0}")]
    ThemeWatch(String),
}
