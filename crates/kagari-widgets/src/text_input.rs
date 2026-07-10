//! The [`TextInput`] widget (#72): a styled single-line text field wrapping the core editable leaf
//! ([`text_edit`]) in a bordered / rounded / padded field with a focus ring, a
//! placeholder, a control size, and a disabled state. Value is **controlled** via `RwSignal<String>` (D2).
//! The reusable input surface behind NumberInput (#76) and Combobox (#79).
//!
//! **Single-line MVP** — `.multiline` (and multi-line caret/selection geometry) is a follow-up (RK-024).
//! The editor leaf is the focus target; the field reads its focus handle to draw a ring reflecting the
//! editor's focus (RK-027 — the ring is present only when enabled). When disabled, no editor is mounted:
//! the field shows a muted static line (the value, or the placeholder when empty) and is inert.

use kagari_base::{Px, SharedString};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{AnyElement, IntoElement, Role, text, text_edit};
use kagari_style::ColorRole;

use crate::control::{ControlSize, field_frame, label_px};

/// A single-line text input (#72). Build with [`text_input`]; chain `.placeholder(..)` / `.size(..)` /
/// `.disabled(..)`. Controlled via `RwSignal<String>` (D2) — keyboard / mouse / IME drive edits that
/// reflect into the signal, and an external write updates the display. Returns `impl IntoElement`;
/// `Styled` is **not** on its surface.
pub struct TextInput {
    value: RwSignal<String>,
    placeholder: Option<SharedString>,
    size: ControlSize,
    disabled: bool,
}

/// Creates a medium, enabled single-line text input bound to the controlled `value` (#72).
pub fn text_input(value: RwSignal<String>) -> TextInput {
    TextInput {
        value,
        placeholder: None,
        size: ControlSize::Md,
        disabled: false,
    }
}

