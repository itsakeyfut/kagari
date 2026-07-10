//! IME caret-area transport (#299): the per-window channel by which the focused editable leaf
//! publishes its caret rect (window coords) at paint, so the shell can point the OS IME candidate
//! window at it (`set_ime_cursor_area`) and toggle `set_ime_allowed` on text-field focus enter/leave.
//!
//! Mirrors [`AnchorHandle`](crate::element::AnchorHandle)'s "element publishes, consumer reads" shape
//! — a plain shared cell, not a reactive signal. It is a **per-frame-record sink** (RK-019): the shell
//! clears it before each paint, the focused editor writes it during paint, and the shell reads it after
//! the frame. A frame with no focused editor leaves it `None`, which the shell reads as "no text field
//! focused" → disable IME (so a blur / unmount-while-focused tears IME down for free).

use std::sync::{Arc, Mutex};

use kagari_base::Rect;

/// A clone-shared, per-window transport for the focused editor's caret area (#299). Provided into the
/// window's reactive context; the editable leaf captures it with `use_context` and publishes at paint.
#[derive(Clone, Default)]
pub struct ImeCaretArea {
    rect: Arc<Mutex<Option<Rect>>>,
}

impl ImeCaretArea {
    /// A new, empty transport.
    pub fn new() -> Self {
        Self::default()
    }

    /// The caret rect (window coords) published by the focused editor this frame, if any. Read by the
    /// shell after the frame to drive `set_ime_cursor_area` and toggle `set_ime_allowed`.
    pub fn get(&self) -> Option<Rect> {
        self.rect.lock().ok().and_then(|r| *r)
    }

    /// Publish the caret rect (window coords) — called by the focused editable leaf at paint. Crate-internal
    /// (the write face is the framework's; mirrors [`AnchorHandle::set`](crate::element::AnchorHandle)).
    pub(crate) fn set(&self, rect: Rect) {
        if let Ok(mut slot) = self.rect.lock() {
            *slot = Some(rect);
        }
    }

    /// Clear before a frame's paint — called by the shell. A frame with no focused editor then reads back
    /// `None`. Crate-internal (the write face is the framework's).
    pub(crate) fn clear(&self) {
        if let Ok(mut slot) = self.rect.lock() {
            *slot = None;
        }
    }
}
