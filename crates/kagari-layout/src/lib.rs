#![forbid(unsafe_code)]
//! kagari-layout — layout style + (taffy wrapper, scroll, virtualization to come).
//!
//! So far: the authoring-level [`LayoutStyle`] (#31). The taffy wrapper, layout tree, and
//! measure land in #33.

pub mod style;

pub use style::{FlexDirection, LayoutStyle};
