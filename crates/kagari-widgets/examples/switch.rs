//! Switch example (#73) — a manual visual check of the [`switch`](kagari_widgets::switch) widget bound
//! to a `bool` signal. Opens a real window via the App shell (manual/local, like the `button` example),
//! not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example switch`
//!
//! Click or press Space/Enter on a switch to toggle its bound signal; the knob slides to follow it (#245
//! reactive role colors). Disabled switches are muted and inert.

use kagari_core::reactive::RwSignal;
use kagari_core::{App, WindowOptions, div};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{ControlSize, switch};

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(WindowOptions::default().title("kagari — switch"), || {
        div()
            .flex_col()
            .gap_4()
            .p_4()
            .bg(ColorRole::Surface)
            // States: on / off / disabled.
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(switch(RwSignal::new(true)).label("Dark mode"))
                    .child(switch(RwSignal::new(false)).label("Off"))
                    .child(switch(RwSignal::new(true)).disabled(true).label("Locked")),
            )
            // Sizes (Sm / Md / Lg).
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(switch(RwSignal::new(true)).size(ControlSize::Sm))
                    .child(switch(RwSignal::new(true)).size(ControlSize::Md))
                    .child(switch(RwSignal::new(true)).size(ControlSize::Lg)),
            )
    })?;
    app.run()
}
