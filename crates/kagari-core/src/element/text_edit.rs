//! The [`TextEdit`] editable text leaf element (#298): a focusable leaf that owns a
//! [`TextBuffer`](kagari_text::TextBuffer) and renders editable text (shaped glyphs + caret + selection +
//! IME preedit), driving edits from keyboard / mouse / IME. The reusable editing primitive behind TextInput
//! (#72), NumberInput (#73), and Combobox (#79).
//!
//! **Reactive backing (ARK-003)**: `EventCx` carries no damage sink, so a mutation in `handle_event` cannot
//! flag a repaint directly. The paint-affecting state is signal-backed: every mutation bumps an
//! `edit_epoch` signal (a `ReactiveProp` bound in `request_layout` marks the node paint-dirty on change),
//! and an external `value` write is watched by a bound effect — mirroring `Scroll`'s `Animated` offset.
//!
//! **Controlled value**: the `TextBuffer` is the source of truth; the `RwSignal<String>` is a lossy
//! projection of its committed text. An edit reflects into the signal only on a real text change (guarded by
//! `last_written`, the last step after the buffer borrow ends); an external signal write is reconciled in
//! paint via `set_text` (refused while composing). Single-line MVP.

use std::sync::Arc;

use kagari_base::{Color, NodeId, Point, Px, Rect, SharedString};
use kagari_layout::LayoutStyle;
use kagari_style::ColorRole;
use kagari_text::{Movement, ShapedText, TextBuffer, TextStyle, fontdb};

use super::reactive_prop::ReactiveProp;
use super::text::paint_shaped_glyphs;
use super::{AnyElement, Element, Event, EventCx, IntoElement, LayoutCx, PaintCx};
use crate::arena::Node;
use crate::clipboard::Clipboard;
use crate::event::{
    CursorIcon, FocusHandle, FocusId, HitRegion, InteractFlags, KeyCode, KeyEvent, MouseButton,
    MouseEvent, MouseKind, use_focus_handle,
};
use crate::ime::ImeCaretArea;
use crate::reactive::prelude::*;
use crate::reactive::{RwSignal, create_effect, rx, use_context};

/// Keeps the focused caret at least this many px from the field's right edge when horizontally scrolling a
/// long single line (#72), so the caret is not painted flush against (or clipped by) the border.
const CARET_SCROLL_MARGIN: f32 = 3.0;

/// The default editor text style (single-line), matching the `text` leaf's default.
fn default_style() -> TextStyle {
    TextStyle {
        family: "Noto Sans".into(),
        size: Px(16.0),
        weight: fontdb::Weight::NORMAL,
        line_height: None,
    }
}

/// A focusable editable text leaf (#298). Build with [`text_edit`]. Owns a [`TextBuffer`] and renders
/// glyphs + caret + selection + IME preedit; keyboard / mouse / IME drive edits, each reflected into the
/// controlled `value`. Returns `impl IntoElement`. The reusable editing primitive behind TextInput (#72).
pub struct TextEdit {
    /// The controlled value: the committed text is projected here; external writes are reconciled in paint.
    value: RwSignal<String>,
    /// The editing core — the source of truth for text / cursor / selection / undo / preedit.
    buffer: TextBuffer,
    handle: FocusHandle,
    focus_id: FocusId,
    /// The OS clipboard (from context), for Ctrl+C/V/X; `None` when unprovided (copy/paste no-op).
    clipboard: Option<Arc<dyn Clipboard>>,
    /// The IME candidate-window transport (from context, #299): the caret rect (window coords) is
    /// published here at paint while focused; `None` when unprovided (headless / no shell).
    ime_caret: Option<ImeCaretArea>,
    style: TextStyle,
    /// Placeholder text shown (muted) when the buffer is empty and unfocused (#72); `None` = no placeholder.
    placeholder: Option<SharedString>,
    /// The shaped placeholder, built once in `request_layout` and reused each paint it is shown.
    placeholder_shaped: Option<ShapedText>,
    /// Bumped on every mutation so a bound `ReactiveProp` flags paint-damage (ARK-003).
    edit_epoch: RwSignal<u64>,
    epoch_prop: ReactiveProp<u64>,
    /// The last text reflected into `value`, to distinguish a self-write (echo) from an external write.
    last_written: String,
    /// Shaped committed text, memoized on the text content.
    shaped: Option<ShapedText>,
    shaped_content: Option<String>,
    /// The last laid-out bounds, cached in paint so a mouse handler can map a window pointer to text-local x.
    last_bounds: Rect,
    /// The horizontal scroll offset (px) applied in paint so the focused caret stays within the field (#72);
    /// cached so a mouse handler maps a click x back to the correct text byte.
    scroll_x: f32,
    id: Option<NodeId>,
}

