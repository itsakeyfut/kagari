//! Abstract IME events (specs §5.2) and the preedit composition state.
//!
//! kagari-core converts the platform's winit IME events into `ImeEvent`, so kagari-text
//! never depends on winit. `ImeState` holds the uncommitted (preedit) string and renders
//! it inline at the caret with underlines (#22 glyphs + #23 underline). Committing the
//! conversion into a `TextBuffer` (with undo) is #26.

use kagari_base::{Color, Point, Rect};
use kagari_render::{Atlas, RoundedRect, Scene, Underline, UnderlineStyle};

use crate::shape::{ShapedText, TextStyle, TextSystem};

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

/// IME composition state: the uncommitted preedit string plus the active segment
/// (the OS cursor's byte range into the preedit, shown with a dotted underline).
#[derive(Default, Clone, Debug)]
pub struct ImeState {
    preedit: String,
    active: Option<(usize, usize)>,
}

impl ImeState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply an IME event. `Preedit` updates the composing text (returns `None`);
    /// `Commit` clears the preedit and returns the committed string for the caller to
    /// insert — #26's `TextBuffer` records that insertion in undo.
    pub fn apply(&mut self, event: ImeEvent) -> Option<String> {
        match event {
            ImeEvent::Preedit { text, cursor } => {
                self.preedit = text;
                self.active = cursor;
                None
            }
            ImeEvent::Commit(text) => {
                self.preedit.clear();
                self.active = None;
                Some(text)
            }
        }
    }

    /// The current uncommitted preedit text (empty when not composing).
    pub fn preedit(&self) -> &str {
        &self.preedit
    }

    /// Whether an IME composition is in progress (non-empty preedit).
    pub fn is_composing(&self) -> bool {
        !self.preedit.is_empty()
    }

    /// The caret area in logical px, relative to the preedit origin, for the OS IME
    /// candidate window. #26 adds the field origin and calls `set_ime_cursor_area`.
    /// The caret sits at the active segment's start (or the preedit start if none).
    pub fn caret_rect(&self, shaped: &ShapedText) -> Rect {
        let caret_byte = self.active.map_or(0, |(start, _)| start);
        let height = shaped.lines.first().map_or(0.0, |line| line.height);
        Rect::from_xywh(x_at_byte(shaped, caret_byte), 0.0, 1.0, height)
    }

    /// Shape and rasterize the preedit at `origin` (logical px) and emit its glyphs
    /// plus a solid underline (whole preedit) and a dotted underline (active segment)
    /// into `scene`. The underlines draw behind the glyphs (lower painter order).
    pub fn emit_preedit(
        &self,
        text_system: &mut TextSystem,
        style: &TextStyle,
        origin: Point,
        color: Color,
        atlas: &mut Atlas,
        scene: &mut Scene,
    ) {
        if self.preedit.is_empty() {
            return;
        }
        let shaped = text_system.shape(&self.preedit, style, None);

        // Glyphs: rasterize at the 0-origin, translate to the caret origin, and draw
        // above the underline (order 1 > the underline's order 0).
        let mut glyphs = Vec::new();
        text_system.rasterize_into(&shaped, color, atlas, &mut glyphs);
        for mut glyph in glyphs {
            glyph.bounds.origin.x += origin.x;
            glyph.bounds.origin.y += origin.y;
            glyph.order = 1;
            scene.glyphs.push(glyph);
        }

        // Underlines just below the text. The active segment (dotted) overlays the
        // solid whole-preedit underline to emphasize the converting region (Q5).
        let thickness = (style.size.0 * 0.0625).max(1.0);
        let underline_y = origin.y + shaped.size.h - thickness;
        let no_clip = RoundedRect {
            rect: Rect::from_xywh(0.0, 0.0, 1.0e4, 1.0e4),
            radii: Default::default(),
        };
        scene.underlines.push(Underline {
            rect: Rect::from_xywh(origin.x, underline_y, shaped.size.w, thickness),
            color,
            style: UnderlineStyle::Solid,
            thickness,
            content_mask: no_clip,
            order: 0,
        });
        if let Some((start, end)) = self.active {
            let (start_x, end_x) = (x_at_byte(&shaped, start), x_at_byte(&shaped, end));
            scene.underlines.push(Underline {
                rect: Rect::from_xywh(
                    origin.x + start_x,
                    underline_y,
                    (end_x - start_x).max(0.0),
                    thickness,
                ),
                color,
                style: UnderlineStyle::Dotted,
                thickness,
                content_mask: no_clip,
                order: 0,
            });
        }
    }
}

/// Logical x (preedit-relative) of byte offset `byte`: the x of the first glyph whose
/// cluster is at or past `byte`, or the laid-out width if `byte` is past the end.
/// Assumes LTR, single-line preedit (cluster ascending ⇒ x ascending), which holds for
/// Japanese IME composition (MVP; RTL/multi-line is post-MVP). Shared with
/// `TextBuffer::caret_rect` (#26).
pub(crate) fn x_at_byte(shaped: &ShapedText, byte: usize) -> f32 {
    shaped
        .glyphs
        .iter()
        .find(|glyph| glyph.cluster >= byte)
        .map_or(shaped.size.w, |glyph| glyph.x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FontDb, fontdb};
    use kagari_base::Px;

    fn style() -> TextStyle {
        TextStyle {
            family: "Noto Sans".into(),
            size: Px(16.0),
            weight: fontdb::Weight::NORMAL,
            line_height: None,
        }
    }

    #[test]
    fn ime_state_apply_should_store_preedit_and_clear_on_commit() {
        let mut state = ImeState::new();
        assert_eq!(
            state.apply(ImeEvent::Preedit {
                text: "あ".into(),
                cursor: Some((0, 3))
            }),
            None
        );
        assert_eq!(state.preedit(), "あ");
        assert!(state.is_composing());
        // Commit clears the preedit and hands back the string for #26's buffer.
        assert_eq!(
            state.apply(ImeEvent::Commit("日".into())),
            Some("日".to_string())
        );
        assert_eq!(state.preedit(), "");
        assert!(!state.is_composing());
    }

    #[test]
    fn x_at_byte_should_map_byte_to_glyph_x() {
        // shape() is GPU-free, so this needs no device.
        let mut system = TextSystem::new(FontDb::new());
        let shaped = system.shape("AB", &style(), None);
        // Byte 0 is the first glyph's origin (0.0); a byte past the end is the width.
        assert_eq!(x_at_byte(&shaped, 0), shaped.glyphs[0].x);
        assert_eq!(x_at_byte(&shaped, 99), shaped.size.w);
    }

    #[test]
    fn caret_rect_should_sit_at_active_segment_start() {
        let mut system = TextSystem::new(FontDb::new());
        let shaped = system.shape("日本語", &style(), None);
        let mut state = ImeState::new();
        // Active segment "本" starts at byte 3 (each kanji is 3 UTF-8 bytes).
        state.apply(ImeEvent::Preedit {
            text: "日本語".into(),
            cursor: Some((3, 6)),
        });
        let caret = state.caret_rect(&shaped);
        assert_eq!(caret.origin.x, x_at_byte(&shaped, 3));
        assert!(
            caret.size.h > 0.0,
            "caret height comes from the line height"
        );
    }
}
