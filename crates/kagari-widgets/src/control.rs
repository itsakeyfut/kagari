//! Shared control primitives (D3): the [`ControlSize`] scale every interactive widget accepts, the
//! per-size layout/label metrics that map it onto the kagari-style spacing scale (§3.6), and the shared
//! builders + focusable root that [`Checkbox`](crate::Checkbox) and [`Switch`](crate::Switch) compose.

use kagari_base::{Px, SharedString};
use kagari_core::element::Div;
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{AnimationSpec, KeyCode, Role, div, text, use_focus_handle};
use kagari_style::{ColorRole, Styled};

/// The spring for control transitions (#253) — snappy and near-critically-damped so a toggle (switch
/// knob slide, checkbox check pop) settles quickly with only a subtle overshoot.
pub(crate) const CONTROL_SPRING: AnimationSpec = AnimationSpec::Spring {
    stiffness: 500.0,
    damping: 40.0,
};

/// The size of an interactive control (button / input / checkbox / …), shared across kagari-widgets so a
/// form's controls line up on one scale (D3). `Md` is the default.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ControlSize {
    /// Compact (dense toolbars / inline).
    Sm,
    /// The default control size.
    #[default]
    Md,
    /// Prominent (primary actions / touch).
    Lg,
}

/// Applies a control's padding + inner gap for `size` onto `d` (kagari-style spacing steps).
pub(crate) fn apply_size(d: Div, size: ControlSize) -> Div {
    match size {
        ControlSize::Sm => d.px_2().py_1().gap_1(),
        ControlSize::Md => d.px_3().py_2().gap_2(),
        ControlSize::Lg => d.px_4().py_3().gap_2(),
    }
}

/// The label font size (logical px) for `size`. Set on the label leaf directly — a parent `text_*`
/// (font-size) token does not propagate into a child [`text`](kagari_core::text) leaf.
pub(crate) fn label_px(size: ControlSize) -> Px {
    match size {
        ControlSize::Sm => Px(14.0),
        ControlSize::Md => Px(16.0),
        ControlSize::Lg => Px(18.0),
    }
}

/// A sane default min width (logical px) for `size` so a standalone empty text field is usable; it still
/// fills a wider slot (the editor flex-grows) and only floors the narrow case.
pub(crate) fn field_min_w(size: ControlSize) -> f32 {
    match size {
        ControlSize::Sm => 120.0,
        ControlSize::Md => 160.0,
        ControlSize::Lg => 200.0,
    }
}

/// The shared bordered / rounded / padded field frame for text-entry widgets (TextInput #72, NumberInput
/// #76): size-driven padding + a min width, a `Surface` bg, and a 2px border. The caller sets the border
/// color (a reactive `FocusRing` when enabled, static `Border` when disabled), the role, and the children.
pub(crate) fn field_frame(size: ControlSize) -> Div {
    apply_size(div(), size)
        .min_w(field_min_w(size))
        .rounded_md()
        .bg(ColorRole::Surface)
        .border_w_2()
}

/// The checkbox's square box side (logical px) for `size` — sized near the label cap height so the box
/// and its label align.
pub(crate) fn checkbox_box_px(size: ControlSize) -> Px {
    match size {
        ControlSize::Sm => Px(16.0),
        ControlSize::Md => Px(18.0),
        ControlSize::Lg => Px(22.0),
    }
}

/// The check-glyph side (logical px) for `size` — a little smaller than the box so the ✓ sits inside it.
pub(crate) fn check_glyph_px(size: ControlSize) -> Px {
    match size {
        ControlSize::Sm => Px(12.0),
        ControlSize::Md => Px(14.0),
        ControlSize::Lg => Px(16.0),
    }
}

/// The switch metrics `(track_w, track_h, knob)` (logical px) for `size`. The knob is inset 2px on all
/// sides (so `track_h = knob + 4`), and its ON travel is `track_w - knob - 4`.
pub(crate) fn switch_dims(size: ControlSize) -> (Px, Px, Px) {
    match size {
        ControlSize::Sm => (Px(28.0), Px(16.0), Px(12.0)),
        ControlSize::Md => (Px(34.0), Px(20.0), Px(16.0)),
        ControlSize::Lg => (Px(42.0), Px(24.0), Px(20.0)),
    }
}

/// The radio mark's outer-circle diameter (logical px) for `size` — matched to the checkbox box scale.
pub(crate) fn radio_mark_px(size: ControlSize) -> Px {
    checkbox_box_px(size)
}

/// The slider metrics `(track_thickness, thumb)` (logical px) for `size` (#75): a thin track with a larger
/// knob (the thumb overflows the track on the cross axis). The thumb doubles as the drag "divider", so its
/// size sets the usable travel (`track_len − thumb`).
pub(crate) fn slider_dims(size: ControlSize) -> (Px, Px) {
    match size {
        ControlSize::Sm => (Px(4.0), Px(14.0)),
        ControlSize::Md => (Px(6.0), Px(18.0)),
        ControlSize::Lg => (Px(8.0), Px(22.0)),
    }
}

/// A value coerced into `[min, max]` (a non-finite value → `min`); guards a degenerate range (`min > max`)
/// so `f32::clamp` (which panics when `min > max`) is never hit and no `flex_grow` gets a NaN (RK-045). The
/// shared slider sanitiser: used by both [`Slider`](crate::Slider) (#75) and [`RangeSlider`](crate::RangeSlider) (#234).
pub(crate) fn sane(v: f32, min: f32, max: f32) -> f32 {
    if !v.is_finite() {
        return min;
    }
    if min <= max { v.clamp(min, max) } else { min }
}

