//! The [`NumberInput`] widget (#76): a numeric entry field bound to a controlled `RwSignal<f64>`, built on
//! the core editable leaf ([`text_edit`]) with up/down spinner arrows. Typing
//! parses + clamps into the value **on commit** (blur or Enter); invalid input reverts. The steppers (and
//! ArrowUp/Down while focused) adjust by `step`. The reusable numeric surface behind VectorInput (#233).
//!
//! **Two-way binding (arch-reviewed)**: the `f64` is the source of truth; an internal `RwSignal<String>` is
//! a projection fed to the editor. An effect reformats the string when the value changes (equal-write
//! deduped) — it does not fire mid-typing (the value only changes on commit/step), so the user's uncommitted
//! text is never clobbered. Commit (parse → clamp → revert-on-invalid) runs on the blur edge and on Enter,
//! which bubbles up from the leaf (#312). Clamping is applied only to widget-originated writes; an external
//! out-of-range `value.set` is displayed as-is (controlled semantics).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kagari_base::{Px, SharedString};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, create_effect, rx};
use kagari_core::{AnyElement, IntoElement, KeyCode, Role, div, text, text_edit};
use kagari_style::ColorRole;

use crate::control::{ControlSize, field_frame, label_px};

/// A numeric entry with up/down steppers (#76). Build with [`number_input`]; chain `.range(min, max)` /
/// `.step(..)` / `.size(..)` / `.disabled(..)`. Controlled via `RwSignal<f64>` (D2). Returns
/// `impl IntoElement`; `Styled` is **not** on its surface.
pub struct NumberInput {
    value: RwSignal<f64>,
    min: f64,
    max: f64,
    step: f64,
    precision: Option<u8>,
    size: ControlSize,
    disabled: bool,
}

/// Creates a medium, enabled number input bound to the controlled `value` (#76). No range (unbounded) and a
/// unit step by default.
pub fn number_input(value: RwSignal<f64>) -> NumberInput {
    NumberInput {
        value,
        min: f64::NEG_INFINITY,
        max: f64::INFINITY,
        step: 1.0,
        precision: None,
        size: ControlSize::Md,
        disabled: false,
    }
}

impl NumberInput {
    /// Clamps the value into `[min, max]` (inclusive) on every widget-originated write. A reversed range is
    /// normalized, so a mis-ordered `.range(10.0, 0.0)` does not collapse every value to one bound.
    pub fn range(mut self, min: f64, max: f64) -> Self {
        self.min = min.min(max);
        self.max = min.max(max);
        self
    }

    /// Sets the increment/decrement applied by the steppers and ArrowUp/Down.
    pub fn step(mut self, step: f64) -> Self {
        self.step = step;
        self
    }

    /// Displays the value with a fixed number of decimal places (`0..` — e.g. `.precision(3)` → `1.500`).
    /// Editing/stepping then operates at that granularity. The default (unset) shows the natural value
    /// trimmed to at most 12 decimals. Used by [`VectorInput`](crate::VectorInput) (#233) for aligned columns.
    pub fn precision(mut self, precision: u8) -> Self {
        self.precision = Some(precision);
        self
    }

