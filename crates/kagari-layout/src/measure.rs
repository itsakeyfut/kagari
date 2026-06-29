//! Leaf intrinsic-size measurement (#33).
//!
//! A leaf (text, image, intrinsic canvas) reports its size through a [`MeasureFn`]. kagari-layout
//! does not depend on kagari-text; the text measure closure is supplied by the caller (kagari-core
//! wires it from the shaper in #34), keeping this crate's deps on the allowed downward path.

use kagari_base::Size;

/// The space taffy offers a leaf when asking for its intrinsic size. `None` on an axis means the
/// available space is unconstrained (min/max-content); `Some(px)` is a definite or known length.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct AvailableSize {
    pub width: Option<f32>,
    pub height: Option<f32>,
}

/// Reports a leaf's intrinsic size for the given available space.
pub type MeasureFn = Box<dyn Fn(AvailableSize) -> Size + 'static>;
