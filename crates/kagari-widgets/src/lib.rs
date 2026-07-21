#![forbid(unsafe_code)]
//! kagari-widgets — the public widget set (specs §9). Widgets are variant-constructor builders that
//! return `impl IntoElement`, hide `Styled`, and compose kagari-core elements with kagari-style
//! semantic tokens (§3.6). Button (#70) is the first.

pub mod button;
pub mod checkbox;
pub mod context_menu;
pub mod control;
pub mod dialog;
pub mod dock;
pub mod dropdown;
pub mod field;
pub mod grid;
pub mod label;
pub mod list;
pub mod menu;
pub mod number_input;
pub mod panel;
pub mod popover;
pub mod progress;
pub mod radio;
pub mod scroll_view;
pub mod segmented;
pub mod spinner;
pub mod splitter;
pub mod switch;
pub mod table;
pub mod tabs;
pub mod text_input;
pub mod tooltip;
pub mod tree;

pub use button::{Button, button};
pub use checkbox::{Checkbox, checkbox};
pub use context_menu::{ContextMenu, context_menu};
pub use control::ControlSize;
pub use dialog::{Dialog, dialog};
pub use dock::{Dock, dock};
pub use dropdown::{Combobox, Dropdown, combobox, dropdown};
pub use field::{Field, FieldLayout, field};
pub use grid::{GridSelection, GridView, grid_view};
pub use label::{Label, label};
pub use list::{VirtualizedList, virtualized_list};
pub use menu::{Menu, menu};
pub use number_input::{NumberInput, number_input};
pub use panel::{Panel, panel};
pub use popover::{Popover, popover};
pub use progress::{Progress, progress};
pub use radio::{RadioGroup, radio_group};
pub use scroll_view::{ScrollView, scroll_view};
pub use segmented::{SegmentContent, Segmented, ToggleGroup, segmented, toggle_group};
pub use spinner::{Spinner, spinner};
pub use splitter::{Splitter, splitter};
pub use switch::{Switch, switch};
pub use table::{Selection, Table, table};
pub use tabs::{Tabs, tabs};
pub use text_input::{TextInput, text_input};
pub use tooltip::{Tooltip, tooltip};
pub use tree::{Tree, TreeNode, TreeSelection, tree};
