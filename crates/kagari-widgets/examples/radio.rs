//! Radio example (#74) — a manual visual check of the [`RadioGroup`](kagari_widgets::RadioGroup)
//! widget. Opens a real window via the App shell, so it is a **manual/local** check (like the
//! `button`/`checkbox` examples), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example radio`
//!
//! Each group binds one `RwSignal<T>`: clicking an option selects it (exclusive), and — while the
//! group is focused (Tab, once focus navigation is on) — the arrow keys move the selection. The
//! selected option shows an Accent dot, and the focus-ring follows it.

use kagari_core::reactive::RwSignal;
use kagari_core::{App, WindowOptions, div};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{ControlSize, radio_group};

#[derive(Clone, Copy, PartialEq)]
enum Quality {
    Draft,
    Preview,
    Full,
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(WindowOptions::default().title("kagari — radio"), || {
        let quality = RwSignal::new(Quality::Preview);
        let size = RwSignal::new(1_i32);
        div()
            .flex_col()
            .gap_4()
            .p_4()
            .bg(ColorRole::Surface)
            // A typed enum group (exclusive selection binds the signal).
            .child(
                radio_group(quality)
                    .option(Quality::Draft, "Draft")
                    .option(Quality::Preview, "Preview")
                    .option(Quality::Full, "Full quality"),
            )
            // Sizes on a second group.
            .child(
                radio_group(size)
                    .size(ControlSize::Lg)
                    .option(0, "Low")
                    .option(1, "Medium")
                    .option(2, "High"),
            )
            // A disabled group (muted, inert).
            .child(
                radio_group(RwSignal::new(0_i32))
                    .disabled(true)
                    .option(0, "Locked A")
                    .option(1, "Locked B"),
            )
    })?;
    app.run()
}
