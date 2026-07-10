//! TextEdit example (#298) — a manual visual check of the [`text_edit`](kagari_core::text_edit) editable
//! leaf.
//!
//! Run with: `cargo run -p kagari-core --example text_edit`
//!
//! Click the field to focus + place the caret, type to insert (IME too), arrow keys / Home / End to move
//! (Shift to select, Ctrl for word), Backspace / Delete to erase, Ctrl+A select-all, Ctrl+C/X/V clipboard,
//! Ctrl+Z / Ctrl+Y undo/redo. Single-line.

use kagari_core::reactive::RwSignal;
use kagari_core::{App, WindowOptions, div, text_edit};
use kagari_style::{ColorRole, Styled};

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — text edit"),
        || {
            let value = RwSignal::new("Edit me".to_string());
            div().p_8().bg(ColorRole::Surface).child(
                // A bordered field surface; the #72 TextInput widget will add placeholder / size / disabled.
                div()
                    .min_w_40()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(ColorRole::SurfaceRaised)
                    .border_w_1()
                    .border_color(Some(ColorRole::Border))
                    .child(text_edit(value)),
            )
        },
    )?;
    app.run()
}
