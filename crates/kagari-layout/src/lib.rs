#![forbid(unsafe_code)]
//! kagari-layout — taffy wrapper (+ scroll, virtualization to come).
//!
//! `LayoutStyle` (#31) maps onto a `taffy::Style`; `LayoutTree` (#33) wraps `taffy::TaffyTree`,
//! mapping kagari `NodeId`s to taffy nodes and running measurement for leaves. taffy is an
//! implementation detail — the public surface uses kagari-owned types only. Scroll and
//! virtualization follow.

pub mod error;
pub mod measure;
pub mod style;
pub mod tree;

pub use error::LayoutError;
pub use measure::{AvailableSize, MeasureFn};
pub use style::{
    AlignItems, Display, FlexDirection, FlexWrap, JustifyContent, LayoutStyle, Overflow,
};
pub use tree::LayoutTree;
