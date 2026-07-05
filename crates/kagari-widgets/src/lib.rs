#![forbid(unsafe_code)]
//! kagari-widgets — the public widget set (specs §9). Widgets are variant-constructor builders that
//! return `impl IntoElement`, hide `Styled`, and compose kagari-core elements with kagari-style
//! semantic tokens (§3.6). Button (#70) is the first.

pub mod button;
pub mod checkbox;
pub mod control;
pub mod label;
pub mod radio;
pub mod switch;

pub use button::{Button, button};
pub use checkbox::{Checkbox, checkbox};
pub use control::ControlSize;
pub use label::{Label, label};
pub use radio::{RadioGroup, radio_group};
pub use switch::{Switch, switch};
