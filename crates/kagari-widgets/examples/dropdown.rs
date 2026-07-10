//! Dropdown / Combobox example (#79) — a manual visual check of the
//! [`dropdown`](kagari_widgets::dropdown) and [`combobox`](kagari_widgets::combobox) widgets. Opens a real
//! window via the App shell, so it is a **manual/local** check (like the `text_input` / `number_input`
//! examples), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example dropdown`
//!
//! Click a trigger to open the list; Arrow/Enter navigate + pick, Esc / an outside click dismisses. The
//! combobox filters as you type. The last row of each is disabled (muted, display-only).

use kagari_core::reactive::RwSignal;
use kagari_core::{App, WindowOptions, div};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{ControlSize, combobox, dropdown};

fn fruits() -> Vec<(u32, kagari_base::SharedString)> {
    vec![
        (1, "Apple".into()),
        (2, "Banana".into()),
        (3, "Cherry".into()),
        (4, "Date".into()),
        (5, "Elderberry".into()),
    ]
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — dropdown / combobox"),
        || {
            let dd = RwSignal::new(2u32);
            let dd_empty = RwSignal::new(0u32); // matches no option → placeholder
            let cb = RwSignal::new(3u32);
            let locked = RwSignal::new(1u32);

            div()
                .flex_col()
                .gap_4()
                .p_4()
                .bg(ColorRole::Surface)
                // Dropdown with a selection.
                .child(dropdown(dd, fruits()))
                // Dropdown with no matching value → placeholder.
                .child(dropdown(dd_empty, fruits()).placeholder("Select a fruit…"))
                // Combobox (filter-as-you-type), small.
                .child(combobox(cb, fruits()).size(ControlSize::Sm))
                // Disabled dropdown (display-only, muted).
                .child(dropdown(locked, fruits()).disabled(true))
        },
    )?;
    app.run()
}
