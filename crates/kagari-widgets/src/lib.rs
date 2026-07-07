#![forbid(unsafe_code)]
//! kagari-widgets — the public widget set (specs §9). Widgets are variant-constructor builders that
//! return `impl IntoElement`, hide `Styled`, and compose kagari-core elements with kagari-style
//! semantic tokens (§3.6). Button (#70) is the first.

pub mod button;
pub mod checkbox;
pub mod control;
pub mod dialog;
pub mod field;
pub mod label;
pub mod menu;
pub mod popover;
pub mod progress;
pub mod radio;
pub mod segmented;
pub mod spinner;
pub mod switch;
pub mod tabs;
pub mod tooltip;

pub use button::{Button, button};
pub use checkbox::{Checkbox, checkbox};
pub use control::ControlSize;
pub use dialog::{Dialog, dialog};
pub use field::{Field, FieldLayout, field};
pub use label::{Label, label};
pub use menu::{Menu, menu};
pub use popover::{Popover, popover};
pub use progress::{Progress, progress};
pub use radio::{RadioGroup, radio_group};
pub use segmented::{SegmentContent, Segmented, ToggleGroup, segmented, toggle_group};
pub use spinner::{Spinner, spinner};
pub use switch::{Switch, switch};
pub use tabs::{Tabs, tabs};
pub use tooltip::{Tooltip, tooltip};
