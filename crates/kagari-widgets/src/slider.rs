//! The [`Slider`] widget (#75): a controlled `RwSignal<f32>` value driven by a **drag** (pointer capture,
//! click-to-set) or the **keyboard**, clamped to a `[min, max]` range and snapped to an optional `step`.
//!
//! Composition (a track + a filled portion + a thumb, no absolute positioning) reuses the
//! [`Splitter`](crate::Splitter) idiom: the thumb doubles as the drag "divider" between the filled
//! (`flex_grow(fraction)`) and unfilled (`flex_grow(1 - fraction)`) track segments, and the pointer→value
//! map reads the track's laid-out length from a [`BoundsHandle`] (`usable = length − thumb`). The thumb is a
//! [`Switch`](crate::Switch)-style knob (rounded + drop shadow), with the focus ring on it; focus + disabled
//! follow [`Button`](crate::Button). Vertical sliders map `bottom = min` / `top = max` (a fader).

use std::cell::Cell;
use std::rc::Rc;

use kagari_base::{Axis, Point, SharedString, Size};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{
    AnyElement, BoundsHandle, CursorIcon, IntoElement, KeyCode, MouseButton, MouseKind, Role, div,
    use_focus_handle,
};
use kagari_style::{ColorRole, Styled};

use crate::control::{ControlSize, slider_dims};

/// Minimum track length (logical px) so a bare slider is usable; it still `flex_grow`s to fill a wider slot.
const SLIDER_MIN_LEN: f32 = 120.0;

/// A value coerced into `[min, max]` (a non-finite value → `min`); guards a degenerate range (`min > max`)
/// so `f32::clamp` (which panics when `min > max`) is never hit and no `flex_grow` gets a NaN (RK-045).
fn sane(v: f32, min: f32, max: f32) -> f32 {
    if !v.is_finite() {
        return min;
    }
    if min <= max { v.clamp(min, max) } else { min }
}

/// The `[0, 1]` fill fraction of `v` in `[min, max]` — always finite (RK-045: fed straight into `flex_grow`,
/// where a NaN/negative would collapse the segment). A degenerate range yields `0`.
fn fraction(v: f32, min: f32, max: f32) -> f32 {
    if max > min {
        ((sane(v, min, max) - min) / (max - min)).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// `v` clamped to `[min, max]` and snapped to the nearest `step` (a `<= 0`/non-finite step = continuous).
fn snap(v: f32, min: f32, max: f32, step: f32) -> f32 {
    let clamped = sane(v, min, max);
    if step > 0.0 && step.is_finite() && max > min {
        (min + ((clamped - min) / step).round() * step).clamp(min, max)
    } else {
        clamped
    }
}

/// A slider (#75): a `[min, max]` value bound to an `RwSignal<f32>`, driven by drag (pointer capture,
/// click-to-set) or keyboard. Build with [`slider`]; chain [`.range`](Self::range)/[`.step`](Self::step)/
/// [`.size`](Self::size)/[`.disabled`](Self::disabled)/[`.vertical`](Self::vertical). Returns
/// `impl IntoElement`; `Styled` is not on its surface. Fills the length its parent gives it.
pub struct Slider {
    value: RwSignal<f32>,
    min: f32,
    max: f32,
    step: f32,
    size: ControlSize,
    disabled: bool,
    axis: Axis,
}

/// Creates a slider bound to `value` (controlled, D2), range `0.0..=1.0`, continuous (no step), horizontal.
pub fn slider(value: RwSignal<f32>) -> Slider {
    Slider {
        value,
        min: 0.0,
        max: 1.0,
        step: 0.0,
        size: ControlSize::Md,
        disabled: false,
        axis: Axis::Horizontal,
    }
}

impl Slider {
    /// Sets the value range (`min..=max`). The value is clamped to it and the thumb spans it.
    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.min = min;
        self.max = max;
        self
    }

    /// Sets the step the value snaps to (drag + keyboard). `0.0` (the default) = continuous.
    pub fn step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }

    /// Sets the control size (track thickness + thumb size).
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Disables the slider: muted styling and inert (no drag / keyboard), and not focusable.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Lays the slider out horizontally (left = min, right = max) — the default.
    pub fn horizontal(mut self) -> Self {
        self.axis = Axis::Horizontal;
        self
    }

    /// Lays the slider out vertically (bottom = min, top = max — a fader): a drag up increases the value.
    pub fn vertical(mut self) -> Self {
        self.axis = Axis::Vertical;
        self
    }
}

