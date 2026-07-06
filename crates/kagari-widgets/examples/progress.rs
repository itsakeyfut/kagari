//! Progress / Spinner example (#87) — a manual visual check of the [`progress`](kagari_widgets::progress)
//! and [`spinner`](kagari_widgets::spinner) widgets. Opens a real window via the App shell (manual/local,
//! like the `label` example), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example progress`
//!
//! The progress bars fill (animating) to their value; the spinner's segment bounces continuously.

use kagari_core::{App, WindowOptions, div};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{ControlSize, progress, spinner};

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — progress / spinner"),
        || {
            div()
                .flex_col()
                .gap_4()
                .p_4()
                .bg(ColorRole::Surface)
                // Determinate bars at a few values.
                .child(progress(0.25))
                .child(progress(0.6))
                .child(progress(0.9).size(ControlSize::Lg))
                // Indeterminate spinners.
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_4()
                        .child(spinner().size(ControlSize::Sm))
                        .child(spinner())
                        .child(spinner().size(ControlSize::Lg)),
                )
        },
    )?;
    app.run()
}
