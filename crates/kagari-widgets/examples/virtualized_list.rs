//! VirtualizedList example (#86) — a manual visual check of the
//! [`virtualized_list`](kagari_widgets::virtualized_list) widget. Opens a real window via the App shell, so
//! it is a **manual/local** check (like the `dropdown` / `text_input` examples), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example virtualized_list`
//!
//! A 10,000-row list in a fixed viewport — only the ~visible rows are ever built. Scroll with the mouse
//! wheel; the window recycles rows as you go (the realized element count stays small regardless of the
//! 10,000 total).

use kagari_base::{Px, Size};
use kagari_core::{AnyElement, App, IntoElement, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::virtualized_list;

fn row(index: usize) -> AnyElement {
    // Alternating row tint so scrolling / recycling is visible.
    let bg = if index % 2 == 0 {
        ColorRole::Surface
    } else {
        ColorRole::SurfaceRaised
    };
    div()
        .size(Size { w: 320.0, h: 28.0 })
        .px_3()
        .items_center()
        .bg(bg)
        .child(
            text(format!("Row {index}"))
                .color_role(ColorRole::Text)
                .size(Px(15.0)),
        )
        .into_element()
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — virtualized list (10k rows)"),
        || {
            div()
                .p_4()
                .bg(ColorRole::Surface)
                .child(virtualized_list(10_000, 28.0, row).size(Size { w: 320.0, h: 440.0 }))
        },
    )?;
    app.run()
}