/// Creates an editable text leaf bound to the controlled `value` (#298). Focusable; keyboard / mouse / IME
/// drive edits, each reflected into `value`; an external `value` write updates the display.
pub fn text_edit(value: RwSignal<String>) -> TextEdit {
    let init = value.get_untracked();
    let edit_epoch = RwSignal::new(0u64);
    let handle = use_focus_handle();
    let focus_id = handle.id();
    TextEdit {
        value,
        buffer: TextBuffer::with_text(init.clone()),
        handle,
        focus_id,
        clipboard: use_context::<Arc<dyn Clipboard>>(),
        ime_caret: use_context::<ImeCaretArea>(),
        style: default_style(),
        placeholder: None,
        placeholder_shaped: None,
        edit_epoch,
        epoch_prop: ReactiveProp::new(rx(move || edit_epoch.get())),
        last_written: init,
        shaped: None,
        shaped_content: None,
        last_bounds: Rect::default(),
        scroll_x: 0.0,
        id: None,
    }
}

/// Whether the placeholder should paint: only with a placeholder set, an empty buffer, and no focus (#72).
fn should_show_placeholder(has_placeholder: bool, is_empty: bool, is_focused: bool) -> bool {
    has_placeholder && is_empty && !is_focused
}

/// The outcome of a key/edit event for the leaf (#312): whether the leaf **handled** it (→ consume, so it
/// does not bubble to ancestors) and whether paint state **changed** (→ repaint via the edit epoch).
enum KeyOutcome {
    /// Handled, and the text/caret/selection changed → consume + repaint.
    Changed,
    /// Handled but nothing changed (Ctrl+C; Ctrl+X/V with nothing to cut/paste) → consume, no repaint.
    ConsumedUnchanged,
    /// Not handled by the leaf → bubble to ancestors (Enter single-line, ArrowUp/Down, unmatched keys), so a
    /// wrapping widget (NumberInput #76, Combobox #79) can act on them.
    Ignored,
}

