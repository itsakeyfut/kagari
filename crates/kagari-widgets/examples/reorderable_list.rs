//! ReorderableList example (#226, live drag #357) — a manual visual check of the
//! [`reorderable_list`](kagari_widgets::reorderable_list) widget. Opens a real window via the App shell (a
//! manual/local check, not a CI test).
//!
//! Run with: `cargo run -p kagari-widgets --example reorderable_list`
//!
//! Two lists of "tracks", each showing a different **user-preference** styling of the same live-reorder (#357):
//! the left uses an **opaque** lift ghost (`.ghost_opacity(1.0)` — a solid carried card) + an `Accent` drop line;
//! the right uses a grip **`.handle(true)`**, a **transparent** ghost (`.ghost_opacity(0.0)` — only the gap + line
//! show the drop), and a `BorderStrong` drop line. **Drag a row / the grip** to reorder: the origin slot collapses and a gap with
//! a colored insertion line opens live where the item will land — it commits on release and reverts on Esc. Or
//! **Tab** to a list and use **↑/↓** to move the highlight and **Alt+↑/↓** to move the highlighted track. Each
//! list's live order is shown above it.

use kagari_base::{Px, SharedString, Size};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{App, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{label, reorderable_list};

/// One labelled column: a live-order readout above a reorderable list of `tracks`, styled with the given drag
/// idiom (`handle`), lift-ghost translucency (`ghost_opacity`), and drop insertion-line color.
fn track_list(
    title: &'static str,
    tracks: RwSignal<Vec<String>>,
    handle: bool,
    ghost_opacity: f32,
    line_color: ColorRole,
) -> impl kagari_core::IntoElement {
    div()
        .flex_col()
        .gap_2()
        .child(text(title).color_role(ColorRole::Text).size(Px(14.0)))
        .child(
            text(rx(move || {
                SharedString::from(format!("Order: {}", tracks.get().join(" → ")))
            }))
            .color_role(ColorRole::TextMuted)
            .size(Px(13.0)),
        )
        .child(
            reorderable_list(tracks, |name: &String| label(name.clone()))
                .handle(handle)
                .ghost_opacity(ghost_opacity)
                .insertion_line_color(line_color),
        )
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default()
            .title("kagari — reorderable list (drag a row / grip, or Tab + Alt+arrows)")
            .inner_size(Size { w: 640.0, h: 460.0 }),
        || {
            let names = || {
                vec![
                    "Bass".to_string(),
                    "Drums".to_string(),
                    "Guitar".to_string(),
                    "Vocals".to_string(),
                    "Synth".to_string(),
                ]
            };
            div()
                .flex()
                .gap_4()
                .p_4()
                .bg(ColorRole::Bg)
                .child(track_list(
                    "Whole-row drag · opaque ghost",
                    RwSignal::new(names()),
                    false,
                    1.0,
                    ColorRole::Accent,
                ))
                .child(track_list(
                    "Drag handle · transparent ghost",
                    RwSignal::new(names()),
                    true,
                    0.0,
                    ColorRole::BorderStrong,
                ))
        },
    )?;
    app.run()
}
