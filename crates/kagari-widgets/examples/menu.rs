//! Menu example (#77) — a manual visual check of the [`Menu`](kagari_widgets::Menu) widget.
//!
//! Run with: `cargo run -p kagari-widgets --example menu`
//!
//! Click "Menu ▾" to open a dropdown anchored below it. Move the highlight with the arrow keys (it skips
//! separators), press Enter to select, or click an item — either runs its action and closes the menu.
//! Press Esc or click outside to dismiss.

use kagari_core::reactive::RwSignal;
use kagari_core::reactive::prelude::*;
use kagari_core::{AnchorHandle, App, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::menu;

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(WindowOptions::default().title("kagari — menu"), || {
        let anchor = AnchorHandle::new();
        let open = RwSignal::new(false);
        div()
            .p_8()
            .bg(ColorRole::Surface)
            .child(
                // The trigger: tagged as the anchor + toggles `open`.
                div()
                    .anchor_ref(&anchor)
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(ColorRole::Accent)
                    .on_click(move |_, _| open.set(!open.get_untracked()))
                    .child(text("Menu \u{25be}").color_role(ColorRole::AccentFg)),
            )
            .child(
                menu(&anchor, open)
                    .item("New", || {})
                    .item("Open", || {})
                    .separator()
                    .item("Save", || {})
                    .item("Save As\u{2026}", || {})
                    .separator()
                    .item("Quit", || {}),
            )
    })?;
    app.run()
}
