//! First-class text editing core (specs §5.3): the `TextBuffer` owns committed text,
//! the cursor and selection, command-coalesced undo/redo, a runtime-agnostic clipboard
//! hook (pure `copy`/`cut`/`paste` — the app supplies the OS clipboard, §7.2), and the
//! embedded IME composition state (#25's `ImeState`).
//!
//! Cursor/motion/delete snap to **grapheme-cluster boundaries** (combining chars stay
//! intact; multi-byte text never panics a `String` slice — moat #5). Horizontal char/word
//! and line/home/end motion is byte-based; vertical (up/down) motion uses the shaped line
//! layout (#220).

use std::ops::Range;

use kagari_base::Rect;
use unicode_segmentation::UnicodeSegmentation;

use crate::ime::{ImeEvent, ImeState, x_at_byte};
use crate::shape::{ShapedText, byte_at_x, line_of_byte};

/// A horizontal cursor movement granularity for [`TextBuffer::move_cursor`]. `Char*` moves by grapheme
/// cluster; vertical (up/down) motion is [`TextBuffer::move_vertical`] (it needs the shaped line layout).
///
/// `#[non_exhaustive]`: motion is a growth area (more granularities are likely), so variants can be added
/// without a breaking change (matching kagari's other growth-area enums like the core's `CursorIcon`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Movement {
    CharLeft,
    CharRight,
    WordLeft,
    WordRight,
    /// To the start of the current line (after the previous `\n`, or the buffer start).
    LineStart,
    /// To the end of the current line (before the next `\n`, or the buffer end).
    LineEnd,
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
    /// The caret x preserved across consecutive vertical moves (the "goal column", #220): set on the
    /// first [`move_vertical`](Self::move_vertical), read by later ones, cleared by any other operation.
    goal_x: Option<f32>,
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
        self.goal_x = None;
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
        if let Some(range) = self.selection.take() {
            self.delete_range(range);
        }
    }

    /// Delete the grapheme cluster before the cursor (Backspace), or the selection if any. Its own group.
    pub fn delete_backward(&mut self) {
        if self.selection.is_some() {
            self.delete_selection();
            return;
        }
        let start = self.prev_boundary(self.cursor);
        if start < self.cursor {
            self.delete_range(start..self.cursor);
        }
    }

    /// Delete the grapheme cluster after the cursor (Delete), or the selection if any. Its own group.
    pub fn delete_forward(&mut self) {
        if self.selection.is_some() {
            self.delete_selection();
            return;
        }
        let end = self.next_boundary(self.cursor);
        if end > self.cursor {
            self.delete_range(self.cursor..end);
        }
    }

    /// Remove `range` (grapheme/char boundaries), collapse the cursor to its start, as one undo group.
    fn delete_range(&mut self, range: Range<usize>) {
        let removed = self.text[range.clone()].to_string();
        self.text.replace_range(range.clone(), "");
        self.cursor = range.start;
        self.selection = None;
        self.coalescing = false;
        self.goal_x = None;
        self.redo.clear();
        self.undo.push(vec![EditCommand::Delete { range, removed }]);
    }

    /// Select the whole buffer; the cursor moves to the end.
    pub fn select_all(&mut self) {
        self.coalescing = false;
        self.goal_x = None;
        self.cursor = self.text.len();
        self.selection = if self.text.is_empty() {
            None
        } else {
            Some(0..self.text.len())
        };
    }

    /// The selected text as a borrowed slice, or `None` when the selection is collapsed. Complements
    /// [`copy`](Self::copy) (which allocates for the clipboard hook).
    pub fn selected_text(&self) -> Option<&str> {
        self.selection
            .as_ref()
            .map(|range| &self.text[range.clone()])
    }

    /// Replace the whole buffer with `text`: cursor at the end, selection cleared, undo history reset —
    /// a programmatic set (e.g. a controlled value binding), not an undoable edit.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.selection = None;
        self.coalescing = false;
        self.goal_x = None;
        self.undo.clear();
        self.redo.clear();
    }

    fn typed_insert(&mut self, s: &str) {
        self.goal_x = None;
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
        self.goal_x = None;
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
        self.goal_x = None;
        let anchor = self.extend_anchor(extend);
        let target = self.compute_move(self.cursor, by);
        self.cursor = target;
        self.set_cursor_selection(anchor, target, extend);
    }

    /// Move the cursor up or down one visual line using the shaped layout (#220), preserving the caret's
    /// x (the "goal column") across consecutive vertical moves. With `extend`, grow/shrink the selection
    /// from its anchor; otherwise collapse it. At the first/last line this clamps within the buffer.
    pub fn move_vertical(&mut self, down: bool, extend: bool, shaped: &ShapedText) {
        self.coalescing = false;
        let anchor = self.extend_anchor(extend);
        // Establish (or reuse) the goal column from the current caret x — the one field a vertical move
        // does *not* clear, so a run of up/down keeps the column.
        let goal = match self.goal_x {
            Some(g) => g,
            None => {
                let g = x_at_byte(shaped, self.cursor);
                self.goal_x = Some(g);
                g
            }
        };
        let line = line_of_byte(shaped, self.cursor);
        let target_line = if down {
            (line + 1).min(shaped.lines.len().saturating_sub(1))
        } else {
            line.saturating_sub(1)
        };
        let target = if target_line == line {
            self.cursor // already at the top/bottom edge
        } else {
            byte_at_x(shaped, target_line, goal).min(self.text.len())
        };
        self.cursor = target;
        self.set_cursor_selection(anchor, target, extend);
    }

    /// The selection anchor for an extend move: the fixed (non-cursor) end of the current selection, or
    /// the cursor when there is none / not extending.
    fn extend_anchor(&self, extend: bool) -> usize {
        match (&self.selection, extend) {
            (Some(sel), true) if self.cursor == sel.start => sel.end,
            (Some(sel), true) => sel.start,
            _ => self.cursor,
        }
    }

    /// Apply the selection after a cursor move to `target`: a range from `anchor` when extending (and the
    /// two differ), else collapsed.
    fn set_cursor_selection(&mut self, anchor: usize, target: usize, extend: bool) {
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
            Movement::LineStart => self.line_start(i),
            Movement::LineEnd => self.line_end(i),
        }
    }

    /// The byte offset of the previous grapheme-cluster boundary before `i` (or `0` if at the start).
    /// Grapheme-aware so a combining sequence (e.g. `e` + U+0301) moves/deletes as one cluster.
    fn prev_boundary(&self, i: usize) -> usize {
        self.text[..i]
            .graphemes(true)
            .next_back()
            .map_or(0, |g| i - g.len())
    }

    /// The byte offset of the next grapheme-cluster boundary after `i` (or `i` if at the end).
    fn next_boundary(&self, i: usize) -> usize {
        self.text[i..]
            .graphemes(true)
            .next()
            .map_or(i, |g| i + g.len())
    }

    /// The start of the line containing `i`: just after the previous `\n`, or the buffer start.
    fn line_start(&self, i: usize) -> usize {
        self.text[..i].rfind('\n').map_or(0, |nl| nl + 1)
    }

    /// The end of the line containing `i`: just before the next `\n`, or the buffer end.
    fn line_end(&self, i: usize) -> usize {
        self.text[i..]
            .find('\n')
            .map_or(self.text.len(), |off| i + off)
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
        self.goal_x = None;
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
        self.goal_x = None;
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

    /// The selection's pixel rect(s), relative to the text origin, for drawing selection background
    /// quads (#297). Empty when there is no selection. Single-line MVP: one rect spanning the byte
    /// range (first-line height, like [`caret_rect`](Self::caret_rect)); multi-line per-line geometry
    /// is post-MVP (the `Vec` return keeps the signature ready for it — `x_at_byte` is single-line
    /// scoped, RK-024).
    pub fn selection_rects(&self, shaped: &ShapedText) -> Vec<Rect> {
        let Some(sel) = self.selection() else {
            return Vec::new();
        };
        // The selection is always normalized (`start < end`) and never empty (a collapse stores `None`),
        // so `x0 <= x1` for single-line LTR; `.max(0.0)` guards the degenerate case regardless.
        let height = shaped.lines.first().map_or(0.0, |line| line.height);
        let x0 = x_at_byte(shaped, sel.start);
        let x1 = x_at_byte(shaped, sel.end);
        vec![Rect::from_xywh(x0, 0.0, (x1 - x0).max(0.0), height)]
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
    fn selection_rects_should_span_the_selected_range() {
        let mut system = TextSystem::new(FontDb::new());
        let style = TextStyle {
            family: "Noto Sans".into(),
            size: Px(16.0),
            weight: fontdb::Weight::NORMAL,
            line_height: None,
        };
        let shaped = system.shape("abc", &style, None);
        let mut buffer = TextBuffer::with_text("abc");
        buffer.set_selection(0..2);

        let rects = buffer.selection_rects(&shaped);
        assert_eq!(rects.len(), 1, "a single-line selection is one rect");
        let r = rects[0];
        assert_eq!(
            r.origin.x,
            x_at_byte(&shaped, 0),
            "rect starts at the selection's first byte"
        );
        assert_eq!(r.origin.y, 0.0);
        assert_eq!(
            r.size.w,
            x_at_byte(&shaped, 2) - x_at_byte(&shaped, 0),
            "rect spans the selected byte range"
        );
        assert!(r.size.h > 0.0, "rect has the line height");
    }

    #[test]
    fn selection_rects_should_be_empty_without_selection() {
        let mut system = TextSystem::new(FontDb::new());
        let style = TextStyle {
            family: "Noto Sans".into(),
            size: Px(16.0),
            weight: fontdb::Weight::NORMAL,
            line_height: None,
        };
        let shaped = system.shape("abc", &style, None);
        let buffer = TextBuffer::with_text("abc");

        assert!(
            buffer.selection_rects(&shaped).is_empty(),
            "no selection yields no rects"
        );
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
    fn move_cursor_char_should_step_over_grapheme() {
        // "é" = 'e' + U+0301 (combining acute) is one grapheme (two chars, three bytes); CharRight
        // steps over the whole cluster, not a single char.
        let mut buffer = TextBuffer::with_text("e\u{0301}x"); // "éx"
        buffer.set_selection(0..0);
        buffer.move_cursor(Movement::CharRight, false);
        assert_eq!(
            buffer.cursor(),
            3,
            "CharRight steps over the whole é grapheme (3 bytes)"
        );
        buffer.move_cursor(Movement::CharRight, false);
        assert_eq!(buffer.cursor(), 4, "then over 'x'");
    }

    #[test]
    fn delete_backward_should_remove_prev_grapheme() {
        // Grapheme-aware backspace removes the whole é cluster; a char-based delete would strip only the
        // accent, leaving "ae".
        let mut buffer = TextBuffer::with_text("ae\u{0301}"); // "aé", cursor at 4
        buffer.delete_backward();
        assert_eq!(buffer.text(), "a", "backspace removes the whole é grapheme");
        assert_eq!(buffer.cursor(), 1);
    }

    #[test]
    fn delete_forward_should_remove_next_grapheme() {
        let mut buffer = TextBuffer::with_text("e\u{0301}a"); // "éa"
        buffer.set_selection(0..0); // cursor at 0
        buffer.delete_forward();
        assert_eq!(
            buffer.text(),
            "a",
            "delete removes the whole é grapheme forward"
        );
        assert_eq!(buffer.cursor(), 0);
    }

    #[test]
    fn delete_backward_should_remove_selection_when_present() {
        let mut buffer = TextBuffer::with_text("hello");
        buffer.set_selection(1..4); // "ell"
        buffer.delete_backward();
        assert_eq!(
            buffer.text(),
            "ho",
            "a selection is deleted whole, not a single grapheme"
        );
        assert_eq!(buffer.cursor(), 1);
    }

    #[test]
    fn select_all_should_span_buffer() {
        let mut buffer = TextBuffer::with_text("hello");
        buffer.select_all();
        assert_eq!(buffer.selection(), Some(0..5));
        assert_eq!(buffer.cursor(), 5);
        assert_eq!(buffer.selected_text(), Some("hello"));
    }

    #[test]
    fn selected_text_should_borrow_selection() {
        let mut buffer = TextBuffer::with_text("hello");
        assert_eq!(buffer.selected_text(), None, "no selection borrows nothing");
        buffer.set_selection(1..4);
        assert_eq!(buffer.selected_text(), Some("ell"));
    }

    #[test]
    fn set_text_should_reset_state() {
        let mut buffer = TextBuffer::with_text("old");
        buffer.insert("x"); // establishes undo history
        buffer.set_selection(0..2);
        buffer.set_text("new");
        assert_eq!(buffer.text(), "new");
        assert_eq!(buffer.cursor(), 3, "cursor at the end of the new text");
        assert_eq!(buffer.selection(), None);
        buffer.undo();
        assert_eq!(buffer.text(), "new", "set_text clears the undo history");
    }

    #[test]
    fn move_cursor_line_should_go_home_and_end() {
        // "ab\ncd": bytes a0 b1 \n2 c3 d4; cursor starts at 5 (end, on line 2).
        let mut buffer = TextBuffer::with_text("ab\ncd");
        buffer.move_cursor(Movement::LineStart, false);
        assert_eq!(buffer.cursor(), 3, "home → start of line 2 (after the \\n)");
        buffer.move_cursor(Movement::LineEnd, false);
        assert_eq!(
            buffer.cursor(),
            5,
            "end → buffer end (line 2 has no trailing \\n)"
        );
        buffer.set_selection(0..0); // line 1
        buffer.move_cursor(Movement::LineEnd, false);
        assert_eq!(buffer.cursor(), 2, "end of line 1 is just before the \\n");
    }

    #[test]
    fn move_vertical_should_change_line_and_clamp() {
        // Two visual lines ("hello" / "world"); vertical motion moves between them via the goal column
        // and clamps at the last line. shape() is GPU-free.
        let mut system = TextSystem::new(FontDb::new());
        let style = TextStyle {
            family: "Noto Sans".into(),
            size: Px(16.0),
            weight: fontdb::Weight::NORMAL,
            line_height: None,
        };
        let shaped = system.shape("hello\nworld", &style, None);
        let mut buffer = TextBuffer::with_text("hello\nworld");

        buffer.set_selection(2..2); // "he|llo" on line 1 (bytes 0..5)
        buffer.move_vertical(true, false, &shaped);
        let on_line2 = buffer.cursor();
        assert!(
            (6..=11).contains(&on_line2),
            "down lands on line 2 (bytes 6..11), got {on_line2}"
        );
        buffer.move_vertical(true, false, &shaped); // already the last line → no-op
        assert_eq!(
            buffer.cursor(),
            on_line2,
            "vertical past the last line is a no-op"
        );
        buffer.move_vertical(false, false, &shaped);
        assert!(buffer.cursor() <= 5, "up returns onto line 1 (bytes 0..5)");
    }
}
