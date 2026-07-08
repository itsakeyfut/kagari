//! ContextMenu example (#78) — a manual visual check of the [`ContextMenu`](kagari_widgets::ContextMenu)
//! widget.
//!
//! Run with: `cargo run -p kagari-widgets --example context_menu`
//!
//! Right-click the "Right-click me" surface to open a menu at the cursor. Move the highlight with the arrow
//! keys (it skips separators), press Enter or click an item to select and close, or press Esc / click
//! outside to dismiss.

use kagari_core::reactive::RwSignal;
use kagari_core::{AnchorHandle, App, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{context_menu, menu};

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — context menu"),
        || {
            let open = RwSignal::new(false);
            div().p_8().bg(ColorRole::Surface).child(context_menu(
                // The right-click target: a bordered surface labelled for the demo.
                div()
                    .min_w_40()
                    .p_8()
                    .rounded_md()
                    .bg(ColorRole::SurfaceRaised)
                    .border_w_1()
                    .border_color(Some(ColorRole::Border))
                    .child(text("Right-click me").color_role(ColorRole::Text)),
                // The anchor is unused (the menu opens at the pointer); `open` is its visibility.
                menu(&AnchorHandle::new(), open)
                    .item("Copy", || {})
                    .item("Paste", || {})
                    .separator()
                    .item("Delete", || {}),
            ))
        },
    )?;
    app.run()
}
