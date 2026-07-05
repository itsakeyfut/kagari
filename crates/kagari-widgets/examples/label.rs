//! Label example (#71) — a manual visual check of the [`label`](kagari_widgets::label) widget. Opens a
//! real window via the App shell (manual/local, like the `button` example), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example label`
//!
//! Labels are display-only text: theme-aware color roles (`.color`/`.muted`) and the shared control size
//! scale (`.size`). No wrapping yet (tracked in #260).

use kagari_core::{App, WindowOptions, div};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{ControlSize, label};

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(WindowOptions::default().title("kagari — label"), || {
        div()
            .flex_col()
            .gap_4()
            .p_4()
            .bg(ColorRole::Surface)
            // Color roles.
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(label("Default text"))
                    .child(label("Muted").muted())
                    .child(label("Accent").color(ColorRole::Accent))
                    .child(label("Danger").color(ColorRole::Danger)),
            )
            // Sizes (Sm / Md / Lg).
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(label("Small").size(ControlSize::Sm))
                    .child(label("Medium").size(ControlSize::Md))
                    .child(label("Large").size(ControlSize::Lg)),
            )
    })?;
    app.run()
}
