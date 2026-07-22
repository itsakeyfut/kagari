//! VectorInput example (#233) — a manual visual check of the [`vector2`](kagari_widgets::vector2) /
//! [`vector3`](kagari_widgets::vector3) widgets. Opens a real window via the App shell (a manual/local check).
//!
//! Run with: `cargo run -p kagari-widgets --example vector_input`
//!
//! A transform-like inspector: a Position (Vec3), a Scale (Vec3) with a "linked/uniform" switch, and a Vec2 —
//! each bound to an `[f64; N]` signal shown live. Click a field and type / use the steppers or ArrowUp/Down;
//! with the link on, editing one Scale axis sets them all.

use kagari_base::{Px, SharedString, Size};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{App, IntoElement, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{switch, vector2, vector3};

/// A titled row: the label + live value above the input.
fn row(title: &str, live: impl IntoElement, input: impl IntoElement) -> impl IntoElement {
    let title = title.to_string();
    div()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(
                    text(SharedString::from(title))
                        .color_role(ColorRole::Text)
                        .size(Px(14.0)),
                )
                .child(live),
        )
        .child(input)
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default()
            .title("kagari — vector input (transform inspector)")
            .inner_size(Size { w: 560.0, h: 420.0 }),
        || {
            let position = RwSignal::new([0.0_f64, 1.5, -2.0]);
            let scale = RwSignal::new([1.0_f64, 1.0, 1.0]);
            let linked = RwSignal::new(true);
            let uv = RwSignal::new([0.5_f64, 0.25]);

            let pos_live = text(rx(move || {
                let [x, y, z] = position.get();
                SharedString::from(format!("[{x:.2}, {y:.2}, {z:.2}]"))
            }))
            .color_role(ColorRole::TextMuted)
            .size(Px(13.0));
            let scale_live = text(rx(move || {
                let [x, y, z] = scale.get();
                SharedString::from(format!("[{x:.2}, {y:.2}, {z:.2}]"))
            }))
            .color_role(ColorRole::TextMuted)
            .size(Px(13.0));
            let uv_live = text(rx(move || {
                let [u, v] = uv.get();
                SharedString::from(format!("[{u:.2}, {v:.2}]"))
            }))
            .color_role(ColorRole::TextMuted)
            .size(Px(13.0));

            div()
                .flex_col()
                .gap_4()
                .p_4()
                .bg(ColorRole::Bg)
                .child(row(
                    "Position",
                    pos_live,
                    vector3(position).step(0.1).precision(2),
                ))
                .child(row(
                    "Scale (linked)",
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .child(scale_live)
                        .child(switch(linked)),
                    vector3(scale).step(0.1).precision(2).linked(linked),
                ))
                .child(row(
                    "UV",
                    uv_live,
                    vector2(uv).range(0.0, 1.0).step(0.05).precision(3),
                ))
        },
    )?;
    app.run()
}