impl IntoElement for Slider {
    fn into_element(self) -> AnyElement {
        let Slider {
            value,
            min,
            max,
            step,
            size,
            disabled,
            axis,
        } = self;
        let vertical = matches!(axis, Axis::Vertical);
        let (track_h, thumb_px) = slider_dims(size);
        let (track_h, thumb_px) = (track_h.0, thumb_px.0);

        // The container publishes its laid-out bounds; the drag reads the main-axis length (RK: paint-cell,
        // read untracked in handlers — never a signal).
        let bounds = BoundsHandle::new();

        // Track segments: filled (Accent) grows with the fill fraction, rest (Border) with `1 - fraction`.
        // A thin fixed cross-thickness (`track_h`) + `flex_grow` on the main axis. (Light-theme note RK-030:
        // the unfilled track uses `Border`, not `SurfaceRaised` which resolves to white.)
        let (filled_bg, rest_bg) = if disabled {
            (ColorRole::BorderStrong, ColorRole::Border)
        } else {
            (ColorRole::Accent, ColorRole::Border)
        };
        let seg = |bg: ColorRole, d: kagari_core::element::Div| {
            let d = d.rounded_full().bg(bg);
            if vertical {
                d.min_w(track_h).min_h(0.0)
            } else {
                d.min_h(track_h).min_w(0.0)
            }
        };
        let filled = seg(
            filled_bg,
            div().flex_grow(rx(move || fraction(value.get(), min, max))),
        );
        let rest = seg(
            rest_bg,
            div().flex_grow(rx(move || 1.0 - fraction(value.get(), min, max))),
        );

        // The thumb: a Switch-style knob (rounded + drop shadow so a white knob reads on the light theme),
        // with the focus ring (a reactive `FocusRing` border, keyboard modality) on it when enabled.
        let focus = (!disabled).then(use_focus_handle);
        let thumb_bg = if disabled {
            ColorRole::Surface
        } else {
            ColorRole::AccentFg
        };
        let mut thumb = div().rounded_full().shadow_sm().bg(thumb_bg).size(Size {
            w: thumb_px,
            h: thumb_px,
        });
        if let Some(f) = &focus {
            let ring = f.clone();
            thumb = thumb.border_w_2().border_color(rx(move || {
                ring.is_focus_visible().then_some(ColorRole::FocusRing)
            }));
        }

        // The root fills the slot its parent gives it (a definite main-axis length so the thumb can travel).
        let mut root = if vertical {
            div().flex_col()
        } else {
            div().flex()
        };
        root = root
            .items_center()
            .flex_grow(1.0)
            .cursor(CursorIcon::Pointer)
            .track_bounds(&bounds);
        root = if vertical {
            root.min_h(SLIDER_MIN_LEN).min_w(0.0)
        } else {
            root.min_w(SLIDER_MIN_LEN).min_h(0.0)
        };
        // A11y (#75): expose the control as a slider and announce its current value, resolved reactively at
        // paint so a screen reader tracks drags/keyboard changes (a disabled slider still reports its value).
        root = root
            .role(Role::Slider)
            .a11y_value(rx(move || SharedString::from(format!("{}", value.get()))));
        if let Some(f) = &focus {
            root = root.track_focus(f);
        }

        if !disabled {
            let dragging = Rc::new(Cell::new(false));
            let (drag_d, drag_m, drag_u) = (dragging.clone(), dragging.clone(), dragging);
            // The pointer → value map (click-to-set + drag): the fraction of the pointer along the usable
            // travel (`length − thumb`, so the pointer tracks the thumb centre). Vertical inverts so a drag
            // up raises the value (top = max).
            let set: Rc<dyn Fn(Point)> = {
                let bounds = bounds.clone();
                Rc::new(move |p: Point| {
                    let b = bounds.get();
                    let (coord, origin, len) = if vertical {
                        (p.y, b.origin.y, b.size.h)
                    } else {
                        (p.x, b.origin.x, b.size.w)
                    };
                    let usable = len - thumb_px;
                    if usable <= 1.0 {
                        return;
                    }
                    let mut f = ((coord - origin - thumb_px / 2.0) / usable).clamp(0.0, 1.0);
                    if vertical {
                        f = 1.0 - f;
                    }
                    value.set(snap(min + f * (max - min), min, max, step));
                })
            };
            let (set_d, set_m) = (set.clone(), set);
            root = root
                .on_mouse_down(move |ev, cx| {
                    if let MouseKind::Down(MouseButton::Left) = ev.kind {
                        cx.capture_pointer();
                        drag_d.set(true);
                        set_d(ev.pos);
                    }
                })
                .on_mouse_move(move |ev, _cx| {
                    if drag_m.get() {
                        set_m(ev.pos);
                    }
                })
                .on_mouse_up(move |ev, cx| {
                    if let MouseKind::Up(MouseButton::Left) = ev.kind {
                        if drag_u.replace(false) {
                            cx.release_pointer();
                        }
                    }
                })
                .on_key_down(move |kev, _cx| {
                    if !kev.pressed {
                        return;
                    }
                    // Arrow/PageUp-Down/Home/End are orientation-agnostic: up/right increase. A `step` sets
                    // the arrow increment; PageUp/Down move by 10× (or a page tenth when continuous).
                    let cur = sane(value.get_untracked(), min, max);
                    let kstep = if step > 0.0 {
                        step
                    } else {
                        (max - min) / 100.0
                    };
                    let page = if step > 0.0 {
                        step * 10.0
                    } else {
                        (max - min) / 10.0
                    };
                    let next = match kev.code {
                        KeyCode::ArrowUp | KeyCode::ArrowRight => cur + kstep,
                        KeyCode::ArrowDown | KeyCode::ArrowLeft => cur - kstep,
                        KeyCode::PageUp => cur + page,
                        KeyCode::PageDown => cur - page,
                        KeyCode::Home => min,
                        KeyCode::End => max,
                        _ => return,
                    };
                    value.set(snap(next, min, max, step));
                });
        }

        // The filled portion sits on the min side: [filled, thumb, rest] horizontally (left = min),
        // [rest, thumb, filled] vertically (filled at the bottom = min).
        root = if vertical {
            root.child(rest).child(thumb).child(filled)
        } else {
            root.child(filled).child(thumb).child(rest)
        };
        root.into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    use kagari_base::{NodeId, Size as BSize};
    use kagari_core::arena::Arena;
    use kagari_core::damage::DamageState;
    use kagari_core::event::FocusRegistry;
    use kagari_core::paint::render_tree;
    use kagari_core::reactive::{Owner, provide_context};
    use kagari_core::{
        DispatchState, HitTest, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseKind,
        dispatch_key, dispatch_mouse,
    };
    use kagari_layout::LayoutTree;
    use kagari_render::Scene;
    use kagari_style::Theme;
    use kagari_text::{FontDb, TextSystem};

    const VIEWPORT: BSize = BSize { w: 200.0, h: 200.0 };
    /// The slot width the slider fills in tests → a definite track length for the drag map.
    const SLOT: f32 = 200.0;

    struct Harness {
        root: AnyElement,
        arena: Arena,
        layout: LayoutTree,
        text: TextSystem,
        hit: HitTest,
        focus: FocusRegistry,
        damage: StdArc<DamageState>,
        theme: Theme,
        root_id: NodeId,
    }

    /// Builds `sl` inside a `SLOT`-sized flex slot (so its track has a definite length) and frames it.
    fn harness(sl: Slider) -> Harness {
        let focus = FocusRegistry::new();
        provide_context(focus.clone());
        let root = div()
            .flex()
            .size(BSize { w: SLOT, h: 40.0 })
            .child(sl)
            .into_element();
        let mut h = Harness {
            root,
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            focus,
            damage: StdArc::new(DamageState::default()),
            theme: Theme::light(),
            root_id: NodeId::from_raw(0),
        };
        h.root_id = frame(&mut h);
        h
    }

    fn frame(h: &mut Harness) -> NodeId {
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

    /// The slider root node (the sized slot's only child).
    fn slider_node(h: &Harness) -> NodeId {
        h.arena.get(h.root_id).unwrap().children[0]
    }

    fn press(h: &mut Harness, focused: NodeId, code: KeyCode) {
        let kev = KeyEvent::new(code, Modifiers::default(), true, false);
        dispatch_key(&mut h.root, &h.arena, Some(focused), &kev);
    }

    /// Dispatches a mouse-down (+ optional moves) then up at the given x's (a drag along the track).
    fn drag(h: &mut Harness, xs: &[f32], y: f32) {
        let mut st = DispatchState::default();
        for (i, &x) in xs.iter().enumerate() {
            let kind = if i == 0 {
                MouseKind::Down(MouseButton::Left)
            } else {
                MouseKind::Move
            };
            let ev = MouseEvent::new(kind, kagari_base::Point::new(x, y), Modifiers::default());
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut st);
        }
        let up = MouseEvent::new(
            MouseKind::Up(MouseButton::Left),
            kagari_base::Point::new(*xs.last().unwrap(), y),
            Modifiers::default(),
        );
        dispatch_mouse(&mut h.root, &h.arena, &h.hit, &up, &mut st);
    }

    #[test]
    fn fraction_should_be_finite_in_range_and_guard_degenerate() {
        assert_eq!(fraction(0.5, 0.0, 1.0), 0.5);
        assert_eq!(fraction(-1.0, 0.0, 1.0), 0.0, "below min clamps to 0");
        assert_eq!(fraction(2.0, 0.0, 1.0), 1.0, "above max clamps to 1");
        assert_eq!(fraction(f32::NAN, 0.0, 1.0), 0.0, "NaN → 0 (finite)");
        assert_eq!(fraction(0.5, 1.0, 1.0), 0.0, "degenerate range → 0");
        assert!(
            fraction(0.5, 5.0, -5.0).is_finite(),
            "min>max is finite, no panic"
        );
    }

    #[test]
    fn snap_should_clamp_and_snap_to_step() {
        assert_eq!(
            snap(5.5, 0.0, 10.0, 2.0),
            6.0,
            "5.5 snaps to the nearest even step"
        );
        assert_eq!(snap(20.0, 0.0, 10.0, 2.0), 10.0, "clamps to max");
        assert_eq!(snap(-3.0, 0.0, 10.0, 2.0), 0.0, "clamps to min");
        assert_eq!(
            snap(3.3, 0.0, 10.0, 0.0),
            3.3,
            "continuous (step 0) does not snap"
        );
    }

    #[test]
    fn slider_drag_should_update_value() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(0.0);
        let mut h = harness(slider(value)); // range 0..1, thumb 18 (Md)
        // Drag to the track centre (x = SLOT/2). usable = SLOT - 18 = 182; frac = (100 - 9)/182 ≈ 0.5.
        drag(&mut h, &[SLOT / 2.0], 20.0);
        let v = value.get_untracked();
        assert!(
            (v - 0.5).abs() < 0.05,
            "a mid-track drag sets ~0.5 (got {v})"
        );
        drop(owner);
    }