impl TextEdit {
    /// Sets placeholder text shown (muted) when the buffer is empty and the leaf is unfocused (#72).
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = Some(placeholder.into());
        self
    }

    /// Sets the text font size (logical px). Used by a wrapper (TextInput #72) to scale the input with its
    /// `ControlSize`; the placeholder and committed text share it.
    pub fn font_size(mut self, size: Px) -> Self {
        self.style.size = size;
        self
    }

    /// The leaf's focus handle. A wrapper (TextInput #72) reads it to drive a focus ring reflecting the
    /// editor's focus — the leaf remains the sole focus target (the wrapper does not register the handle),
    /// so no extra tab-order entry is minted.
    pub fn focus_handle(&self) -> FocusHandle {
        self.handle.clone()
    }

    /// Bumps the edit epoch (strictly increasing), so the bound `ReactiveProp` effect marks the node
    /// paint-dirty and the frame repaints (ARK-003).
    fn bump(&self) {
        self.edit_epoch.set(self.edit_epoch.get_untracked() + 1);
    }

    /// Reflects the committed text into `value` — only on a real text change, and as the **last** step after
    /// the buffer borrow has ended (a synchronous signal write re-enters subscribers). Guarded by
    /// `last_written` (self-origination), not `buffer.text()`, so a transforming controlled parent does not
    /// loop.
    fn reflect(&mut self) {
        if self.buffer.text() != self.last_written.as_str() {
            self.last_written = self.buffer.text().to_string();
            self.value.set(self.last_written.clone());
        }
    }

    fn copy_to_clipboard(&self) {
        if let (Some(cb), Some(text)) = (&self.clipboard, self.buffer.copy()) {
            cb.set_text(&text);
        }
    }

    /// Cut/paste mutate the buffer → return whether the text changed.
    fn cut_to_clipboard(&mut self) -> bool {
        if let Some(cb) = self.clipboard.clone() {
            if let Some(text) = self.buffer.cut() {
                cb.set_text(&text);
                return true;
            }
        }
        false
    }

    fn paste_from_clipboard(&mut self) -> bool {
        if let Some(cb) = self.clipboard.clone() {
            if let Some(text) = cb.get_text() {
                self.buffer.paste(&text);
                return true;
            }
        }
        false
    }

    /// Applies a key press to the buffer; returns the [`KeyOutcome`] — whether the leaf handled the key (so
    /// `handle_event` consumes it) and whether paint state changed (so it repaints).
    fn handle_key(&mut self, ke: &KeyEvent) -> KeyOutcome {
        // Exclude AltGr (reported as Ctrl+Alt on Windows/Linux) so it types text (e.g. German AltGr+Q = '@')
        // instead of misfiring shortcuts. macOS Cmd maps to `meta`, which never carries `alt`.
        let ctrl = (ke.modifiers.ctrl || ke.modifiers.meta) && !ke.modifiers.alt;
        let shift = ke.modifiers.shift;
        match ke.code {
            KeyCode::Backspace => {
                self.buffer.delete_backward();
                KeyOutcome::Changed
            }
            KeyCode::Delete => {
                self.buffer.delete_forward();
                KeyOutcome::Changed
            }
            KeyCode::ArrowLeft => {
                let by = if ctrl {
                    Movement::WordLeft
                } else {
                    Movement::CharLeft
                };
                self.buffer.move_cursor(by, shift);
                KeyOutcome::Changed
            }
            KeyCode::ArrowRight => {
                let by = if ctrl {
                    Movement::WordRight
                } else {
                    Movement::CharRight
                };
                self.buffer.move_cursor(by, shift);
                KeyOutcome::Changed
            }
            KeyCode::Home => {
                self.buffer.move_cursor(Movement::LineStart, shift);
                KeyOutcome::Changed
            }
            KeyCode::End => {
                self.buffer.move_cursor(Movement::LineEnd, shift);
                KeyOutcome::Changed
            }
            KeyCode::KeyA if ctrl => {
                self.buffer.select_all();
                KeyOutcome::Changed
            }
            KeyCode::KeyZ if ctrl && shift => {
                self.buffer.redo();
                KeyOutcome::Changed
            }
            KeyCode::KeyZ if ctrl => {
                self.buffer.undo();
                KeyOutcome::Changed
            }
            KeyCode::KeyY if ctrl => {
                self.buffer.redo();
                KeyOutcome::Changed
            }
            // Clipboard shortcuts are always consumed by a focused field, even when there is nothing to do.
            KeyCode::KeyC if ctrl => {
                self.copy_to_clipboard();
                KeyOutcome::ConsumedUnchanged // copy leaves the buffer unchanged
            }
            KeyCode::KeyX if ctrl => {
                if self.cut_to_clipboard() {
                    KeyOutcome::Changed
                } else {
                    KeyOutcome::ConsumedUnchanged
                }
            }
            KeyCode::KeyV if ctrl => {
                if self.paste_from_clipboard() {
                    KeyOutcome::Changed
                } else {
                    KeyOutcome::ConsumedUnchanged
                }
            }
            // Single-line: Enter is not handled — it bubbles so a wrapper can submit/commit (#312). Multi-line
            // (#309) will instead insert a newline (Changed); the classification is per mode.
            KeyCode::Enter | KeyCode::NumpadEnter => KeyOutcome::Ignored,
            _ => {
                // A plain typed press (no Ctrl/Cmd) inserts its layout-resolved text (#303), minus control
                // chars (Tab/CR/etc. are not inserted verbatim). Anything the leaf does not turn into an edit
                // (ArrowUp/Down, PageUp/Down, Esc, F-keys, unbound Ctrl combos) is Ignored → it bubbles.
                if !ctrl {
                    if let Some(text) = &ke.text {
                        let filtered: String = text.chars().filter(|c| !c.is_control()).collect();
                        if !filtered.is_empty() {
                            self.buffer.insert(&filtered);
                            return KeyOutcome::Changed;
                        }
                    }
                }
                KeyOutcome::Ignored
            }
        }
    }

    /// Applies a mouse event; returns whether the caret/selection changed. Click-to-caret + Shift-click
    /// extend (drag-select is a follow-up).
    fn handle_mouse(&mut self, me: &MouseEvent) -> bool {
        if let MouseKind::Down(MouseButton::Left) = me.kind {
            self.handle.focus();
            if let Some(shaped) = self.shaped.as_ref() {
                // Map the window x to text-local x, adding back the horizontal scroll offset (#72) that paint
                // applied, so a click lands on the correct byte when a long line is scrolled.
                let x = me.pos.x - self.last_bounds.origin.x + self.scroll_x;
                let byte = self.buffer.byte_at_point(shaped, x, 0.0);
                if me.modifiers.shift {
                    let anchor = self.buffer.cursor();
                    self.buffer
                        .set_selection(anchor.min(byte)..anchor.max(byte));
                } else {
                    self.buffer.set_selection(byte..byte);
                }
            }
            return true;
        }
        false
    }
}

