//! Segmented / ToggleGroup example (#230) — a manual visual check of the
//! [`Segmented`](kagari_widgets::Segmented) and [`ToggleGroup`](kagari_widgets::ToggleGroup) widgets.
//! Opens a real window via the App shell, so it is a **manual/local** check (like the
//! `radio`/`checkbox` examples), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example segmented`
//!
//! `Segmented` binds one `RwSignal<T>` (exclusive: clicking a segment selects it, arrows move the
//! selection while focused). `ToggleGroup` binds a `RwSignal<HashSet<T>>` (multi-select: each segment
//! toggles in/out; arrows move a roving focus and Space/Enter toggles the focused segment).

use std::collections::HashSet;

use kagari_core::reactive::RwSignal;
use kagari_core::{App, IconId, WindowOptions, div};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{ControlSize, segmented, toggle_group};

#[derive(Clone, Copy, PartialEq)]
enum ViewMode {
    Grid,
    List,
    Columns,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum TextStyle {
    Bold,
    Italic,
    Underline,
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — segmented"),
        || {
            let view = RwSignal::new(ViewMode::Grid);
            let space = RwSignal::new(0_i32);
            let styles = RwSignal::new(HashSet::<TextStyle>::new());
            div()
                .flex_col()
                .gap_4()
                .p_4()
                .bg(ColorRole::Surface)
                // Exclusive segmented control (label segments).
                .child(
                    segmented(view)
                        .segment(ViewMode::Grid, "Grid")
                        .segment(ViewMode::List, "List")
                        .segment(ViewMode::Columns, "Columns"),
                )
                // A larger exclusive control with icon segments.
                .child(
                    segmented(space)
                        .size(ControlSize::Lg)
                        .segment(0, IconId::Check)
                        .segment(1, IconId::Close)
                        .segment(2, IconId::ChevronDown),
                )
                // Multi-select toggle group.
                .child(
                    toggle_group(styles)
                        .segment(TextStyle::Bold, "B")
                        .segment(TextStyle::Italic, "I")
                        .segment(TextStyle::Underline, "U"),
                )
                // A disabled segmented control (muted, inert).
                .child(
                    segmented(RwSignal::new(0_i32))
                        .disabled(true)
                        .segment(0, "Locked A")
                        .segment(1, "Locked B"),
                )
        },
    )?;
    app.run()
}
