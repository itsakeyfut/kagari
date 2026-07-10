//! Runtime clipboard access for editable text (#298). The editor element (`text_edit`) reads/writes the OS
//! clipboard through a [`Clipboard`] provided in reactive context, so copy/paste is testable (an in-memory
//! mock impl in tests) and the OS backend ([`SystemClipboard`], via `arboard`) is swappable.

/// An OS-clipboard abstraction for text. Provided in reactive context
/// (`provide_context::<std::sync::Arc<dyn Clipboard>>(..)`) so an editor can `use_context` it; a headless
/// test provides an in-memory mock instead of the OS backend.
pub trait Clipboard: Send + Sync {
    /// The clipboard's current text, or `None` if empty / unavailable.
    fn get_text(&self) -> Option<String>;
    /// Replace the clipboard text (best-effort — a failure is silently ignored).
    fn set_text(&self, text: &str);
}

/// The OS clipboard via `arboard`. A unit struct (trivially `Send + Sync`); a fresh `arboard::Clipboard` is
/// opened per operation, so no non-`Send` handle is held and clipboard use stays on the calling (UI) thread.
/// Any error (e.g. a headless environment with no display) degrades to a no-op — copy/paste simply do
/// nothing rather than failing the edit.
pub struct SystemClipboard;

impl Clipboard for SystemClipboard {
    fn get_text(&self) -> Option<String> {
        arboard::Clipboard::new().ok()?.get_text().ok()
    }

    fn set_text(&self, text: &str) {
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(text.to_string());
        }
    }
}