/// The `[0, 1]` fill fraction of `v` in `[min, max]` — always finite (RK-045: fed straight into `flex_grow`,
/// where a NaN/negative would collapse the segment). A degenerate range yields `0`.
pub(crate) fn fraction(v: f32, min: f32, max: f32) -> f32 {
    if max > min {
        ((sane(v, min, max) - min) / (max - min)).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// `v` clamped to `[min, max]` and snapped to the nearest `step` (a `<= 0`/non-finite step = continuous).
pub(crate) fn snap(v: f32, min: f32, max: f32, step: f32) -> f32 {
    let clamped = sane(v, min, max);
    if step > 0.0 && step.is_finite() && max > min {
        (min + ((clamped - min) / step).round() * step).clamp(min, max)
    } else {
        clamped
    }
}

/// The progress-bar track `(width, height)` (logical px) for `size` — a thin, wide pill (#87).
pub(crate) fn progress_dims(size: ControlSize) -> (Px, Px) {
    match size {
        ControlSize::Sm => (Px(96.0), Px(4.0)),
        ControlSize::Md => (Px(128.0), Px(6.0)),
        ControlSize::Lg => (Px(160.0), Px(8.0)),
    }
}

/// The spinner metrics `(track_w, track_h, segment_w)` (logical px) for `size` — an indeterminate pill
/// with a segment that bounces across the track (#87).
pub(crate) fn spinner_dims(size: ControlSize) -> (Px, Px, Px) {
    match size {
        ControlSize::Sm => (Px(48.0), Px(4.0), Px(16.0)),
        ControlSize::Md => (Px(64.0), Px(6.0), Px(22.0)),
        ControlSize::Lg => (Px(80.0), Px(8.0), Px(28.0)),
    }
}

/// Seconds per spinner bounce cycle (#87) — how long the segment takes to travel across and back.
pub(crate) const SPINNER_PERIOD: f32 = 1.2;

/// The radio mark's inner-dot diameter (logical px) for `size` — about half the outer circle. Kept
/// **even** (like the even outer diameters from `checkbox_box_px`) so `(outer − dot)/2` is a whole
/// number: an odd dot in an even circle centers at a half-pixel and snaps off-center on the pixel grid.
pub(crate) fn radio_dot_px(size: ControlSize) -> Px {
    match size {
        ControlSize::Sm => Px(8.0),
        ControlSize::Md => Px(10.0),
        ControlSize::Lg => Px(12.0),
    }
}

/// Adds the shared `size`/`disabled`/`label` builder methods to a control type that has those three
/// fields ([`Checkbox`](crate::Checkbox) / [`Switch`](crate::Switch)) — one definition for both.
macro_rules! control_builders {
    ($t:ty) => {
        impl $t {
            /// Sets the control size (D3).
            pub fn size(mut self, size: ControlSize) -> Self {
                self.size = size;
                self
            }

            /// Disables the control: muted styling, inert (no toggle), and not focusable.
            pub fn disabled(mut self, disabled: bool) -> Self {
                self.disabled = disabled;
                self
            }

            /// Adds a trailing text label (clickable together with the control).
            pub fn label(mut self, label: impl Into<SharedString>) -> Self {
                self.label = Some(label.into());
                self
            }
        }
    };
}
pub(crate) use control_builders;

/// Wraps a control's `indicator` (checkbox box / switch track) with the shared root: a focusable,
/// clickable row with an optional label. The focus ring (a reactive `FocusRing` border) and
/// focusability are attached only when enabled (RK-027); activation (click **or** Space/Enter) flips
/// `value`. Mirrors [`Button`](crate::Button).
pub(crate) fn control_root(
    value: RwSignal<bool>,
    size: ControlSize,
    disabled: bool,
    label: Option<SharedString>,
    role: Role,
    indicator: Div,
) -> Div {
    // `a11y_checked` is reactive and set even when disabled — a disabled control still announces its
    // on/off state to a screen reader (#257).
    let mut root = div()
        .flex()
        .items_center()
        .gap_2()
        .role(role)
        .a11y_checked(rx(move || value.get()));
    if let Some(l) = &label {
        root = root.a11y_label(l.clone());
    }
    if !disabled {
        let handle = use_focus_handle();
        let h = handle.clone();
        root = root
            .rounded_md()
            .border_w_2()
            .border_color(rx(move || {
                h.is_focus_visible().then_some(ColorRole::FocusRing)
            }))
            .track_focus(&handle)
            .on_click(move |_ev, _cx| value.set(!value.get_untracked()))
            .on_key_down(move |kev, _cx| {
                if !kev.repeat
                    && matches!(
                        kev.code,
                        KeyCode::Space | KeyCode::Enter | KeyCode::NumpadEnter
                    )
                {
                    value.set(!value.get_untracked());
                }
            });
    }
    root = root.child(indicator);
    if let Some(l) = label {
        let color = if disabled {
            ColorRole::TextMuted
        } else {
            ColorRole::Text
        };
        root = root.child(text(l).color_role(color).size(label_px(size)));
    }
    root
}