impl TextInput {
    /// Sets placeholder text shown (muted) when the field is empty (and, when enabled, unfocused).
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = Some(placeholder.into());
        self
    }

    /// Sets the control size (D3) — drives the field padding and the text font size.
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Disables the input: muted, display-only (no editor mounted), inert, and not focusable.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl IntoElement for TextInput {
    fn into_element(self) -> AnyElement {
        let TextInput {
            value,
            placeholder,
            size,
            disabled,
        } = self;
        let font: Px = label_px(size);

        if disabled {
            // Display-only: a muted static line — the current value, or the placeholder when empty. No editor
            // is mounted, so the control is inert and skips the tab order (RK-027).
            let ph = placeholder;
            let content = rx(move || {
                let v = value.get();
                if v.is_empty() {
                    ph.clone().unwrap_or_else(|| SharedString::from(""))
                } else {
                    v.into()
                }
            });
            return field_frame(size)
                .border_color(Some(ColorRole::Border))
                .role(Role::TextInput)
                .child(text(content).color_role(ColorRole::TextMuted).size(font))
                .into_element();
        }

        // Enabled: the editor leaf is the focus target; the field draws a ring reflecting *its* focus.
        let mut editor = text_edit(value).font_size(font);
        if let Some(ph) = placeholder {
            editor = editor.placeholder(ph);
        }
        // A read-only clone of the editor's focus handle — the field does not register it (the editor is the
        // sole focus target), it only reads `is_focus_visible` to color the ring (keyboard modality only).
        let handle = editor.focus_handle();

        field_frame(size)
            .border_color(rx(move || {
                Some(if handle.is_focus_visible() {
                    ColorRole::FocusRing
                } else {
                    ColorRole::Border
                })
            }))
            .role(Role::TextInput)
            .child(editor)
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use kagari_base::{NodeId, Point, Size};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::{
        DispatchState, FocusRegistry, HitTest, KeyCode, KeyEvent, Modifiers, MouseButton,
        MouseEvent, MouseKind, dispatch_key, dispatch_mouse,
    };
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: Size = Size { w: 300.0, h: 60.0 };

    struct Harness {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        hit: HitTest,
        focus: FocusRegistry,
        damage: Arc<DamageState>,
        theme: Theme,
        root_id: NodeId,
    }

    /// Builds `widget` under a `FocusRegistry` context (so the inner editor's `use_focus_handle` mints into
    /// it) and renders one frame.
    fn harness(widget: impl IntoElement) -> Harness {
        let focus = FocusRegistry::new();
        provide_context(focus.clone());
        let mut h = Harness {
            root: widget.into_element(),
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            focus,
            damage: Arc::new(DamageState::default()),
            theme: Theme::light(),
            root_id: NodeId::from_raw(0),
        };
        h.root_id = render(&mut h).0;
        h
    }

    /// Renders one frame; returns the root node id and the resulting scene.
    fn render(h: &mut Harness) -> (NodeId, Scene) {
        let mut scene = Scene::new();
        let id = render_tree(
            &mut h.root,
            &mut h.arena,
            &mut h.layout,
            &mut h.text,
            None,
            Some(&mut h.hit),
            None,
            Some(&mut h.focus),
            None,
            None,
            &mut scene,
            VIEWPORT,
            &h.damage,
            &h.theme,
        )
        .unwrap();
        (id, scene)
    }

    /// Dispatches a left-button press at window `x` (y at the field's vertical middle).
    fn click(h: &mut Harness, x: f32) {
        let mut dispatch = DispatchState::default();
        let ev = MouseEvent::new(
            MouseKind::Down(MouseButton::Left),
            Point::new(x, 8.0),
            Modifiers::default(),
        );
        dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut dispatch);
    }

    /// Dispatches a key press to the currently focused node (the inner editor, once focused).
    fn press(h: &mut Harness, code: KeyCode, text: Option<&str>) {
        let mut kev = KeyEvent::new(code, Modifiers::default(), true, false);
        kev.text = text.map(|s| s.to_string().into());
        let target = h.focus.focused_node();
        dispatch_key(&mut h.root, &h.arena, target, &kev);
    }

    #[test]
    fn text_input_should_insert_at_caret() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new("abc".to_string());
        let mut h = harness(text_input(value));

        // Click inside the field (past the padding) to focus the editor, then type.
        click(&mut h, 20.0);
        press(&mut h, KeyCode::End, None); // caret to the end
        press(&mut h, KeyCode::KeyZ, Some("Z"));
        assert_eq!(
            value.get_untracked(),
            "abcZ",
            "a click focuses the editor and typing inserts at the caret"
        );
        drop(owner);
    }

    #[test]
    fn text_input_should_reflect_external_signal() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new("a".to_string());
        let mut h = harness(text_input(value));

        // An external write is reconciled into the editor at paint; a subsequent edit builds on it.
        value.set("xyz".to_string());
        render(&mut h);
        click(&mut h, 20.0);
        press(&mut h, KeyCode::End, None);
        press(&mut h, KeyCode::KeyQ, Some("Q"));
        assert_eq!(
            value.get_untracked(),
            "xyzQ",
            "an external value write updates the display and the next edit builds on it"
        );
        drop(owner);
    }

    #[test]
    fn text_input_focused_should_show_focus_ring() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new("abc".to_string());
        let mut h = harness(text_input(value));
        let ring = h.theme.resolve_color(kagari_style::ColorRole::FocusRing);
        let has_ring = |s: &Scene| {
            s.quads
                .iter()
                .any(|q| q.border.color == ring && q.border.widths.top > 0.0)
        };

        let (_, scene) = render(&mut h);
        assert!(
            !has_ring(&scene),
            "unfocused → the border is not the FocusRing"
        );

        // Pointer focus alone (`:focus-visible` false) keeps the Border, not the ring.
        click(&mut h, 20.0);
        let (_, scene) = render(&mut h);
        assert!(
            !has_ring(&scene),
            "pointer focus alone (no :focus-visible) keeps Border, not FocusRing"
        );

        // Marking the modality keyboard shows the ring.
        h.focus.set_focus_visible(true);
        let (_, scene) = render(&mut h);
        assert!(
            has_ring(&scene),
            "a keyboard-focused input paints its FocusRing border"
        );
        drop(owner);
    }

    #[test]
    fn text_input_should_scroll_a_long_line_to_keep_the_caret_visible() {
        let owner = Owner::new();
        owner.set();
        // Short at build so the field stays at its (min) width; then grow the line far past it.
        let value = RwSignal::new("a".to_string());
        let mut h = harness(text_input(value));
        value.set("w".repeat(80));
        render(&mut h);
        click(&mut h, 20.0);
        press(&mut h, KeyCode::End, None);

        let (_, scene) = render(&mut h);
        // With horizontal scroll the caret (the rightmost quad) stays within the field; without it, the caret
        // would sit ~700px right (far past the viewport).
        let rightmost = scene
            .quads
            .iter()
            .map(|q| q.bounds.origin.x)
            .fold(0.0_f32, f32::max);
        assert!(
            rightmost < VIEWPORT.w,
            "a long line scrolls so the caret stays within the field (rightmost quad x = {rightmost})"
        );
        drop(owner);
    }

    #[test]
    fn text_input_click_on_a_scrolled_line_should_map_to_the_visible_byte() {
        let owner = Owner::new();
        owner.set();
        // Short at build so the field stays narrow; then grow the line so it scrolls.
        let value = RwSignal::new("a".to_string());
        let mut h = harness(text_input(value));
        value.set("w".repeat(80));
        render(&mut h);
        click(&mut h, 20.0); // focus
        press(&mut h, KeyCode::End, None); // scroll to the end (the tail is visible)
        render(&mut h); // paint caches scroll_x

        // Click inside the visible field. `handle_mouse` adds the cached scroll_x, so this maps into the
        // visible *tail* of the line — not the head. Insert a marker and assert it landed in the latter half.
        // (Without the scroll_x add-back, the click would map to ~byte 8 and this fails.)
        click(&mut h, 80.0);
        press(&mut h, KeyCode::KeyZ, Some("Z"));
        let v = value.get_untracked();
        let pos = v.find('Z').expect("the marker was inserted");
        assert!(
            pos > 40,
            "a click on a scrolled line maps into the visible tail, not the head (marker at byte {pos})"
        );
        drop(owner);
    }

    #[test]
    fn text_input_disabled_should_not_focus() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new("abc".to_string());
        let mut h = harness(text_input(value).disabled(true));

        // A disabled input mounts no editor, so a click focuses nothing.
        click(&mut h, 20.0);
        assert_eq!(
            h.focus.focused_node(),
            None,
            "a disabled input is inert and not focusable"
        );
        drop(owner);
    }
}
