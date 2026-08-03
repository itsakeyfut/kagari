//! CommandPalette example (#232) — a manual visual check of the
//! [`command_palette`](kagari_widgets::command_palette) widget. Opens a real window via the App shell (a
//! manual/local check, not a CI test).
//!
//! Run with: `cargo run -p kagari-widgets --example command_palette`
//!
//! Click **Open Palette** to show a centered modal fuzzy-search palette. **Type** to filter/rank commands
//! (matched characters are highlighted); **↑/↓** move the selection; **Enter** runs it (updating the status
//! line); **Esc** or an outside click dismisses. Some commands also match search aliases (e.g. "write" → Save).

use kagari_base::{Px, SharedString, Size};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{App, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{button, command, command_palette};

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default()
            .title("kagari — command palette (click Open, then type)")
            .inner_size(Size { w: 640.0, h: 480.0 }),
        || {
            let open = RwSignal::new(false);
            let status = RwSignal::new(String::from("No command run yet."));
            let run = move |name: &str| status.set(format!("Ran: {name}"));

            let commands = vec![
                command("Save File", move || run("Save File")).keyword("write"),
                command("Save As…", move || run("Save As…")).keyword("export"),
                command("Open File", move || run("Open File")).keyword("load"),
                command("Close Tab", move || run("Close Tab")),
                command("Toggle Sidebar", move || run("Toggle Sidebar")).keyword("panel"),
                command("Split Editor", move || run("Split Editor")),
                command("Zoom In", move || run("Zoom In")),
                command("Zoom Out", move || run("Zoom Out")),
                command("Command: Reload Window", move || run("Reload Window")).keyword("restart"),
            ];

            div()
                .flex_col()
                .gap_3()
                .p_4()
                .bg(ColorRole::Bg)
                .child(
                    text("CommandPalette demo")
                        .color_role(ColorRole::Text)
                        .size(Px(18.0)),
                )
                .child(button("Open Palette").on_click(move || open.set(true)))
                .child(
                    text(rx(move || SharedString::from(status.get())))
                        .color_role(ColorRole::TextMuted)
                        .size(Px(14.0)),
                )
                .child(command_palette(open, commands))
        },
    )?;
    app.run()
}