    /// Sets the control size (D3) — drives the field padding and the text font size.
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Disables the input: muted, display-only (no editor, no steppers), inert, and not focusable.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// Formats a value for display. With a fixed `precision` (#233), shows exactly that many decimals (`Some(2)`,
/// `1.5` → `"1.50"`) — aligned inspector columns. Without one, rounds to 12 decimals so binary-accumulation
/// noise from a fractional step (`0.1 + 0.1 + 0.1` → `"0.3"`, not `"0.30000000000000004"`) does not surface,
/// with trailing zeros / the dot trimmed. A non-finite value shows as empty (it should not occur for a clamped
/// finite value).
fn format_number(v: f64, precision: Option<u8>) -> String {
    if !v.is_finite() {
        return String::new();
    }
    match precision {
        Some(p) => format!("{v:.prec$}", prec = p as usize),
        None => {
            let s = format!("{v:.12}");
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        }
    }
}

/// Clamps `n` into `[min, max]` (a no-op for the default unbounded range).
fn clamp(n: f64, min: f64, max: f64) -> f64 {
    n.max(min).min(max)
}

impl IntoElement for NumberInput {
    fn into_element(self) -> AnyElement {
        let NumberInput {
            value,
            min,
            max,
            step,
            precision,
            size,
            disabled,
        } = self;
        let font: Px = label_px(size);

        if disabled {
            // Display-only: a muted static line showing the current value. No editor / steppers → inert.
            let content = rx(move || SharedString::from(format_number(value.get(), precision)));
            return field_frame(size)
                .border_color(Some(ColorRole::Border))
                .role(Role::TextInput)
                .child(text(content).color_role(ColorRole::TextMuted).size(font))
                .into_element();
        }

        // The internal String projection fed to the editor leaf.
        let text_sig = RwSignal::new(format_number(value.get_untracked(), precision));
        // f64 → String: reformat on a real value change (equal-write deduped). Does not fire mid-typing (the
        // value only changes on commit/step), so the user's uncommitted text is never clobbered.
        create_effect(move || {
            let s = format_number(value.get(), precision);
            if text_sig.with_untracked(|t| *t != s) {
                text_sig.set(s);
            }
        });
        // Commit: parse the current text and clamp it into `value` (the f64→String effect then normalizes the
        // display); invalid / non-finite input reverts the text to the formatted value. A **no-op when the
        // text still shows the formatted value** (no uncommitted edit): this stops a passive focus→blur (or a
        // step) from re-parsing a `.precision`-rounded display and snapping the value to that precision (#233)
        // — tabbing through a field must not mutate its value.
        let commit: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if text_sig
                .with_untracked(|t| t.trim() == format_number(value.get_untracked(), precision))
            {
                return;
            }
            match text_sig.get_untracked().trim().parse::<f64>() {
                Ok(n) if n.is_finite() => value.set(clamp(n, min, max)),
                _ => {
                    let s = format_number(value.get_untracked(), precision);
                    if text_sig.with_untracked(|t| *t != s) {
                        text_sig.set(s);
                    }
                }
            }
        });
        // Commit any uncommitted edit first, then step the value by `delta` (clamped) — so stepping from a
        // typed-but-uncommitted number steps from what the user sees.
        let apply_step: Arc<dyn Fn(f64) + Send + Sync> = {
            let commit = commit.clone();
            Arc::new(move |delta: f64| {
                commit();
                // Guard against a step overflowing to a non-finite value (e.g. near f64::MAX) — leave the
                // value unchanged rather than setting `inf` (which would render as an empty, stuck field).
                let next = value.get_untracked() + delta;
                if next.is_finite() {
                    value.set(clamp(next, min, max));
                }
            })
        };

        let editor = text_edit(text_sig).font_size(font);
        let handle = editor.focus_handle();

        // Commit on the focused → unfocused edge (blur). The latch fires only on true→false, not focus gain.
        {
            let commit = commit.clone();
            let handle = handle.clone();
            let prev = Arc::new(AtomicBool::new(false));
            create_effect(move || {
                let now = handle.is_focused();
                let was = prev.swap(now, Ordering::Relaxed);
                if was && !now {
                    commit();
                }
            });
        }

        // The up/down spinner column (not focusable — the field is the focus target). Arrow glyphs are sized
        // so the two stacked line-boxes match the editor's line height (2 × 0.5 × 1.2 = 1.2), keeping the
        // field the same height as a sibling TextInput of the same `ControlSize`.
        let arrow = Px(font.0 * 0.5);
        let up_step = apply_step.clone();
        let down_step = apply_step.clone();
        let spinner = div()
            .flex_col()
            .child(
                div()
                    .on_click(move |_ev, _cx| up_step(step))
                    .child(text("▲").color_role(ColorRole::TextMuted).size(arrow)),
            )
            .child(
                div()
                    .on_click(move |_ev, _cx| down_step(-step))
                    .child(text("▼").color_role(ColorRole::TextMuted).size(arrow)),
            );

