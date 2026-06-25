//! Shared base value types and their operations for kagari.

pub mod color;
pub mod geometry;
pub mod unit;

pub use color::{Color, ColorSpace, TaggedColor, Transfer};
pub use geometry::{Corners, Edges, Point, Rect, Size};
pub use unit::Px;
