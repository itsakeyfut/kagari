//! Abstract IME events (specs §5.2).
//!
//! kagari-core converts the platform's winit IME events into these, so kagari-text
//! never depends on winit. The editing layer (#25 onward) consumes `ImeEvent` to
//! render preedit and commit text.

/// An abstract IME event: a composing-text (preedit) update or a committed insertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImeEvent {
    /// The composing (preedit) text plus an optional selection range as byte offsets
    /// into `text`. `cursor == None` hides the cursor; an empty `text` clears preedit.
    Preedit {
        text: String,
        cursor: Option<(usize, usize)>,
    },
    /// Text to insert into the editor (the conversion was confirmed).
    Commit(String),
}