        // Key handlers: ArrowUp/Down step, Enter commits. These bubble up from the editor (#312); consume
        // them here so they do not also reach an ancestor scroll/submit. `!repeat` → one step per press.
        let key_step = apply_step;
        let key_commit = commit;
        let ring_handle = handle;
        field_frame(size)
            .items_center()
            .border_color(rx(move || {
                Some(if ring_handle.is_focus_visible() {
                    ColorRole::FocusRing
                } else {
                    ColorRole::Border
                })
            }))
            .role(Role::TextInput)
            .on_key_down(move |ke, cx| {
                if ke.repeat {
                    return;
                }
                match ke.code {
                    KeyCode::ArrowUp => {
                        key_step(step);
                        cx.stop_propagation();
                    }
                    KeyCode::ArrowDown => {
                        key_step(-step);
                        cx.stop_propagation();
                    }
                    KeyCode::Enter | KeyCode::NumpadEnter => {
                        key_commit();
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            })
            .child(editor)
            .child(spinner)
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
        DispatchState, FocusRegistry, HitTest, KeyEvent, Modifiers, MouseButton, MouseEvent,
        MouseKind, dispatch_key, dispatch_mouse,
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

    /// Builds `widget` under a `FocusRegistry` context (so the inner editor mints into it) and renders once.
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
        h.root_id = render(&mut h);
        h
    }

    fn render(h: &mut Harness) -> NodeId {
        let mut scene = Scene::new();
        render_tree(
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
        .unwrap()
    }

    fn click(h: &mut Harness, x: f32) {
        let mut dispatch = DispatchState::default();
        let ev = MouseEvent::new(
            MouseKind::Down(MouseButton::Left),
            Point::new(x, 18.0),
            Modifiers::default(),
        );
        dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut dispatch);
    }

    /// Dispatches a key press to the focused editor; Enter/arrows bubble up to the NumberInput field (#312).
    fn press(h: &mut Harness, code: KeyCode, mods: Modifiers, text: Option<&str>) {
        let mut kev = KeyEvent::new(code, mods, true, false);
        kev.text = text.map(|s| s.to_string().into());
        dispatch_key(&mut h.root, &h.arena, h.focus.focused_node(), &kev);
    }

    fn ctrl() -> Modifiers {
        let mut m = Modifiers::default();
        m.ctrl = true;
        m
    }

    /// The absolute center of a laid-out node (layout is parent-relative, RK-007 — accumulate ancestor origins).
    fn abs_center(h: &Harness, target: NodeId) -> Option<Point> {
        fn walk(h: &Harness, node: NodeId, ox: f32, oy: f32, target: NodeId) -> Option<Point> {
            let r = h.layout.layout(node);
            let (ax, ay) = (ox + r.origin.x, oy + r.origin.y);
            if node == target {
                return Some(Point::new(ax + r.size.w / 2.0, ay + r.size.h / 2.0));
            }
            for &c in &h.arena.get(node)?.children {
                if let Some(p) = walk(h, c, ax, ay, target) {
                    return Some(p);
                }
            }
            None
        }
        walk(h, h.root_id, 0.0, 0.0, target)
    }

    /// A full press + release at `p` (an `on_click` fires only on a down/up pair on the same node, #48).
    fn click_at(h: &mut Harness, p: Point) {
        let mut dispatch = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(kind, p, Modifiers::default());
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut dispatch);
        }
    }

    #[test]
    fn number_input_precision_should_format_fixed_decimals() {
        // With a precision, exactly that many decimals (aligned columns); rounding applies.
        assert_eq!(format_number(1.5, Some(2)), "1.50", "fixed 2 decimals");
        assert_eq!(
            format_number(1.23456, Some(3)),
            "1.235",
            "rounds to 3 decimals"
        );
        assert_eq!(
            format_number(2.0, Some(0)),
            "2",
            "precision 0 → no decimals"
        );
        // Without a precision, the natural value is trimmed to at most 12 decimals.
        assert_eq!(
            format_number(1.5, None),
            "1.5",
            "no precision trims trailing zeros"
        );
        assert_eq!(
            format_number(0.1 + 0.2, None),
            "0.3",
            "12-dp rounding hides binary noise"
        );
    }

    #[test]
    fn number_input_precision_should_not_round_on_passive_blur() {
        let owner = Owner::new();
        owner.set();
        // Focusing then blurring a `.precision` field WITHOUT editing must not snap the value to the
        // displayed precision (commit is a no-op when the text still shows the formatted value).
        let value = RwSignal::new(1.005_f64);
        let mut h = harness(number_input(value).precision(2));
        click(&mut h, 20.0); // focus, no edit (display shows "1.00")
        h.focus.current().set(None); // blur → commit
        assert!(
            (value.get_untracked() - 1.005).abs() < 1e-9,
            "a passive focus->blur leaves the value intact (got {})",
            value.get_untracked()
        );
        drop(owner);
    }

    #[test]
    fn number_input_should_parse_and_clamp() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(5.0);
        let mut h = harness(number_input(value).range(0.0, 10.0));

        click(&mut h, 20.0); // focus the editor
        press(&mut h, KeyCode::KeyA, ctrl(), None); // select-all
        press(&mut h, KeyCode::Digit9, Modifiers::default(), Some("9"));
        press(&mut h, KeyCode::Digit9, Modifiers::default(), Some("9"));
        press(&mut h, KeyCode::Enter, Modifiers::default(), None); // commit
        assert_eq!(
            value.get_untracked(),
            10.0,
            "typed 99 parses and clamps to the max"
        );
        drop(owner);
    }

    #[test]
    fn number_input_stepper_should_increment_by_step() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(5.0);
        let mut h = harness(number_input(value).step(2.0));

        click(&mut h, 20.0);
        press(&mut h, KeyCode::ArrowUp, Modifiers::default(), None);
        assert_eq!(
            value.get_untracked(),
            7.0,
            "ArrowUp steps by `step` (5 + 2)"
        );
        press(&mut h, KeyCode::ArrowDown, Modifiers::default(), None);
        assert_eq!(value.get_untracked(), 5.0, "ArrowDown steps back (7 - 2)");
        drop(owner);
    }

    #[test]
    fn number_input_invalid_should_revert() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(3.0);
        let mut h = harness(number_input(value));

        click(&mut h, 20.0);
        press(&mut h, KeyCode::KeyA, ctrl(), None); // select-all
        press(&mut h, KeyCode::KeyX, Modifiers::default(), Some("x")); // type a non-number
        press(&mut h, KeyCode::Enter, Modifiers::default(), None); // commit → invalid → revert
        assert_eq!(
            value.get_untracked(),
            3.0,
            "invalid input reverts; the value is unchanged"
        );

        // Positive control: a *valid* entry on the same (still-focused) field does commit — so the assert
        // above tests revert, not a dead Enter path that never reaches commit.
        press(&mut h, KeyCode::KeyA, ctrl(), None); // select-all (the reverted "3")
        press(&mut h, KeyCode::Digit4, Modifiers::default(), Some("4"));
        press(&mut h, KeyCode::Enter, Modifiers::default(), None);
        assert_eq!(
            value.get_untracked(),
            4.0,
            "a valid entry commits (Enter reaches the commit path)"
        );
        drop(owner);
    }

    #[test]
    fn number_input_blur_should_commit() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(5.0);
        let mut h = harness(number_input(value));

        click(&mut h, 20.0); // focus the editor
        press(&mut h, KeyCode::KeyA, ctrl(), None); // select-all
        press(&mut h, KeyCode::Digit7, Modifiers::default(), Some("7")); // type "7" (uncommitted)
        assert_eq!(
            value.get_untracked(),
            5.0,
            "typing alone does not commit — the value is unchanged until blur/Enter"
        );
        // Blur: clearing the focus registry drops focus off the editor; the blur-watch edge fires commit.
        h.focus.current().set(None);
        assert_eq!(
            value.get_untracked(),
            7.0,
            "losing focus commits the typed value"
        );
        drop(owner);
    }

    #[test]
    fn number_input_spinner_click_should_step() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(5.0);
        let mut h = harness(number_input(value).step(2.0));

        // The field's children are [editor, spinner-column]; the spinner's children are [up ▲, down ▼].
        let spinner = h.arena.get(h.root_id).expect("root node").children[1];
        let arrows = h.arena.get(spinner).expect("spinner node").children.clone();
        let (up, down) = (arrows[0], arrows[1]);

        let up_c = abs_center(&h, up).expect("up arrow laid out");
        click_at(&mut h, up_c);
        assert_eq!(value.get_untracked(), 7.0, "clicking ▲ steps up by `step`");
        let down_c = abs_center(&h, down).expect("down arrow laid out");
        click_at(&mut h, down_c);
        assert_eq!(
            value.get_untracked(),
            5.0,
            "clicking ▼ steps down by `step`"
        );
        drop(owner);
    }

    #[test]
    fn number_input_external_set_should_update_the_display() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(5.0);
        let mut h = harness(number_input(value)); // unbounded, unit step

        value.set(7.0); // external write → the f64→String projection reformats the display
        render(&mut h);
        // Prove the display reflected 7 (not the initial 5): a step commits the *displayed* text first, so
        // ArrowUp gives 7 → 8. Had the projection not run, commit would parse "5" and yield 6.
        click(&mut h, 20.0);
        press(&mut h, KeyCode::ArrowUp, Modifiers::default(), None);
        assert_eq!(
            value.get_untracked(),
            8.0,
            "an external value.set updates the display; a step builds on it (7 → 8)"
        );
        drop(owner);
    }

    #[test]
    fn number_input_disabled_should_not_focus() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(5.0);
        let mut h = harness(number_input(value).disabled(true));

        click(&mut h, 20.0);
        assert_eq!(
            h.focus.focused_node(),
            None,
            "a disabled number input mounts no editor and is not focusable"
        );
        drop(owner);
    }
}