impl Element for TextEdit {
    fn request_layout(&mut self, cx: &mut LayoutCx) -> NodeId {
        if let Some(id) = self.id {
            return id;
        }
        let id = cx.arena.insert(Node::default());
        self.id = Some(id);
        cx.layout.insert(id, None);
        // Register FocusId → NodeId so keyboard / IME dispatch resolves this leaf (Div's pattern).
        if let Some(reg) = cx.focus.as_deref_mut() {
            reg.register(self.focus_id, id);
        }
        // Declare an I-beam cursor so the pointer reads as "editable text" over the field (#298).
        if let Some(reg) = cx.cursor.as_deref_mut() {
            reg.register(id, CursorIcon::Text);
        }
        // ARK-003: bind the edit-epoch so a mutation (which bumps it) marks this node paint-dirty.
        self.epoch_prop.bind(&cx.damage, id);
        // Watch the controlled value: an external write flags paint-dirty so the next paint reconciles it.
        // Hand-rolled (value is `String`, not `Copy`, so it can't ride a `ReactiveProp`).
        let value = self.value;
        let value_damage = Arc::clone(&cx.damage);
        create_effect(move || {
            value.track();
            value_damage.mark_paint_dirty(id);
        });
        // Watch focus: a focus gain/loss repaints so the caret shows/hides. Reading `is_focused` (tracked)
        // inside this effect is the subscription; `paint` reads it *untracked* (a `.get()` in paint would
        // warn + never re-fire on a focus change).
        let focus_handle = self.handle.clone();
        let focus_damage = Arc::clone(&cx.damage);
        create_effect(move || {
            focus_handle.is_focused();
            focus_damage.mark_paint_dirty(id);
        });
        // Fill the parent's width (an input occupies its field), with `min-width: 0` so a flex parent can
        // shrink it; the measure below provides the intrinsic (line) height. Clicking anywhere in the field
        // then hits this leaf, so an x past the text resolves to the end (byte_at_x clamps).
        cx.layout.set_style(
            id,
            &LayoutStyle {
                flex_grow: 1.0,
                min_width: Some(0.0),
                ..LayoutStyle::default()
            },
        );
        // Single-line MVP: a constant measure from the build-time content gives the line height; horizontal
        // fill comes from `flex_grow` above. Auto-grow of the intrinsic width on edit is post-MVP.
        let shaped = cx.text.shape(self.buffer.text(), &self.style, None);
        let size = shaped.size;
        self.shaped_content = Some(self.buffer.text().to_string());
        self.shaped = Some(shaped);
        // Shape the placeholder once (if any) for reuse each paint it is shown (#72).
        if let Some(ph) = &self.placeholder {
            self.placeholder_shaped = Some(cx.text.shape(ph, &self.style, None));
        }
        cx.layout.set_measure(id, Box::new(move |_avail| size));
        id
    }

    fn paint(&mut self, bounds: Rect, cx: &mut PaintCx) {
        self.last_bounds = bounds;
        // 1. Reconcile an external `value` write (poll; never while composing — `set_text` would orphan the
        //    preedit + reset the caret/undo).
        //    Compare by reference (no per-paint String clone on the common unchanged path); clone only on a
        //    genuine external write.
        let differs = self.value.with_untracked(|v| *v != self.last_written);
        if differs && !self.buffer.ime().is_composing() {
            let external = self.value.get_untracked();
            self.buffer.set_text(external.clone());
            self.last_written = external;
        }
        // 2. Shape the committed text, memoized on content.
        if self.shaped_content.as_deref() != Some(self.buffer.text()) {
            self.shaped = Some(cx.text.shape(self.buffer.text(), &self.style, None));
            self.shaped_content = Some(self.buffer.text().to_string());
        }
        let Some(shaped) = self.shaped.as_ref() else {
            return;
        };
        // The caret rect (text-local), resolved once when focused and reused for the IME caret publish (#299)
        // and the caret quad below (#298).
        let caret = self
            .handle
            .is_focused_untracked()
            .then(|| self.buffer.caret_rect(shaped));
        // Horizontal scroll (#72): shift the text left so the focused caret stays within the field; empty /
        // unfocused → 0 (left-aligned). `ox` is the effective text origin x. Cached in `self.scroll_x` (below)
        // so `handle_mouse` maps a click x back to a text byte.
        // `avail` is clamped to ≥ 0 so a degenerate field narrower than the margin still yields scroll_x = 0
        // at Home (not a spurious offset).
        let avail = (bounds.size.w - CARET_SCROLL_MARGIN).max(0.0);
        let scroll_x = caret.map_or(0.0, |cr| (cr.origin.x - avail).max(0.0));
        let ox = bounds.origin.x - scroll_x;
        // Publish the caret area (window coords, scroll-adjusted) so the OS IME candidate window follows the
        // on-screen caret + IME enables while a text field is focused (#299). Only the focused editor writes.
        if let (Some(cr), Some(area)) = (caret, &self.ime_caret) {
            area.set(Rect::from_xywh(
                ox + cr.origin.x,
                bounds.origin.y + cr.origin.y,
                cr.size.w.max(1.0),
                cr.size.h,
            ));
        }
        // Resolve colors before the atlas/scene borrows.
        let text_color = cx.theme.resolve_color(ColorRole::Text);
        let selection_color = scale_alpha(cx.theme.resolve_color(ColorRole::Accent), 0.35);
        // Clip all content (glyphs / caret / selection) to the field so a long line does not overflow the
        // border (#72); restored before the hit region below.
        let prev_clip = cx.push_clip(bounds);
        // 3. Selection quads (below the glyphs).
        for r in self.buffer.selection_rects(shaped) {
            let rect = Rect::from_xywh(
                ox + r.origin.x,
                bounds.origin.y + r.origin.y,
                r.size.w,
                r.size.h,
            );
            push_fill_quad(cx, rect, selection_color);
        }
        // 4. Glyphs (shifted by the scroll offset; the pushed clip masks the overflow).
        let text_bounds = Rect::from_xywh(ox, bounds.origin.y, bounds.size.w, bounds.size.h);
        paint_shaped_glyphs(cx, shaped, text_color, text_bounds);
        // 4b. Placeholder (muted) — only when empty and unfocused (#72). An empty buffer paints no real glyphs
        //     above, so there is no overlap. `caret.is_some()` == focused (it is gated on focus above).
        if should_show_placeholder(
            self.placeholder.is_some(),
            self.buffer.text().is_empty(),
            caret.is_some(),
        ) {
            if let Some(ph_shaped) = self.placeholder_shaped.as_ref() {
                let muted = cx.theme.resolve_color(ColorRole::TextMuted);
                paint_shaped_glyphs(cx, ph_shaped, muted, text_bounds);
            }
        }
        // 5. Caret (only when focused and not composing) — above the glyphs. `caret` is already gated on
        //    focus (untracked; the focus-watch effect drives the repaint); a 2px bar reads clearly.
        if let Some(cr) = caret {
            if !self.buffer.ime().is_composing() {
                let rect = Rect::from_xywh(
                    ox + cr.origin.x,
                    bounds.origin.y + cr.origin.y,
                    cr.size.w.max(2.0),
                    cr.size.h,
                );
                push_fill_quad(cx, rect, text_color);
            }
        }
        // 6. IME preedit at the (scroll-adjusted) caret origin, while composing. Allocate the painter order
        //    from the tree's monotonic counter (underlines at `order_base`, glyphs at `order_base + 1`) so the
        //    preedit composes above the field background — a hardcoded low order would be overdrawn nested.
        if self.buffer.ime().is_composing() {
            let cr = self.buffer.caret_rect(shaped);
            let origin = Point::new(ox + cr.origin.x, bounds.origin.y + cr.origin.y);
            let order_base = cx.next_order();
            let _glyph_slot = cx.next_order(); // reserve `order_base + 1` for the preedit glyphs
            // The active field clip (Some(bounds) here — we are inside `push_clip`) so the preedit is masked
            // to the field like the committed glyphs (#72), captured before the atlas borrow.
            let clip = cx.clip();
            if let Some(atlas) = cx.atlas.as_deref_mut() {
                self.buffer.ime().emit_preedit(
                    cx.text,
                    &self.style,
                    origin,
                    text_color,
                    atlas,
                    cx.scene,
                    order_base,
                    clip,
                );
            }
        }
        cx.set_clip(prev_clip);
        self.scroll_x = scroll_x;
        // 7. Record a hit region so clicks land + the tab order sees it as focusable.
        if let Some(id) = self.id {
            let clip = cx.clip_rect(bounds);
            let order = cx.next_order();
            cx.record_hit(HitRegion {
                node: id,
                bounds,
                clip,
                order,
                flags: InteractFlags {
                    mouse: true,
                    focusable: true,
                    cursor: true,
                    ..Default::default()
                },
            });
        }
    }

