//! First-class text editing core (specs §5.3): the `TextBuffer` owns committed text,
//! the cursor and selection, command-coalesced undo/redo, the OS clipboard bridge
//! (arboard), and the embedded IME composition state (#25's `ImeState`).
//!
//! All offsets are byte offsets kept on `char` boundaries, so the multi-byte text that
//! moat #5 targets never panics a `String` slice. Vertical (up/down) movement needs the
//! shaped line layout and is post-MVP; horizontal char/word movement is here.

use std::ops::Range;

use kagari_base::Rect;

use crate::ime::{ImeEvent, ImeState, x_at_byte};
use crate::shape::ShapedText;

/// A cursor movement granularity for [`TextBuffer::move_cursor`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Movement {
    CharLeft,
    CharRight,
    WordLeft,
    WordRight,
}

/// A forward edit. `undo` inverts it (in reverse within a group); `redo` re-applies it.
#[derive(Clone, Debug)]
enum EditCommand {
    Insert {
        at: usize,
        text: String,
    },
    Delete {
        range: Range<usize>,
        removed: String,
    },
}

/// The editing core: committed text + cursor/selection + coalesced undo/redo + the
/// embedded IME composition state. Pure logic (no GPU), so it is fully unit-testable.
#[derive(Default)]
pub struct TextBuffer {
    text: String,
    /// Byte offset of the cursor (the active end of any selection), on a char boundary.
    cursor: usize,
    /// Normalized selection `start..end` (char boundaries); `None` when collapsed.
    selection: Option<Range<usize>>,
    /// Undo/redo stacks; each entry is one atomic group of forward commands.
    undo: Vec<Vec<EditCommand>>,
    redo: Vec<Vec<EditCommand>>,
    /// Whether a contiguous typing run is open to coalescing into the last undo group.
    /// Any non-typing edit (move, delete, paste, IME commit, undo/redo) is a barrier.
    coalescing: bool,
    ime: ImeState,
}

