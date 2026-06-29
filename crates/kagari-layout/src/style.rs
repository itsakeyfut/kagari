//! Authoring-level layout style (#31).
//!
//! Minimal inputs for a flex container — the values a builder records. #33 maps these onto a
//! taffy `Style`, builds the layout tree, and runs measure; this type is the stable input it
//! consumes.

use kagari_base::{Px, Size};

/// Main-axis direction of a flex container.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FlexDirection {
    /// Lay children out along the horizontal axis (the flex default).
    #[default]
    Row,
    /// Lay children out along the vertical axis.
    Column,
}

/// The layout intent of an element: flex direction, child gap, and an optional fixed size.
/// Resolved into a taffy `Style` by #33.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct LayoutStyle {
    pub flex_direction: FlexDirection,
    pub gap: Px,
    pub size: Option<Size>,
}
