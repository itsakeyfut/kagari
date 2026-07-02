//! Persistence service (#68, specs §7.3): read/write app state as **RON** in the OS config dir
//! (`directories`, Windows `%APPDATA%`) — window geometry, theme selection, and keymap overrides,
//! restored on restart. Provided as a context service (§1.8) so app code (recent files, etc.) shares
//! it. A missing/corrupt file falls back to defaults (warn, never panic); write failures surface
//! [`AppError::Persist`] and are logged, not fatal (§1.11).
//!
//! The framework auto-captures **window geometry**; theme + keymap overrides are applied on load and
//! set by the consumer via this service (there is no built-in theme switcher / rebinding UI yet).
//! Dock/panel layout persistence and multi-profile support are post-MVP.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use kagari_base::{Point, SharedString, Size};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::event::{Action, KeyChord, Keymap};

/// A persisted window's position + size (logical px). Keyed by open-order index in
/// [`PersistedState::windows`] (robust id/role keying is post-MVP).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub position: Point,
    pub size: Size,
}

/// One persisted keymap override: a chord → action binding, optionally context-scoped (mirrors
/// [`Keymap::bind`]'s parameters). [`KeymapOverrides::merge_into`] replays these onto a `Keymap`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverrideBinding {
    pub context: Option<SharedString>,
    pub chord: KeyChord,
    pub action: Action,
}

/// The user's keymap overrides, layered onto the framework default keymap (§1.5).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KeymapOverrides(pub Vec<OverrideBinding>);

impl KeymapOverrides {
    /// Applies each override to `keymap` via [`Keymap::bind`]. Appended after the framework defaults,
    /// so on the same chord + context an override wins (the keymap resolves ties last-bound-first,
    /// RK-023) — layering the user's overrides over the defaults.
    pub fn merge_into(&self, keymap: &mut Keymap) {
        for b in &self.0 {
            keymap.bind(b.context.clone(), b.chord, b.action.clone());
        }
    }
}

/// The full persisted framework state. Every field is `#[serde(default)]` so an older / partial RON
/// file still loads (forward-compatible), filling missing fields with defaults.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PersistedState {
    /// Per-window geometry, indexed by window open order.
    #[serde(default)]
    pub windows: Vec<WindowGeometry>,
    /// The selected theme id (e.g. `"light"`/`"dark"`; arbitrary ids are stored but only built-ins
    /// are applied until a theme registry exists).
    #[serde(default)]
    pub theme: Option<SharedString>,
    /// User keymap overrides layered onto the default keymap.
    #[serde(default)]
    pub keymap_overrides: KeymapOverrides,
}

/// Shared inner state behind the cloneable [`PersistenceService`] handle.
struct Inner {
    /// The RON file path, or `None` when no OS config dir could be resolved (then saves are no-ops).
    path: Option<PathBuf>,
    state: Mutex<PersistedState>,
    /// Set on any [`with`](PersistenceService::with); cleared on a successful [`save`](PersistenceService::save).
    dirty: AtomicBool,
    /// When the last change happened, for the app's debounce (coalesce rapid writes, #68).
    last_change: Mutex<Option<Instant>>,
}

/// A cloneable, `Send + Sync` handle to the persisted state — provided as a context service (§1.8).
/// Clones share one underlying state; mutate via [`with`](Self::with), read via [`snapshot`](Self::snapshot).
#[derive(Clone)]
pub struct PersistenceService {
    inner: Arc<Inner>,
}

impl PersistenceService {
    /// Loads from the OS config dir's `state.ron` (`directories`), or defaults if unavailable/missing.
    pub fn load() -> Self {
        Self::from_path(Self::default_path())
    }

    /// Loads from an explicit RON file path — for tests (a temp dir, never the real config dir) or a
    /// caller-chosen location.
    pub fn load_from(path: PathBuf) -> Self {
        Self::from_path(Some(path))
    }

    /// An in-memory service with no backing file: default state, saves are no-ops. For tests (avoids
    /// touching the real OS config dir) and for a consumer that wants persistence disabled.
    pub fn in_memory() -> Self {
        Self::from_path(None)
    }

    fn from_path(path: Option<PathBuf>) -> Self {
        let state = match &path {
            Some(p) => Self::read_ron(p),
            None => PersistedState::default(),
        };
        Self {
            inner: Arc::new(Inner {
                path,
                state: Mutex::new(state),
                dirty: AtomicBool::new(false),
                last_change: Mutex::new(None),
            }),
        }
    }