    fn handle_event(&mut self, ev: &Event, cx: &mut EventCx) {
        let outcome = match ev {
            Event::Keyboard(ke) if ke.pressed => self.handle_key(ke),
            Event::Ime(ie) => {
                self.buffer.apply_ime(ie.clone());
                KeyOutcome::Changed
            }
            Event::Mouse(me) => {
                if self.handle_mouse(me) {
                    KeyOutcome::Changed
                } else {
                    KeyOutcome::Ignored
                }
            }
            _ => KeyOutcome::Ignored,
        };
        // Consume only what the leaf handled — unhandled keys (Enter single-line, ArrowUp/Down, …) bubble to
        // ancestors so a wrapping widget (NumberInput #76, Combobox #79) can act on them (#312).
        if matches!(outcome, KeyOutcome::Changed | KeyOutcome::ConsumedUnchanged) {
            cx.stop_propagation();
        }
        if matches!(outcome, KeyOutcome::Changed) {
            // Bump the epoch (repaint), then reflect into `value` as the last step (post-borrow; reflect
            // no-ops unless the text actually changed — skips caret/selection/preedit).
            self.bump();
            self.reflect();
        }
    }
}

impl IntoElement for TextEdit {
    fn into_element(self) -> AnyElement {
        Box::new(self)
    }
}

/// Pushes a solid filled rect (caret / selection) into the scene, clipped to any scroll ancestor.
fn push_fill_quad(cx: &mut PaintCx, rect: Rect, color: Color) {
    use kagari_base::Corners;
    use kagari_render::{Background, Border, Quad, RoundedRect};
    let mask = cx.clip_rect(rect);
    if cx.clip().is_some() && mask.is_empty() {
        return;
    }
    let order = cx.next_order();
    cx.scene.quads.push(Quad {
        bounds: rect,
        corner_radii: Corners::default(),
        bg: Background::Solid(color),
        border: Border {
            widths: Default::default(),
            color: Color::TRANSPARENT,
        },
        content_mask: RoundedRect {
            rect: mask,
            radii: Corners::default(),
        },
        order,
    });
}

