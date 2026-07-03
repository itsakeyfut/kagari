//! Shared control primitives (D3): the [`ControlSize`] scale every interactive widget accepts, plus the
//! per-size layout/label metrics that map it onto the kagari-style spacing scale (§3.6).

use kagari_base::Px;
use kagari_core::element::Div;
use kagari_style::Styled;

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
