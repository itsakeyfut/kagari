//! ReorderableList example (#226) — a manual visual check of the
//! [`reorderable_list`](kagari_widgets::reorderable_list) widget. Opens a real window via the App shell (a
//! manual/local check, not a CI test).
//!
//! Run with: `cargo run -p kagari-widgets --example reorderable_list`
//!
//! A list of "tracks": **drag a row** to reorder it (a line marks where it will drop), or **Tab** to the list
//! and use **↑/↓** to move the highlight and **Alt+↑/↓** to move the highlighted track. The live order is
//! shown above.

use kagari_base::{Px, SharedString, Size};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{App, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{label, reorderable_list};

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default()
            .title("kagari — reorderable list (drag a row, or Tab + Alt+arrows)")
            .inner_size(Size { w: 420.0, h: 460.0 }),
        || {
            let tracks = RwSignal::new(vec![
                "Bass".to_string(),
                "Drums".to_string(),
                "Guitar".to_string(),
                "Vocals".to_string(),
                "Synth".to_string(),
            ]);
            div()
                .flex_col()
                .gap_3()
                .p_4()
                .bg(ColorRole::Bg)
                .child(
                    text(rx(move || {
                        SharedString::from(format!("Order: {}", tracks.get().join(" → ")))
                    }))
                    .color_role(ColorRole::TextMuted)
                    .size(Px(13.0)),
                )
                .child(reorderable_list(tracks, |name: &String| {
                    label(name.clone())
                }))
        },
    )?;
    app.run()
}
