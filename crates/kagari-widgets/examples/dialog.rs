//! Dialog example (#82) — a manual visual check of the [`Dialog`](kagari_widgets::Dialog) widget.
//!
//! Run with: `cargo run -p kagari-widgets --example dialog`
//!
//! Click "Open dialog" to open a modal: a dimmed backdrop + a centered card that scales in. Click the
//! "Close" button, press Esc, or click the backdrop to close it — focus returns to the trigger.

use kagari_core::reactive::RwSignal;
use kagari_core::reactive::prelude::*;
use kagari_core::{App, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::dialog;

fn button(label: &'static str) -> impl kagari_core::IntoElement {
    div()
        .px_4()
        .py_2()
        .rounded_md()
        .bg(ColorRole::Accent)
        .child(text(label).color_role(ColorRole::AccentFg))
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(WindowOptions::default().title("kagari — dialog"), || {
        let open = RwSignal::new(false);
        div()
            .p_8()
            .bg(ColorRole::Surface)
            .child(
                div()
                    .child(button("Open dialog"))
                    .on_click(move |_, _| open.set(true)),
            )
            .child(dialog(open, move || {
                // A modal card — bordered + shadowed so it reads as a raised surface (RK-030).
                div()
                    .flex_col()
                    .gap_4()
                    .p_6()
                    .rounded_lg()
                    .bg(ColorRole::SurfaceRaised)
                    .border_w_1()
                    .border_color(Some(ColorRole::Border))
                    .shadow_lg()
                    .child(text("Delete item?").color_role(ColorRole::Text))
                    .child(text("This action cannot be undone.").color_role(ColorRole::TextMuted))
                    .child(
                        div()
                            .child(button("Close"))
                            .on_click(move |_, _| open.set(false)),
                    )
            }))
    })?;
    app.run()
}