impl TextBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start with `text`, cursor at the end.
    pub fn with_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            ..Default::default()
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn selection(&self) -> Option<Range<usize>> {
        self.selection.clone()
    }

    /// The current uncommitted IME preedit (empty when not composing).
    pub fn preedit(&self) -> &str {
        self.ime.preedit()
    }

    /// The embedded IME state, e.g. for a widget to render the preedit (#25).
    pub fn ime(&self) -> &ImeState {
        &self.ime
    }

    /// Set the selection to `range`; the cursor moves to its end. The range is
    /// normalized (`start <= end`) and clamped to the text length, and a
    /// non-char-boundary range is rejected (no-op) — so a caller passing a stray
    /// byte offset (e.g. from a hit test) can never panic a later `String` slice.
    /// An empty range collapses the selection.
    pub fn set_selection(&mut self, range: Range<usize>) {
        let end = range.start.max(range.end).min(self.text.len());
        let start = range.start.min(range.end).min(end);
        if !self.text.is_char_boundary(start) || !self.text.is_char_boundary(end) {
            debug_assert!(false, "set_selection: non-char-boundary range {range:?}");
            return;
        }
        self.coalescing = false;
        self.cursor = end;
        self.selection = if start == end { None } else { Some(start..end) };
    }

    // --- editing -----------------------------------------------------------------

    /// Insert `s` at the cursor, replacing the selection if any. Consecutive plain
    /// inserts (typing) coalesce into one undo group; replacing a selection is its own.
    pub fn insert(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if self.selection.is_some() {
            self.replace_selection(s);
        } else {
            self.typed_insert(s);
        }
    }

    /// Delete the current selection (no-op if collapsed). Its own undo group.
    pub fn delete_selection(&mut self) {
        let Some(range) = self.selection.take() else {
            return;
        };
        let removed = self.text[range.clone()].to_string();
        self.text.replace_range(range.clone(), "");
        self.cursor = range.start;
        self.coalescing = false;
        self.redo.clear();
        self.undo.push(vec![EditCommand::Delete { range, removed }]);
    }

    fn typed_insert(&mut self, s: &str) {
        let at = self.cursor;
        self.text.insert_str(at, s);
        self.cursor = at + s.len();
        self.redo.clear();
        // Coalesce into the open typing group if it ends exactly at this insert.
        if self.coalescing {
            if let Some([EditCommand::Insert { at: last_at, text }]) =
                self.undo.last_mut().map(Vec::as_mut_slice)
            {
                if *last_at + text.len() == at {
                    text.push_str(s);
                    return;
                }
            }
        }
        self.undo.push(vec![EditCommand::Insert {
            at,
            text: s.to_string(),
        }]);
        self.coalescing = true;
    }

    fn replace_selection(&mut self, s: &str) {
        // Only reached from `insert` under an `is_some()` guard; the let-else keeps it
        // panic-free if that invariant ever changes. Indices are on char boundaries.
        let Some(range) = self.selection.take() else {
            return;
        };
        let removed = self.text[range.clone()].to_string();
        self.text.replace_range(range.clone(), "");
        let at = range.start;
        self.text.insert_str(at, s);
        self.cursor = at + s.len();
        self.coalescing = false;
        self.redo.clear();
        self.undo.push(vec![
            EditCommand::Delete {
                range: range.clone(),
                removed,
            },
            EditCommand::Insert {
                at,
                text: s.to_string(),
            },
        ]);
    }

    // --- cursor / selection ------------------------------------------------------

    /// Move the cursor by `by`. With `extend`, grow/shrink the selection from its
    /// anchor; otherwise collapse the selection. A movement is an undo-coalescing barrier.
    pub fn move_cursor(&mut self, by: Movement, extend: bool) {
        self.coalescing = false;
        let anchor = match (&self.selection, extend) {
            (Some(sel), true) => {
                if self.cursor == sel.start {
                    sel.end
                } else {
                    sel.start
                }
            }
            _ => self.cursor,
        };
        let target = self.compute_move(self.cursor, by);
        self.cursor = target;
        self.selection = if extend && anchor != target {
            Some(anchor.min(target)..anchor.max(target))
        } else {
            None
        };
    }

    fn compute_move(&self, i: usize, by: Movement) -> usize {
        match by {
            Movement::CharLeft => self.prev_boundary(i),
            Movement::CharRight => self.next_boundary(i),
            Movement::WordLeft => self.word_left(i),
            Movement::WordRight => self.word_right(i),
        }
    }

    fn prev_boundary(&self, i: usize) -> usize {
        self.text[..i]
            .chars()
            .next_back()
            .map_or(0, |c| i - c.len_utf8())
    }

    fn next_boundary(&self, i: usize) -> usize {
        self.text[i..]
            .chars()
            .next()
            .map_or(i, |c| i + c.len_utf8())
    }

    /// Previous word start: skip whitespace left, then the word run. Simple MVP rule
    /// (whitespace vs non-whitespace); Japanese morphological segmentation is post-MVP.
    fn word_left(&self, mut i: usize) -> usize {
        while let Some(c) = self.text[..i].chars().next_back() {
            if c.is_whitespace() {
                i -= c.len_utf8();
            } else {
                break;
            }
        }
        while let Some(c) = self.text[..i].chars().next_back() {
            if c.is_whitespace() {
                break;
            }
            i -= c.len_utf8();
        }
        i
    }

    /// Next word start: skip the word run right, then trailing whitespace.
    fn word_right(&self, mut i: usize) -> usize {
        while let Some(c) = self.text[i..].chars().next() {
            if c.is_whitespace() {
                break;
            }
            i += c.len_utf8();
        }
        while let Some(c) = self.text[i..].chars().next() {
            if !c.is_whitespace() {
                break;
            }
            i += c.len_utf8();
        }
        i
    }

    // --- clipboard ---------------------------------------------------------------

    /// The selected text, if any (pure; no OS clipboard).
    pub fn copy(&self) -> Option<String> {
        self.selection
            .as_ref()
            .map(|range| self.text[range.clone()].to_string())
    }

    /// Return and delete the selected text, if any (pure; no OS clipboard).
    pub fn cut(&mut self) -> Option<String> {
        let text = self.copy()?;
        self.delete_selection();
        Some(text)
    }

    /// Insert `s` at the cursor (replacing the selection) as its own undo group.
    pub fn paste(&mut self, s: &str) {
        // A paste is never part of a typing run, in either direction.
        self.coalescing = false;
        self.insert(s);
        self.coalescing = false;
    }

    /// Copy the selection to the OS clipboard (arboard). A missing/unavailable
    /// clipboard is logged and ignored — never panics (headless CI has no clipboard).
    pub fn clipboard_copy(&self) {
        let Some(text) = self.copy() else {
            return;
        };
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text)) {
            Ok(()) => {}
            Err(error) => tracing::warn!(%error, "clipboard copy failed"),
        }
    }

    /// Paste the OS clipboard text (arboard) at the cursor. Unavailable clipboard or a
    /// read error is logged and ignored.
    pub fn clipboard_paste(&mut self) {
        match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
            Ok(text) => self.paste(&text),
            Err(error) => tracing::warn!(%error, "clipboard paste failed"),
        }
    }

    // --- IME ---------------------------------------------------------------------

    /// Apply an IME event: `Preedit` updates the embedded composition state (rendered
    /// by a widget via [`Self::ime`]); `Commit` inserts the confirmed text as its own
    /// undo group.
    pub fn apply_ime(&mut self, event: ImeEvent) {
        if let Some(committed) = self.ime.apply(event) {
            // An empty commit (some IMEs send one on cancel) only clears the preedit
            // (done above); don't break an in-flight typing run with a no-op insert.
            if committed.is_empty() {
                return;
            }
            self.coalescing = false;
            self.insert(&committed);
            self.coalescing = false;
        }
    }

    // --- undo / redo -------------------------------------------------------------

    /// Revert the most recent edit group.
    pub fn undo(&mut self) {
        self.coalescing = false;
        let Some(group) = self.undo.pop() else {
            return;
        };
        for command in group.iter().rev() {
            match command {
                EditCommand::Insert { at, text } => {
                    self.text.replace_range(*at..at + text.len(), "");
                    self.cursor = *at;
                }
                EditCommand::Delete { range, removed } => {
                    self.text.insert_str(range.start, removed);
                    self.cursor = range.start + removed.len();
                }
            }
        }
        self.selection = None;
        self.redo.push(group);
    }

    /// Re-apply the most recently undone edit group.
    pub fn redo(&mut self) {
        self.coalescing = false;
        let Some(group) = self.redo.pop() else {
            return;
        };
        for command in &group {
            match command {
                EditCommand::Insert { at, text } => {
                    self.text.insert_str(*at, text);
                    self.cursor = at + text.len();
                }
                EditCommand::Delete { range, .. } => {
                    self.text.replace_range(range.clone(), "");
                    self.cursor = range.start;
                }
            }
        }
        self.selection = None;
        self.undo.push(group);
    }

    // --- geometry ----------------------------------------------------------------

    /// The caret area (logical px, relative to the text origin) at the **committed**
    /// cursor, for the OS IME candidate window. While composing (`ime().is_composing()`),
    /// position the preedit via [`ImeState::emit_preedit`]/[`ImeState::caret_rect`] with
    /// this rect's origin as the preedit origin; use this method directly only when not
    /// composing. Single-line MVP (multi-line vertical placement needs the line index
    /// and is post-MVP).
    pub fn caret_rect(&self, shaped: &ShapedText) -> Rect {
        let height = shaped.lines.first().map_or(0.0, |line| line.height);
        Rect::from_xywh(x_at_byte(shaped, self.cursor), 0.0, 1.0, height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FontDb, TextStyle, TextSystem, fontdb};
    use kagari_base::Px;

    #[test]
    fn insert_should_advance_cursor() {
        let mut buffer = TextBuffer::new();
        buffer.insert("hi");
        assert_eq!(buffer.text(), "hi");
        assert_eq!(buffer.cursor(), 2);
    }

    #[test]
    fn delete_selection_should_remove_range() {
        let mut buffer = TextBuffer::with_text("hello");
        buffer.set_selection(1..4); // "ell"
        buffer.delete_selection();
        assert_eq!(buffer.text(), "ho");
        assert_eq!(buffer.cursor(), 1);
        assert_eq!(buffer.selection(), None);
    }

    #[test]
    fn undo_should_revert_last_group() {
        let mut buffer = TextBuffer::with_text("ab");
        buffer.set_selection(0..2);
        buffer.delete_selection();
        assert_eq!(buffer.text(), "");
        buffer.undo();
        assert_eq!(buffer.text(), "ab");
    }

    #[test]
    fn typing_run_should_coalesce_into_one_undo() {
        let mut buffer = TextBuffer::new();
        for ch in ["h", "e", "l", "l", "o"] {
            buffer.insert(ch);
        }
        assert_eq!(buffer.text(), "hello");
        // A single undo reverts the whole typed run (coalesced into one group).
        buffer.undo();
        assert_eq!(buffer.text(), "");
    }

    #[test]
    fn clipboard_round_trip_should_restore_text() {
        // Pure copy → paste (no OS clipboard, so this is deterministic in CI).
        let mut buffer = TextBuffer::with_text("日本語");
        buffer.set_selection(0..9); // all three kanji (3 bytes each)
        let copied = buffer.cut().expect("selection copied");
        assert_eq!(buffer.text(), "");
        buffer.paste(&copied);
        assert_eq!(buffer.text(), "日本語");
    }

    #[test]
    fn redo_should_reapply_undone_group() {
        let mut buffer = TextBuffer::new();
        buffer.insert("x");
        buffer.undo();
        assert_eq!(buffer.text(), "");
        buffer.redo();
        assert_eq!(buffer.text(), "x");
    }

    #[test]
    fn move_cursor_word_should_skip_to_boundary() {
        let mut buffer = TextBuffer::with_text("hello world"); // cursor at 11
        buffer.move_cursor(Movement::WordLeft, false);
        assert_eq!(buffer.cursor(), 6); // start of "world"
        buffer.move_cursor(Movement::WordLeft, false);
        assert_eq!(buffer.cursor(), 0); // start of "hello"
    }

    #[test]
    fn move_cursor_extend_should_build_selection() {
        let mut buffer = TextBuffer::with_text("abc");
        buffer.set_selection(0..0); // collapse cursor at 0
        buffer.move_cursor(Movement::CharRight, true);
        buffer.move_cursor(Movement::CharRight, true);
        assert_eq!(buffer.selection(), Some(0..2));
        assert_eq!(buffer.cursor(), 2);
    }

    #[test]
    fn apply_ime_commit_should_insert_undoably() {
        let mut buffer = TextBuffer::new();
        buffer.apply_ime(ImeEvent::Preedit {
            text: "にほん".into(),
            cursor: Some((0, 3)),
        });
        assert_eq!(buffer.text(), "", "preedit is not committed text");
        assert_eq!(buffer.preedit(), "にほん");
        buffer.apply_ime(ImeEvent::Commit("日本".into()));
        assert_eq!(buffer.text(), "日本");
        assert_eq!(buffer.preedit(), "");
        buffer.undo();
        assert_eq!(buffer.text(), "");
    }

    #[test]
    fn insert_over_selection_should_replace_as_one_undo() {
        let mut buffer = TextBuffer::with_text("hello");
        buffer.set_selection(1..4); // "ell"
        buffer.insert("X");
        assert_eq!(buffer.text(), "hXo");
        buffer.undo();
        assert_eq!(buffer.text(), "hello", "replace is a single undo group");
    }

    #[test]
    fn caret_rect_should_sit_at_cursor() {
        // shape() is GPU-free, so this needs no device.
        let mut system = TextSystem::new(FontDb::new());
        let style = TextStyle {
            family: "Noto Sans".into(),
            size: Px(16.0),
            weight: fontdb::Weight::NORMAL,
            line_height: None,
        };
        let shaped = system.shape("abc", &style, None);
        let mut buffer = TextBuffer::with_text("abc");
        buffer.move_cursor(Movement::CharLeft, false); // cursor at byte 2
        let caret = buffer.caret_rect(&shaped);
        assert_eq!(caret.origin.x, x_at_byte(&shaped, 2));
        assert!(caret.size.h > 0.0);
    }

    #[test]
    fn typing_after_move_should_start_new_undo_group() {
        // A cursor move is a coalescing barrier: the two runs are separate undo groups.
        let mut buffer = TextBuffer::new();
        buffer.insert("ab");
        buffer.move_cursor(Movement::CharRight, false); // barrier (no-op move at end)
        buffer.insert("cd");
        assert_eq!(buffer.text(), "abcd");
        buffer.undo();
        assert_eq!(buffer.text(), "ab", "only the post-barrier run is reverted");
    }

    #[test]
    fn redo_should_reapply_replaced_selection_group() {
        // A replace is a 2-command group (Delete + Insert); redo must re-apply both.
        let mut buffer = TextBuffer::with_text("hello");
        buffer.set_selection(1..4); // "ell"
        buffer.insert("X");
        buffer.undo();
        assert_eq!(buffer.text(), "hello");
        buffer.redo();
        assert_eq!(buffer.text(), "hXo");
        assert_eq!(buffer.cursor(), 2);
    }

    #[test]
    fn move_cursor_word_should_handle_multibyte() {
        // Word movement must land on char boundaries with multi-byte text ("あ"/"い"
        // are 3 bytes each; the space is 1): "あ い" → byte layout 0..3 "あ", 3 " ", 4..7 "い".
        let mut buffer = TextBuffer::with_text("あ い"); // cursor at 7
        buffer.move_cursor(Movement::WordLeft, false);
        assert_eq!(buffer.cursor(), 4, "start of \"い\"");
        buffer.move_cursor(Movement::WordLeft, false);
        assert_eq!(buffer.cursor(), 0, "start of \"あ\"");
        buffer.move_cursor(Movement::WordRight, false);
        assert_eq!(buffer.cursor(), 4, "past \"あ\" and the space");
    }

    #[test]
    fn clipboard_copy_should_not_panic_without_clipboard() {
        // The OS round-trip can't be tested deterministically (headless CI has no
        // clipboard; clipboard_paste depends on global state), but the graceful path
        // must never panic and must not mutate the buffer.
        let mut buffer = TextBuffer::with_text("abc");
        buffer.set_selection(0..3);
        buffer.clipboard_copy();
        assert_eq!(buffer.text(), "abc");
    }
}
