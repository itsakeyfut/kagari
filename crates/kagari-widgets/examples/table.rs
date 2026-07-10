//! Table example (#224) — a manual visual check of the [`table`](kagari_widgets::table) widget. Opens a real
//! window via the App shell, so it is a **manual/local** check (like the `dropdown` / `virtualized_list`
//! examples), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example table`
//!
//! A 500-row table with three columns and a sticky header; the body virtualizes (only the visible rows are
//! built) and scrolls with the mouse wheel. Click a row to select it (the selected row tints Accent).

use kagari_base::Px;
use kagari_core::reactive::RwSignal;
use kagari_core::{App, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{Selection, table};

struct Clip {
    name: String,
    duration: String,
    kind: &'static str,
}

fn clips(n: usize) -> Vec<Clip> {
    (0..n)
        .map(|i| Clip {
            name: format!("clip_{i:03}.mov"),
            duration: format!("{}:{:02}", i / 60, i % 60),
            kind: if i % 3 == 0 { "video" } else { "audio" },
        })
        .collect()
}

fn cell(s: impl Into<kagari_base::SharedString>) -> impl kagari_core::IntoElement {
    let s: kagari_base::SharedString = s.into();
    text(s).color_role(ColorRole::Text).size(Px(15.0))
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — table (500 rows, virtualized)"),
        || {
            let selection = RwSignal::new(Selection::none());
            div().p_4().bg(ColorRole::Surface).child(
                table(clips(500))
                    .column("Name", 180.0, |c: &Clip| cell(c.name.clone()))
                    .column("Duration", 90.0, |c: &Clip| cell(c.duration.clone()))
                    .column("Kind", 80.0, |c: &Clip| cell(c.kind))
                    .selection(selection)
                    .height(360.0),
            )
        },
    )?;
    app.run()
}
