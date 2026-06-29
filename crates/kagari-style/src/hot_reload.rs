//! Dev theme hot-reload (#44, specs §3.7): watch a theme RON file and reload it on change.
//!
//! Gated behind the `hot-reload` feature. The watcher lives here (kagari-style owns [`Theme`] + RON
//! parsing) but is **winit-agnostic**: it hands each rebuilt [`Theme`] to a caller-supplied `sink`
//! closure. The sink runs on the watcher's background thread, so it must be `Send` and must not
//! touch UI state directly — the kagari-core App posts to the UI thread via the winit
//! `EventLoopProxy` and writes the reactive theme signal there (specs §1.8/§7.2).

use std::path::{Path, PathBuf};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::error::StyleError;
use crate::theme::Theme;

/// Upper bound on a theme RON file. A theme is small KB-scale data, so this guards a watched path
/// that accidentally points at a huge or binary file from reading gigabytes into memory on every
/// reload (the file is re-read on each save event).
const MAX_THEME_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Reads and parses a theme RON file. A read failure (or an over-size file) maps to
/// [`StyleError::ThemeFile`]; a malformed document to [`StyleError::ThemeParse`] (via
/// [`Theme::from_ron`]).
pub fn load_theme_file(path: &Path) -> Result<Theme, StyleError> {
    let size = std::fs::metadata(path)
        .map_err(|e| StyleError::ThemeFile(e.to_string()))?
        .len();
    if size > MAX_THEME_FILE_BYTES {
        return Err(StyleError::ThemeFile(format!(
            "theme file is {size} bytes, exceeding the {MAX_THEME_FILE_BYTES}-byte limit"
        )));
    }
    let ron = std::fs::read_to_string(path).map_err(|e| StyleError::ThemeFile(e.to_string()))?;
    Theme::from_ron(&ron)
}

/// Reloads the theme from `path` and hands it to `sink`. A read/parse error keeps the current theme
/// and logs `tracing::warn!` — it never panics and never calls `sink` (error tolerance, specs §3.7).
fn dispatch(path: &Path, sink: &(dyn Fn(Theme) + Send)) {
    match load_theme_file(path) {
        Ok(theme) => sink(theme),
        Err(e) => tracing::warn!(
            error = %e,
            path = %path.display(),
            "theme hot-reload failed; keeping current theme"
        ),
    }
}

/// A live theme-file watcher. Drop it to stop watching.
pub struct ThemeWatcher {
    // Held to keep the OS watch alive (dropping it unwatches). The reload runs inside the watcher's
    // own callback, so this struct intentionally exposes no methods beyond construction.
    _watcher: RecommendedWatcher,
}

impl ThemeWatcher {
    /// Watches `path`; on each modify/create event, reloads the theme and calls `sink(theme)`.
    ///
    /// `sink` runs on the watcher's background thread (hence `Send + 'static`) and must not mutate
    /// UI state directly — post to the UI thread (the App uses the winit `EventLoopProxy`). A reload
    /// that fails to read/parse keeps the current theme and warns (no panic; `sink` is not called).
    pub fn new(
        path: impl Into<PathBuf>,
        sink: impl Fn(Theme) + Send + 'static,
    ) -> Result<Self, StyleError> {
        let path = path.into();
        let watch_path = path.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) if is_reload_trigger(&event.kind) => dispatch(&path, &sink),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "theme watch error"),
            })
            .map_err(|e| StyleError::ThemeWatch(e.to_string()))?;
        watcher
            .watch(&watch_path, RecursiveMode::NonRecursive)
            .map_err(|e| StyleError::ThemeWatch(e.to_string()))?;
        Ok(Self { _watcher: watcher })
    }
}

/// Whether a watch event should trigger a reload. Editors save via modify or atomic
/// remove-and-create, so both `Modify` and `Create` count; access/metadata-only events do not.
fn is_reload_trigger(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Modify(_) | EventKind::Create(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::{ColorRole, SpacingStep};
    use kagari_base::Px;
    use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tempfile::NamedTempFile;

    const VALID: &str = r##"( primitives: ( colors: { "blue-500": "#3b82f6" }, spacing: { S4: 16.0 } ), roles: ( colors: { Accent: "blue-500" } ) )"##;

    /// A temp file holding `contents`; kept alive by the returned handle (drop deletes it).
    fn temp_with(contents: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("create temp file");
        f.write_all(contents.as_bytes()).expect("write temp file");
        f.flush().expect("flush temp file");
        f
    }

    #[test]
    fn load_theme_file_should_parse_valid_ron() {
        let f = temp_with(VALID);
        let theme = load_theme_file(f.path()).expect("valid RON loads");
        assert_eq!(theme.spacing(SpacingStep::S4), Some(Px(16.0)));
        assert_eq!(
            theme.roles.colors.get(&ColorRole::Accent),
            Some(&"blue-500".to_string())
        );
    }

    #[test]
    fn load_theme_file_should_error_on_invalid_ron() {
        let f = temp_with("( this is not valid ron");
        assert!(matches!(
            load_theme_file(f.path()),
            Err(StyleError::ThemeParse(_))
        ));
    }

    #[test]
    fn load_theme_file_should_error_on_missing_file() {
        // A unique temp dir guarantees the path does not exist and avoids cross-test name clashes.
        let dir = tempfile::tempdir().expect("temp dir");
        let missing = dir.path().join("missing.ron");
        assert!(matches!(
            load_theme_file(&missing),
            Err(StyleError::ThemeFile(_))
        ));
    }

    #[test]
    fn is_reload_trigger_should_match_modify_and_create_only() {
        // Modify/Create are file-content changes that warrant a reload; access/metadata and remove
        // events are not (a remove leaves nothing to parse).
        assert!(is_reload_trigger(&EventKind::Modify(ModifyKind::Any)));
        assert!(is_reload_trigger(&EventKind::Create(CreateKind::Any)));
        assert!(!is_reload_trigger(&EventKind::Access(AccessKind::Any)));
        assert!(!is_reload_trigger(&EventKind::Remove(RemoveKind::Any)));
        assert!(!is_reload_trigger(&EventKind::Other));
    }

    #[test]
    fn dispatch_should_call_sink_on_valid_theme() {
        let f = temp_with(VALID);
        let seen: Arc<Mutex<Vec<Theme>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let sink = move |theme: Theme| sink_seen.lock().unwrap().push(theme);

        dispatch(f.path(), &sink);

        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.len(),
            1,
            "a valid reload dispatches the theme to the sink"
        );
        assert_eq!(seen[0].spacing(SpacingStep::S4), Some(Px(16.0)));
    }

    #[test]
    fn dispatch_should_keep_current_and_not_call_sink_on_parse_error() {
        let f = temp_with("not valid ron @@@");
        let called = Arc::new(Mutex::new(false));
        let flag = Arc::clone(&called);
        let sink = move |_theme: Theme| *flag.lock().unwrap() = true;

        // Must not panic; a parse error keeps the current theme (sink left uncalled).
        dispatch(f.path(), &sink);

        assert!(
            !*called.lock().unwrap(),
            "a parse error must keep the current theme and not call the sink"
        );
    }
}
