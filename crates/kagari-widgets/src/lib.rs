#![forbid(unsafe_code)]
//! kagari-widgets — the public widget set (specs §9). Widgets are variant-constructor builders that
//! return `impl IntoElement`, hide `Styled`, and compose kagari-core elements with kagari-style
//! semantic tokens (§3.6). Button (#70) is the first.

pub mod button;
pub mod control;

pub use button::{Button, button};
pub use control::ControlSize;
