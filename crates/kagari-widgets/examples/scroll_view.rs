//! ScrollView example (#83) — a manual visual check of the [`scroll_view`](kagari_widgets::scroll_view)
//! widget. Opens a real window via the App shell, so it is a **manual/local** check (like the `table` / `tree`
//! / `grid` examples), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example scroll_view`
//!
//! A tall column of rows that overflows the viewport: scroll with the mouse wheel, or **drag the overlay
//! scrollbar thumb** on the right edge (the grab point tracks the cursor). Only the visible region is clipped
//! to the viewport; the scrollbar reflects the position.

use kagari_base::{Px, Size};
use kagari_core::{App, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::scroll_view;

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — scroll_view"),
        || {
            // Tall content: 40 labeled rows stacked in a column, overflowing the 400px viewport.
            let mut col = div().flex_col().gap(Px(4.0)).p_2();
            for i in 0..40 {
                col = col.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(ColorRole::SurfaceRaised)
                        .child(
                            text(format!("Row {i}"))
                                .size(Px(14.0))
                                .color_role(ColorRole::Text),
                        ),
                );
            }
            div()
                .p_4()
                .bg(ColorRole::Surface)
                .child(scroll_view(col).size(Size { w: 320.0, h: 400.0 }))
        },
    )?;
    app.run()
}
