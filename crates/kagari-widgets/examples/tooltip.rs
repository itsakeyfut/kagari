//! Tooltip example (#80) — a manual visual check of the [`Tooltip`](kagari_widgets::Tooltip) widget.
//!
//! Run with: `cargo run -p kagari-widgets --example tooltip`
//!
//! Hover the pointer over either chip and hold still: after a short delay a label bubble appears anchored
//! over it (the second uses a longer delay). Move the pointer away and the bubble hides. The App shell
//! provides the per-window ticker that drives the hover delay.

use std::time::Duration;

use kagari_core::{App, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::tooltip;

fn chip(label: &'static str) -> impl kagari_core::IntoElement {
    div()
        .px_4()
        .py_2()
        .rounded_md()
        .bg(ColorRole::Accent)
        .child(text(label).color_role(ColorRole::AccentFg))
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(WindowOptions::default().title("kagari — tooltip"), || {
        div()
            .flex_col()
            .gap_6()
            .p_8()
            .bg(ColorRole::Surface)
            .child(tooltip(
                chip("Hover me"),
                "This is a tooltip shown on hover.",
            ))
            .child(
                tooltip(chip("Slow tooltip"), "This one waits a full second.")
                    .delay(Duration::from_millis(1000)),
            )
    })?;
    app.run()
}