/// A premultiplied-linear color scaled toward transparency (all channels including alpha) — a translucent
/// selection fill from an opaque role color.
fn scale_alpha(c: Color, factor: f32) -> Color {
    Color::new(c.r * factor, c.g * factor, c.b * factor, c.a * factor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use kagari_base::Size;
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, ImeEvent, TextSystem};

    use crate::arena::Arena;
    use crate::damage::DamageState;
    use crate::event::{
        DispatchState, FocusRegistry, HitTest, Modifiers, dispatch_ime, dispatch_key,
        dispatch_mouse,
    };
    use crate::overlay::OverlayRegistry;
    use crate::paint::render_tree;
    use crate::reactive::{Owner, provide_context};

    const VIEWPORT: Size = Size { w: 300.0, h: 60.0 };

    /// An in-memory clipboard so copy/paste is testable without an OS display.
    struct MockClipboard(Mutex<String>);
    impl Clipboard for MockClipboard {
        fn get_text(&self) -> Option<String> {
            Some(self.0.lock().unwrap().clone())
        }
        fn set_text(&self, text: &str) {
            *self.0.lock().unwrap() = text.to_string();
        }
    }

    struct Harness {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        hit: HitTest,
        overlay: OverlayRegistry,
        focus: FocusRegistry,
        damage: Arc<DamageState>,
        theme: Theme,
        ime_caret: ImeCaretArea,
        root_id: NodeId,
    }

    /// Builds an editor (under an owner with a `FocusRegistry` + `MockClipboard` + `ImeCaretArea` in context)
    /// and renders one frame. Returns the harness with the editor's node id.
    fn harness(value: RwSignal<String>) -> Harness {
        let focus = FocusRegistry::new();
        provide_context(focus.clone());
        provide_context::<Arc<dyn Clipboard>>(Arc::new(MockClipboard(Mutex::new(String::new()))));
        let ime_caret = ImeCaretArea::new();
        provide_context(ime_caret.clone());
        let mut h = Harness {
            root: text_edit(value).into_element(),
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            overlay: OverlayRegistry::new(),
            focus,
            damage: Arc::new(DamageState::default()),
            theme: Theme::light(),
            ime_caret,
            root_id: NodeId::from_raw(0),
        };
        h.root_id = frame(&mut h);
        h
    }

    /// Renders one frame; returns the root node id + leaves the scene available via a fresh `Scene`.
    fn frame(h: &mut Harness) -> NodeId {
        // Clear the IME caret cell before paint, mirroring the shell (#299): the focused editor republishes
        // during paint, so a frame with no focused editor reads back `None`.
        h.ime_caret.clear();
        let mut scene = Scene::new();
        render_tree(
            &mut h.root,
            &mut h.arena,
            &mut h.layout,
            &mut h.text,
            None,
            Some(&mut h.hit),
            Some(&mut h.overlay),
            Some(&mut h.focus),
            None,
            None,
            &mut scene,
            VIEWPORT,
            &h.damage,
            &h.theme,
        )
        .unwrap()
    }

    fn quad_count(h: &mut Harness) -> usize {
        let mut scene = Scene::new();
        render_tree(
            &mut h.root,
            &mut h.arena,
            &mut h.layout,
            &mut h.text,
            None,
            Some(&mut h.hit),
            Some(&mut h.overlay),
            Some(&mut h.focus),
            None,
            None,
            &mut scene,
            VIEWPORT,
            &h.damage,
            &h.theme,
        )
        .unwrap();
        scene.quads.len()
    }

    /// Dispatches a key press (`text` = the typed characters, if any) to the editor.
    fn press(h: &mut Harness, code: KeyCode, modifiers: Modifiers, text: Option<&str>) {
        let kev = KeyEvent {
            code,
            modifiers,
            pressed: true,
            repeat: false,
            text: text.map(|s| s.to_string().into()),
        };
        dispatch_key(&mut h.root, &h.arena, Some(h.root_id), &kev);
    }

    /// Dispatches a left-button press at window x (y at the field's vertical middle).
    fn click(h: &mut Harness, x: f32) {
        let mut dispatch = DispatchState::default();
        let ev = MouseEvent::new(
            MouseKind::Down(MouseButton::Left),
            Point::new(x, 8.0),
            Modifiers::default(),
        );
        dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut dispatch);
    }

    /// Dispatches an IME event to the (focused) editor.
    fn ime(h: &mut Harness, ev: ImeEvent) {
        dispatch_ime(&mut h.root, &h.arena, Some(h.root_id), &ev);
    }

    #[test]
    fn text_edit_should_insert_typed_text() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(String::new());
        let mut h = harness(value);

        press(&mut h, KeyCode::KeyA, Modifiers::default(), Some("a"));
        assert_eq!(
            value.get_untracked(),
            "a",
            "a typed key inserts + reflects into value"
        );

        drop(owner);
    }

    #[test]
    fn text_edit_caret_should_move() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new("ab".to_string());
        let mut h = harness(value);

        // ArrowLeft moves the caret from the end (2) to 1; the next insert lands there → "aXb".
        press(&mut h, KeyCode::ArrowLeft, Modifiers::default(), None);
        press(&mut h, KeyCode::KeyX, Modifiers::default(), Some("X"));
        assert_eq!(
            value.get_untracked(),
            "aXb",
            "the insert lands where the caret moved"
        );

        drop(owner);
    }

    #[test]
    fn text_edit_should_render_selection() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new("abc".to_string());
        let mut h = harness(value);
        assert_eq!(quad_count(&mut h), 0, "no selection → no quads");

        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        press(&mut h, KeyCode::KeyA, ctrl, None); // Ctrl+A select-all
        assert!(
            quad_count(&mut h) >= 1,
            "a selection renders a background quad"
        );

        drop(owner);
    }

    #[test]
    fn text_edit_should_reconcile_external_value() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new("a".to_string());
        let mut h = harness(value);

        // An external write reconciles the buffer in paint (cursor at end); the next insert appends.
        value.set("xyz".to_string());
        frame(&mut h);
        press(&mut h, KeyCode::KeyZ, Modifiers::default(), Some("Z"));
        assert_eq!(
            value.get_untracked(),
            "xyzZ",
            "an external value write is reconciled into the buffer"
        );

        drop(owner);
    }

    #[test]
    fn text_edit_edit_should_flag_paint_damage() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(String::new());
        let mut h = harness(value);

        h.damage.clear();
        assert!(!h.damage.is_dirty(), "clean after the initial frame");
        press(&mut h, KeyCode::KeyA, Modifiers::default(), Some("a"));
        assert!(
            h.damage.is_dirty(),
            "an edit bumps the epoch → the bound effect flags paint-damage (ARK-003)"
        );

        drop(owner);
    }

    #[test]
    fn text_edit_click_should_place_caret() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new("abc".to_string());
        let mut h = harness(value);

        // Click near the left edge → caret at the start; the next insert prepends.
        click(&mut h, 1.0);
        press(&mut h, KeyCode::KeyZ, Modifiers::default(), Some("Z"));
        assert_eq!(
            value.get_untracked(),
            "Zabc",
            "a click places the caret at the pointer, so the insert prepends"
        );

        drop(owner);
    }

    #[test]
    fn text_edit_focused_caret_should_render() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new("abc".to_string());
        let mut h = harness(value);
        // Unfocused: no selection, no caret → no quads.
        assert_eq!(quad_count(&mut h), 0, "an unfocused editor draws no caret");
        // A click focuses it; the focus-watch effect repaints and the caret quad appears.
        click(&mut h, 1.0);
        assert!(quad_count(&mut h) >= 1, "a focused editor draws its caret");

        drop(owner);
    }

    #[test]
    fn text_edit_ctrl_c_v_should_roundtrip_via_clipboard() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new("abc".to_string());
        let mut h = harness(value);
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };

        press(&mut h, KeyCode::KeyA, ctrl, None); // select-all
        press(&mut h, KeyCode::KeyC, ctrl, None); // copy "abc"
        press(&mut h, KeyCode::End, Modifiers::default(), None); // collapse to end
        press(&mut h, KeyCode::KeyV, ctrl, None); // paste "abc"
        assert_eq!(
            value.get_untracked(),
            "abcabc",
            "copy then paste round-trips through the clipboard"
        );

        drop(owner);
    }

    #[test]
    fn text_edit_ime_commit_should_reflect_into_value() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(String::new());
        let mut h = harness(value);

        // A preedit composes but commits no text; the commit inserts it and reflects into `value`.
        ime(
            &mut h,
            ImeEvent::Preedit {
                text: "にほん".into(),
                cursor: Some((0, 3)),
            },
        );
        h.damage.clear();
        ime(&mut h, ImeEvent::Commit("日本".into()));
        assert_eq!(
            value.get_untracked(),
            "日本",
            "an IME commit inserts + reflects into value"
        );
        assert!(
            h.damage.is_dirty(),
            "the commit bumps the epoch → the bound effect flags paint-damage (ARK-003)"
        );

        drop(owner);
    }

    #[test]
    fn text_edit_should_refuse_external_reconcile_while_composing() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(String::new());
        let mut h = harness(value);

        // Start composing, then an external write arrives. Reconcile is refused while composing (a `set_text`
        // would orphan the preedit), so the commit builds on the pre-composition buffer, not on "zzz".
        ime(
            &mut h,
            ImeEvent::Preedit {
                text: "ほん".into(),
                cursor: Some((0, 3)),
            },
        );
        value.set("zzz".to_string());
        frame(&mut h);
        ime(&mut h, ImeEvent::Commit("本".into()));
        assert_eq!(
            value.get_untracked(),
            "本",
            "the external write was refused mid-composition; the commit did not build on \"zzz\""
        );

        drop(owner);
    }

    #[test]
    fn text_edit_unfocused_should_not_publish_ime_caret() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new("abc".to_string());
        let h = harness(value);

        // The initial frame rendered while unfocused → nothing published (the shell reads `None` = disable IME).
        assert_eq!(
            h.ime_caret.get(),
            None,
            "an unfocused editor publishes no IME caret area"
        );

        drop(owner);
    }

    #[test]
    fn text_edit_focused_should_publish_ime_caret_area() {
        let owner = Owner::new();
        owner.set();
        // Non-empty so the root editor has width for the click to land (empty → 0-width leaf, click misses).
        let value = RwSignal::new("abc".to_string());
        let mut h = harness(value);
        assert_eq!(h.ime_caret.get(), None, "unfocused → no caret area");

        // A click focuses it; the next paint publishes the caret rect (window coords) for the OS candidate window.
        click(&mut h, 1.0);
        frame(&mut h);
        let area = h
            .ime_caret
            .get()
            .expect("a focused editor publishes its caret area");
        assert!(
            area.size.h > 0.0,
            "the published caret area has the caret's height"
        );

        drop(owner);
    }

    #[test]
    fn text_edit_ime_caret_should_track_caret() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new("abc".to_string());
        let mut h = harness(value);

        click(&mut h, 1.0); // focus; caret near the start
        frame(&mut h);
        let x0 = h.ime_caret.get().expect("focused → published").origin.x;
        // Moving the caret to the end advances it → the published area's x tracks it (the OS candidate window
        // follows caret movement).
        press(&mut h, KeyCode::End, Modifiers::default(), None);
        frame(&mut h);
        let x1 = h.ime_caret.get().expect("focused → published").origin.x;
        assert!(
            x1 > x0,
            "the published caret x tracks the caret moving to the end ({x0} → {x1})"
        );

        drop(owner);
    }

    #[test]
    fn should_show_placeholder_only_when_empty_and_unfocused_with_placeholder() {
        // has_placeholder, is_empty, is_focused
        assert!(
            should_show_placeholder(true, true, false),
            "placeholder set + empty + unfocused → show"
        );
        assert!(
            !should_show_placeholder(true, true, true),
            "focused → hide (the caret leads)"
        );
        assert!(
            !should_show_placeholder(true, false, false),
            "non-empty → hide (the text leads)"
        );
        assert!(
            !should_show_placeholder(false, true, false),
            "no placeholder set → nothing to show"
        );
    }

    #[test]
    fn text_edit_should_bubble_unhandled_keys_to_ancestor() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let owner = Owner::new();
        owner.set();
        let mut focus = FocusRegistry::new();
        provide_context(focus.clone());

        // An ancestor div records the keys that bubble up to it — i.e. the keys the editor did NOT consume.
        let bubbled: Rc<RefCell<Vec<KeyCode>>> = Rc::new(RefCell::new(Vec::new()));
        let rec = bubbled.clone();
        let value = RwSignal::new("abc".to_string());
        let mut root = crate::div()
            .on_key_down(move |ke, _cx| rec.borrow_mut().push(ke.code))
            .child(text_edit(value))
            .into_element();

        let mut arena = Arena::new();
        let mut layout = LayoutTree::new();
        let mut text = TextSystem::new(FontDb::new());
        let mut hit = HitTest::new();
        let damage = Arc::new(DamageState::default());
        let theme = Theme::light();
        let mut scene = Scene::new();
        render_tree(
            &mut root,
            &mut arena,
            &mut layout,
            &mut text,
            None,
            Some(&mut hit),
            None,
            Some(&mut focus),
            None,
            None,
            &mut scene,
            VIEWPORT,
            &damage,
            &theme,
        )
        .unwrap();

        // Click inside the field to focus the editor.
        let mut dispatch = DispatchState::default();
        let click = MouseEvent::new(
            MouseKind::Down(MouseButton::Left),
            Point::new(2.0, 8.0),
            Modifiers::default(),
        );
        dispatch_mouse(&mut root, &arena, &hit, &click, &mut dispatch);
        let target = focus.focused_node();
        assert!(target.is_some(), "the click focused the editor");

        let press = |root: &mut AnyElement, code: KeyCode, mods: Modifiers, text: Option<&str>| {
            let mut kev = KeyEvent::new(code, mods, true, false);
            kev.text = text.map(|s| s.to_string().into());
            dispatch_key(root, &arena, target, &kev);
        };
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        // Unhandled keys bubble to the ancestor…
        press(&mut root, KeyCode::Enter, Modifiers::default(), None);
        press(&mut root, KeyCode::ArrowUp, Modifiers::default(), None);
        press(&mut root, KeyCode::ArrowDown, Modifiers::default(), None);
        // …handled keys do not: a typed insert, a caret move, and Ctrl+C all stay consumed by the field.
        press(&mut root, KeyCode::KeyA, Modifiers::default(), Some("a"));
        press(&mut root, KeyCode::ArrowLeft, Modifiers::default(), None);
        press(&mut root, KeyCode::KeyC, ctrl, None);

        assert_eq!(
            *bubbled.borrow(),
            vec![KeyCode::Enter, KeyCode::ArrowUp, KeyCode::ArrowDown],
            "only unhandled keys bubble to the ancestor"
        );

        drop(owner);
    }
}
