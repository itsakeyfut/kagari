//! GridView example (#225) — a manual visual check of the [`grid_view`](kagari_widgets::grid) widget. Opens a
//! real window via the App shell, so it is a **manual/local** check (like the `table` / `tree` examples), not a
//! CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example grid`
//!
//! An asset-browser-style grid of uniform tiles: click a tile to select it (the selected tile tints Accent),
//! and use the arrow keys for 2D navigation (Right/Left within a row, Down/Up across rows). **Enter/Space
//! activate** the selected tile (#368) — the status line above the grid shows the last activated item index.
//! The body virtualizes — only the visible rows are ever built.

use kagari_base::{Px, SharedString, Size};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{App, IntoElement, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{GridSelection, grid_view};

const TILE: Size = Size { w: 120.0, h: 120.0 };

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — grid (asset browser)"),
        || {
            let selection = RwSignal::new(GridSelection::none());
            let activated = RwSignal::new(SharedString::from(
                "Activated item: — (click a tile, then press Enter/Space on a selected tile)",
            ));
            div()
                .flex_col()
                .gap_2()
                .p_4()
                .bg(ColorRole::Surface)
                .child(
                    text(rx(move || activated.get()))
                        .color_role(ColorRole::TextMuted)
                        .size(Px(14.0)),
                )
                .child(
                    grid_view(60, TILE, |i| {
                        // A centred thumbnail tile + a label; the surrounding cell background shows the Accent
                        // selection tint as a frame around this content.
                        div()
                            .size(TILE)
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(Px(6.0))
                            .child(
                                div()
                                    .size(Size { w: 72.0, h: 72.0 })
                                    .rounded_md()
                                    .border_w_1()
                                    .border_color(Some(ColorRole::Border))
                                    .bg(ColorRole::SurfaceRaised),
                            )
                            .child(
                                text(format!("Asset {i}"))
                                    .size(Px(13.0))
                                    .color_role(ColorRole::TextMuted),
                            )
                            .into_element()
                    })
                    .gap(12.0)
                    .size(Size { w: 540.0, h: 420.0 })
                    .selection(selection)
                    .on_activate(move |i| {
                        activated.set(SharedString::from(format!("Activated item: {i}")))
                    }),
                )
        },
    )?;
    app.run()
}
