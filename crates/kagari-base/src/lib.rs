#![forbid(unsafe_code)]
//! Shared base value types and their operations for kagari.

pub mod color;
pub mod geometry;
pub mod id;
pub mod math;
pub mod string;
pub mod unit;

pub use color::{Color, ColorSpace, TaggedColor, Transfer};
pub use geometry::{Corners, Edges, Point, Rect, Size};
pub use id::{NodeId, WindowId};
pub use string::SharedString;
pub use unit::Px;
