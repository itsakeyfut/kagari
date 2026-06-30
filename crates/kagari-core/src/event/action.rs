//! Actions and keymap (#50, specs §1.5): a named [`Action`], a [`KeyChord`] → `Action` [`Keymap`]
//! with key-context resolution, and the default keymap.
//!
//! A key unhandled by the focused element (#49) is resolved against the active [`KeyContext`] (the
//! context names along the focus chain, innermost-first) and the resulting `Action` is dispatched up
//! the focus chain to `on_action` handlers ([`dispatch_action`](super::dispatch_action)). MVP is the
//! `Action` enum + the default keymap; user-config rebinding, multi-key chords, and a command palette
//! are post-MVP. The focus-chain `KeyContext` *collection* + the live `key → resolve → dispatch` glue
//! land with the deferred live keyboard wiring (#49); this module is the resolvable engine they use.

use kagari_base::SharedString;

use crate::element::EventCx;
use crate::event::{KeyCode, Modifiers};

/// A named action dispatched to `on_action` handlers. Framework actions plus a `Named` escape for
/// app-defined ones (avio-editor / Legend add their own without editing this enum). `#[non_exhaustive]`
/// so framework actions can grow without a breaking change.
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Action {
    /// Move focus to the next focusable in tab order.
    FocusNext,
    /// Move focus to the previous focusable in tab order.
    FocusPrev,
    /// An application-defined action, identified by name.
    Named(SharedString),
}

/// A single key chord: a physical key plus the modifiers held. Multi-key chords (sequences) are
/// post-MVP.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyChord {
    pub code: KeyCode,
    pub modifiers: Modifiers,
}

impl KeyChord {
    pub fn new(code: KeyCode, modifiers: Modifiers) -> Self {
        Self { code, modifiers }
    }
}

/// The active key contexts along the focus chain, **innermost-first** (focused node → root). Built
/// directly for now; deriving it from each node's `.key_context(name)` along the focus chain lands
/// with the live keyboard wiring (#49-deferred).
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct KeyContext(Vec<SharedString>);

impl KeyContext {
    /// Builds a context from names ordered innermost-first.
    pub fn new(names: impl IntoIterator<Item = impl Into<SharedString>>) -> Self {
        Self(names.into_iter().map(Into::into).collect())
    }
}

/// One keymap entry: a chord → action binding, optionally scoped to a context (`None` = global,
/// always active).
struct Binding {
    context: Option<SharedString>,
    chord: KeyChord,
    action: Action,
}

/// A keymap: chord → action bindings resolved against the active [`KeyContext`]. [`Keymap::default`]
/// is the framework default (tab navigation); [`Keymap::new`] is empty (build a custom one with
/// [`bind`](Keymap::bind)).
pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Keymap {
    /// An empty keymap. For the framework defaults use [`Keymap::default`].
    pub fn new() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    /// Adds a binding. `context` `None` is global (always active); `Some(name)` matches only when
    /// `name` is in the active [`KeyContext`].
    pub fn bind(
        &mut self,
        context: Option<impl Into<SharedString>>,
        chord: KeyChord,
        action: Action,
    ) {
        self.bindings.push(Binding {
            context: context.map(Into::into),
            chord,
            action,
        });
    }

    /// Resolves `chord` in context `ctx`: among bindings whose chord matches and whose context is
    /// global or present in `ctx`, the **innermost** (smallest index in `ctx`) wins; global bindings
    /// are least specific. Returns the matched action, if any.
    pub fn resolve(&self, chord: KeyChord, ctx: &KeyContext) -> Option<Action> {
        self.bindings
            .iter()
            .filter(|b| {
                b.chord == chord
                    && match &b.context {
                        None => true,
                        Some(c) => ctx.0.contains(c),
                    }
            })
            // Innermost context = smallest index in `ctx`; global (`None`) ranks last.
            .min_by_key(|b| match &b.context {
                Some(c) => ctx.0.iter().position(|n| n == c).unwrap_or(usize::MAX),
                None => usize::MAX,
            })
            .map(|b| b.action.clone())
    }
}

impl Default for Keymap {
    /// The framework default keymap: `Tab` → [`Action::FocusNext`], `Shift+Tab` → [`Action::FocusPrev`]
    /// (both global). Tab navigation ties into the focus registry's `focus_next`/`focus_prev` (#49).
    fn default() -> Self {
        let mut km = Keymap::new();
        km.bind(
            None::<&str>,
            KeyChord::new(KeyCode::Tab, Modifiers::default()),
            Action::FocusNext,
        );
        km.bind(
            None::<&str>,
            KeyChord::new(
                KeyCode::Tab,
                Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
            ),
            Action::FocusPrev,
        );
        km
    }
}

/// A boxed action handler stored on an element (#50). Imperative (writes to signals), not a reactive
/// effect — runs as a resolved `Action` bubbles the focus chain.
pub(crate) type ActionHandler = Box<dyn FnMut(&Action, &mut EventCx)>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keymap_should_resolve_chord_to_action() {
        // The default keymap maps Tab → FocusNext and Shift+Tab → FocusPrev, in any context.
        let km = Keymap::default();
        let ctx = KeyContext::default();
        assert_eq!(
            km.resolve(KeyChord::new(KeyCode::Tab, Modifiers::default()), &ctx),
            Some(Action::FocusNext)
        );
        assert_eq!(
            km.resolve(
                KeyChord::new(
                    KeyCode::Tab,
                    Modifiers {
                        shift: true,
                        ..Modifiers::default()
                    }
                ),
                &ctx
            ),
            Some(Action::FocusPrev)
        );
        // An unbound chord resolves to nothing.
        assert_eq!(
            km.resolve(KeyChord::new(KeyCode::KeyA, Modifiers::default()), &ctx),
            None
        );
    }

    #[test]
    fn keymap_should_prefer_innermost_context() {
        // Same chord bound in "Editor" and globally; the innermost (Editor) wins when active, and the
        // global binding wins when no specific context is active.
        let chord = KeyChord::new(KeyCode::KeyS, Modifiers::default());
        let mut km = Keymap::new();
        km.bind(None::<&str>, chord, Action::Named("global-save".into()));
        km.bind(Some("Editor"), chord, Action::Named("editor-save".into()));

        assert_eq!(
            km.resolve(chord, &KeyContext::new(["Editor", "Global"])),
            Some(Action::Named("editor-save".into())),
            "the innermost (Editor) binding wins over the global one"
        );
        assert_eq!(
            km.resolve(chord, &KeyContext::default()),
            Some(Action::Named("global-save".into())),
            "with no active context, the global binding resolves"
        );
    }
}