    #[test]
    fn slider_click_should_set_value() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(0.0);
        let mut h = harness(slider(value));
        // A single mouse-down near the right of the track (inside it) jumps the value there (click-to-set).
        drag(&mut h, &[SLOT - 20.0], 20.0);
        let v = value.get_untracked();
        assert!(
            v > 0.85,
            "a click near the right jumps the value high (got {v})"
        );
        drop(owner);
    }

    #[test]
    fn slider_drag_past_end_should_clamp() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(0.5);
        let mut h = harness(slider(value));
        // Down inside the track, then drag the (captured) pointer past the right end → clamps to max.
        drag(&mut h, &[100.0, 400.0], 20.0);
        assert_eq!(
            value.get_untracked(),
            1.0,
            "dragging past the right end clamps to max"
        );
        // Down inside, drag past the left end → clamps to min.
        drag(&mut h, &[100.0, -50.0], 20.0);
        assert_eq!(
            value.get_untracked(),
            0.0,
            "dragging past the left end clamps to min"
        );
        drop(owner);
    }

    #[test]
    fn slider_arrow_should_step_and_clamp() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(4.0);
        let mut h = harness(slider(value).range(0.0, 10.0).step(2.0));
        let sl = slider_node(&h);
        press(&mut h, sl, KeyCode::ArrowRight);
        assert_eq!(value.get_untracked(), 6.0, "ArrowRight adds one step");
        press(&mut h, sl, KeyCode::ArrowDown);
        assert_eq!(value.get_untracked(), 4.0, "ArrowDown subtracts one step");
        press(&mut h, sl, KeyCode::End);
        assert_eq!(value.get_untracked(), 10.0, "End jumps to max");
        press(&mut h, sl, KeyCode::ArrowUp);
        assert_eq!(value.get_untracked(), 10.0, "stepping past max clamps");
        press(&mut h, sl, KeyCode::Home);
        assert_eq!(value.get_untracked(), 0.0, "Home jumps to min");
        press(&mut h, sl, KeyCode::PageUp);
        assert_eq!(
            value.get_untracked(),
            10.0,
            "PageUp moves by 10× step (20 → clamped to 10)"
        );
        drop(owner);
    }

    #[test]
    fn slider_external_write_should_move_the_filled_portion() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(0.25);
        let mut h = harness(slider(value));
        // filled is the slider's first child (horizontal: [filled, thumb, rest]).
        let filled = h.arena.get(slider_node(&h)).unwrap().children[0];
        let w_before = h.layout.absolute_layout(filled).size.w;
        value.set(0.75);
        h.root_id = frame(&mut h);
        let w_after = h.layout.absolute_layout(filled).size.w;
        assert!(
            w_after > w_before + 20.0,
            "raising the value widens the filled portion ({w_before} -> {w_after})"
        );
        drop(owner);
    }

    #[test]
    fn slider_vertical_drag_up_should_raise_value() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(0.5);
        // A vertical slider in a tall slot: dragging to the TOP (small y) raises toward max.
        let focus = FocusRegistry::new();
        provide_context(focus.clone());
        let root = div()
            .flex()
            .size(BSize { w: 40.0, h: SLOT })
            .child(slider(value).vertical())
            .into_element();
        let mut h = Harness {
            root,
            arena: Arena::new(),
            layout: LayoutTree::new(),
            text: TextSystem::new(FontDb::new()),
            hit: HitTest::new(),
            focus,
            damage: StdArc::new(DamageState::default()),
            theme: Theme::light(),
            root_id: NodeId::from_raw(0),
        };
        h.root_id = frame(&mut h);
        // Drag near the top (y ≈ 10) → value near max.
        let mut st = DispatchState::default();
        for kind in [
            MouseKind::Down(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
        ] {
            let ev = MouseEvent::new(
                kind,
                kagari_base::Point::new(20.0, 10.0),
                Modifiers::default(),
            );
            dispatch_mouse(&mut h.root, &h.arena, &h.hit, &ev, &mut st);
        }
        assert!(
            value.get_untracked() > 0.9,
            "dragging a vertical slider to the top raises the value (got {})",
            value.get_untracked()
        );
        drop(owner);
    }

    #[test]
    fn slider_disabled_should_be_inert() {
        let owner = Owner::new();
        owner.set();
        let value = RwSignal::new(0.3);
        let mut h = harness(slider(value).disabled(true));
        drag(&mut h, &[SLOT / 2.0], 20.0);
        let sl = slider_node(&h);
        press(&mut h, sl, KeyCode::ArrowRight);
        assert_eq!(
            value.get_untracked(),
            0.3,
            "a disabled slider ignores drag and keyboard"
        );
        drop(owner);
    }
}
