//! TextInput example (#72) — a manual visual check of the [`text_input`](kagari_widgets::text_input)
//! widget. Opens a real window via the App shell, so it is a **manual/local** check (like the
//! `field`/`segmented` examples), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example text_input`
//!
//! Click a field to focus + place the caret, type to insert (IME too); Tab moves focus (keyboard focus
//! shows the ring). The placeholder shows (muted) while a field is empty and unfocused. The last row is
//! disabled (muted, display-only), and one is wrapped in a labeled `Field`.

use kagari_core::reactive::RwSignal;
use kagari_core::{App, WindowOptions, div};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{ControlSize, field, text_input};

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — text input"),
        || {
            let name = RwSignal::new(String::new());
            let email = RwSignal::new("editor@example.com".to_string());
            let small = RwSignal::new(String::new());
            let large = RwSignal::new(String::new());
            let locked = RwSignal::new("read-only value".to_string());

            div()
                .flex_col()
                .gap_4()
                .p_4()
                .bg(ColorRole::Surface)
                // Placeholder (shown while empty + unfocused).
                .child(text_input(name).placeholder("Your name"))
                // Pre-filled value.
                .child(text_input(email))
                // Sizes.
                .child(text_input(small).size(ControlSize::Sm).placeholder("Small"))
                .child(text_input(large).size(ControlSize::Lg).placeholder("Large"))
                // Disabled (display-only, muted).
                .child(text_input(locked).disabled(true))
                // Inside a labeled Field (inspector row).
                .child(field(
                    "Search",
                    text_input(RwSignal::new(String::new())).placeholder("Filter…"),
                ))
        },
    )?;
    app.run()
}
