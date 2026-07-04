//! Checkbox & Switch example (#73) — a manual visual check of the [`checkbox`](kagari_widgets::checkbox)
//! and [`switch`](kagari_widgets::switch) widgets bound to a `bool` signal. Opens a real window via the
//! App shell (manual/local, like the `button` example), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example checkbox`
//!
//! Click or press Space/Enter on a control to toggle its bound signal; the check/knob follows it (#245
//! reactive role colors + the #246 check icon). Disabled controls are muted and inert.

use kagari_core::reactive::RwSignal;
use kagari_core::{App, WindowOptions, div};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{ControlSize, checkbox, switch};

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — checkbox & switch"),
        || {
            div()
                .flex_col()
                .gap_4()
                .p_4()
                .bg(ColorRole::Surface)
                // States: checked / unchecked / disabled + on/off switches.
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_4()
                        .child(checkbox(RwSignal::new(true)).label("Enabled"))
                        .child(checkbox(RwSignal::new(false)).label("Off"))
                        .child(checkbox(RwSignal::new(true)).disabled(true).label("Locked"))
                        .child(switch(RwSignal::new(true)).label("Dark mode"))
                        .child(switch(RwSignal::new(false))),
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
                        )
                        .child(switch(RwSignal::new(true)).size(ControlSize::Sm))
                        .child(switch(RwSignal::new(true)).size(ControlSize::Md))
                        .child(switch(RwSignal::new(true)).size(ControlSize::Lg)),
                )
        },
    )?;
    app.run()
}
