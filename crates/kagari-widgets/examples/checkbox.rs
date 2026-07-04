//! Checkbox example (#73) — a manual visual check of the [`checkbox`](kagari_widgets::checkbox) widget
//! bound to a `bool` signal. Opens a real window via the App shell (manual/local, like the `button`
//! example), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example checkbox`
//!
//! Click or press Space/Enter on a checkbox to toggle its bound signal; the check follows it (#245
//! reactive role colors + the #246 check icon). Disabled checkboxes are muted and inert.

use kagari_core::reactive::RwSignal;
use kagari_core::{App, WindowOptions, div};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{ControlSize, checkbox};

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — checkbox"),
        || {
            div()
                .flex_col()
                .gap_4()
                .p_4()
                .bg(ColorRole::Surface)
                // States: checked / unchecked / disabled.
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_4()
                        .child(checkbox(RwSignal::new(true)).label("Enabled"))
                        .child(checkbox(RwSignal::new(false)).label("Off"))
                        .child(checkbox(RwSignal::new(true)).disabled(true).label("Locked")),
                )
                // Sizes (Sm / Md / Lg).
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_4()
                        .child(
                            checkbox(RwSignal::new(true))
                                .size(ControlSize::Sm)
                                .label("Sm"),
                        )
                        .child(
                            checkbox(RwSignal::new(true))
                                .size(ControlSize::Md)
                                .label("Md"),
                        )
                        .child(
                            checkbox(RwSignal::new(true))
                                .size(ControlSize::Lg)
                                .label("Lg"),
                        ),
                )
        },
    )?;
    app.run()
}