    /// The default RON path: `<OS config dir>/state.ron`, the app segment derived from the executable
    /// name (so each app gets its own dir). `None` if no config dir is available (headless / sandbox).
    fn default_path() -> Option<PathBuf> {
        let app = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "kagari".to_string());
        let dirs = directories::ProjectDirs::from("", "kagari", &app)?;
        Some(dirs.config_dir().join("state.ron"))
    }

    /// Reads + parses the RON at `path`. A missing file is a silent default; a read error or corrupt
    /// RON warns and falls back to default — never panics (§1.11).
    fn read_ron(path: &Path) -> PersistedState {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return PersistedState::default(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "failed to read persisted state; using defaults");
                return PersistedState::default();
            }
        };
        match ron::from_str::<PersistedState>(&text) {
            Ok(state) => state,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "corrupt persisted state; using defaults");
                PersistedState::default()
            }
        }
    }

    /// A clone of the current state (for applying on load: geometry/theme/keymap). Recovers a poisoned
    /// lock (`into_inner`) rather than returning defaults, so a prior unrelated panic can't make
    /// [`save`](Self::save) overwrite real state with an empty default.
    pub fn snapshot(&self) -> PersistedState {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Mutates the state in place, marking it dirty and stamping the change time (for the debounce).
    /// Use it to record window geometry, the selected theme, recent files, etc. Recovers a poisoned
    /// lock so the mutation isn't silently dropped.
    pub fn with(&self, f: impl FnOnce(&mut PersistedState)) {
        {
            let mut s = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut s);
        }
        self.inner.dirty.store(true, Ordering::SeqCst);
        let mut lc = self
            .inner
            .last_change
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *lc = Some(Instant::now());
    }

    /// Whether there are unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.inner.dirty.load(Ordering::SeqCst)
    }

    /// When the last change occurred (for the app's debounce window); `None` if unchanged since load.
    pub fn last_change(&self) -> Option<Instant> {
        *self
            .inner
            .last_change
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Writes the current state to the RON file (creating parent dirs). Clears the dirty flag on
    /// success. A `None` path (no config dir) is a successful no-op. Errors surface [`AppError::Persist`].
    ///
    /// This does blocking disk I/O; the app calls it from the UI thread's `about_to_wait`, debounced
    /// to ~once per settle so it rarely runs. Offloading the write to a worker thread is post-MVP.
    pub fn save(&self) -> Result<(), AppError> {
        let Some(path) = self.inner.path.clone() else {
            self.inner.dirty.store(false, Ordering::SeqCst);
            return Ok(());
        };
        let state = self.snapshot();
        let text = ron::ser::to_string_pretty(&state, ron::ser::PrettyConfig::default())
            .map_err(|e| AppError::Persist(e.to_string()))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| AppError::Persist(e.to_string()))?;
        }
        std::fs::write(&path, text).map_err(|e| AppError::Persist(e.to_string()))?;
        self.inner.dirty.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Saves if dirty, logging (not propagating) any error — the final on-exit save (§1.11).
    pub fn flush(&self) {
        if self.is_dirty() {
            if let Err(e) = self.save() {
                tracing::error!(error = %e, "failed to flush persisted state on exit");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{KeyCode, KeyContext, Modifiers};

    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        }
    }

    #[test]
    fn persist_roundtrip_should_restore_state() {
        let state = PersistedState {
            windows: vec![WindowGeometry {
                position: Point::new(10.0, 20.0),
                size: Size { w: 800.0, h: 600.0 },
            }],
            theme: Some("dark".into()),
            keymap_overrides: KeymapOverrides(vec![OverrideBinding {
                context: Some("Editor".into()),
                chord: KeyChord::new(KeyCode::KeyS, ctrl()),
                action: Action::Named("save".into()),
            }]),
        };
        let text = ron::ser::to_string_pretty(&state, ron::ser::PrettyConfig::default())
            .expect("serialize");
        let back: PersistedState = ron::from_str(&text).expect("deserialize");
        assert_eq!(state, back, "state round-trips through RON");
    }

    /// A process-unique temp dir for a test, so concurrent `cargo test` runs (or worktrees) don't
    /// share a fixed path. Caller removes it at the end.
    fn unique_temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kagari-persist-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn persist_corrupt_input_should_fall_back_to_default() {
        let dir = unique_temp_dir("corrupt");
        let path = dir.join("state.ron");
        std::fs::write(&path, b"this is not valid RON {{{").expect("write garbage");

        let svc = PersistenceService::load_from(path.clone());
        assert_eq!(
            svc.snapshot(),
            PersistedState::default(),
            "a corrupt file falls back to default (no panic)"
        );

        let missing = dir.join("does-not-exist.ron");
        let svc2 = PersistenceService::load_from(missing);
        assert_eq!(
            svc2.snapshot(),
            PersistedState::default(),
            "a missing file falls back to default"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keymap_overrides_should_merge_into_keymap() {
        let chord = KeyChord::new(KeyCode::KeyS, ctrl());
        let overrides = KeymapOverrides(vec![OverrideBinding {
            context: None,
            chord,
            action: Action::Named("save".into()),
        }]);
        let mut km = Keymap::new();
        overrides.merge_into(&mut km);
        assert_eq!(
            km.resolve(chord, &KeyContext::default()),
            Some(Action::Named("save".into())),
            "a merged override resolves to its action"
        );
    }

    #[test]
    fn persist_save_should_reload_via_file() {
        let dir = unique_temp_dir("save");
        let path = dir.join("state.ron");

        let svc = PersistenceService::load_from(path.clone());
        svc.with(|s| {
            s.theme = Some("dark".into());
            s.windows.push(WindowGeometry {
                position: Point::new(1.0, 2.0),
                size: Size { w: 3.0, h: 4.0 },
            });
        });
        assert!(svc.is_dirty(), "a change marks the service dirty");
        svc.save().expect("save");
        assert!(!svc.is_dirty(), "a successful save clears dirty");

        let reloaded = PersistenceService::load_from(path.clone());
        assert_eq!(reloaded.snapshot().theme, Some("dark".into()));
        assert_eq!(reloaded.snapshot().windows.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
