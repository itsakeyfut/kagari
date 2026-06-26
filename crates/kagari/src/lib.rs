#![forbid(unsafe_code)]
//! kagari — facade crate.
//!
//! Re-exports the public API of the kagari crates from one entry point (added as
//! the API stabilizes). For now it also provides a dev-only tracing helper.

/// Install a dev/test `tracing` subscriber (fmt + `EnvFilter`).
///
/// Reads `RUST_LOG` (default `info`; per-frame hot-path spans live at `trace`, so
/// they stay off unless explicitly enabled). For examples and tests only —
/// applications install their own subscriber, and kagari library crates never
/// install one (see `docs/rules/logging.md`). Idempotent: a no-op if a subscriber
/// is already installed, so it is safe to call from multiple tests or examples.
#[cfg(feature = "dev")]
pub fn init_dev_tracing() {
    use tracing_subscriber::{EnvFilter, filter::LevelFilter, fmt};

    // `from_env_lossy` keeps the valid `RUST_LOG` directives and ignores malformed
    // ones, defaulting to `info` when `RUST_LOG` is unset.
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    // `try_init` (not `init`): non-panicking if a global subscriber already exists.
    let _ = fmt().with_env_filter(filter).try_init();
}

#[cfg(all(test, feature = "dev"))]
mod tests {
    use super::*;

    #[test]
    fn init_dev_tracing_should_be_idempotent() {
        // Calling it more than once must not panic (try_init is a no-op the 2nd time).
        init_dev_tracing();
        init_dev_tracing();
    }
}
