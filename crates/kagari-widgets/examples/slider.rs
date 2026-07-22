//! Slider example (#75) — a manual visual check of the [`slider`](kagari_widgets::slider) widget. Opens a
//! real window via the App shell, so it is a **manual/local** check (not a CI test).
//!
//! Run with: `cargo run -p kagari-widgets --example slider`
//!
//! A continuous slider, a stepped one, a disabled one, and a vertical fader — each bound to a signal whose
//! value is shown live. Drag the track (click-to-set), or focus a slider (Tab) and use the arrow / Page /
//! Home / End keys.

use kagari_base::{Px, SharedString, Size};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{App, IntoElement, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::slider;

/// A labelled row: the title + live value above a fixed-width horizontal slider.
fn labeled(title: &str, value: RwSignal<f32>, sl: impl IntoElement) -> impl IntoElement {
    let title = title.to_string();
    div()
        .flex_col()
        .gap_1()
        .child(
            text(rx(move || {
                SharedString::from(format!("{title}: {:.2}", value.get()))
            }))
            .color_role(ColorRole::Text)
            .size(Px(14.0)),
        )
        .child(div().flex().size(Size { w: 360.0, h: 24.0 }).child(sl))
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default()
            .title("kagari — slider (drag or focus + arrow keys)")
            .inner_size(Size { w: 560.0, h: 420.0 }),
        || {
            let volume = RwSignal::new(0.6);
            let steps = RwSignal::new(4.0);
            let muted = RwSignal::new(0.3);
            let fader = RwSignal::new(0.7);
            div()
                .flex_col()
                .gap_4()
                .p_4()
                .bg(ColorRole::Bg)
                .child(labeled("Volume (0..1, continuous)", volume, slider(volume)))
                .child(labeled(
                    "Steps (0..10, step 2)",
                    steps,
                    slider(steps).range(0.0, 10.0).step(2.0),
                ))
                .child(labeled("Disabled", muted, slider(muted).disabled(true)))
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .items_center()
                        .child(
                            text(rx(move || {
                                SharedString::from(format!("Fader (vertical): {:.2}", fader.get()))
                            }))
                            .color_role(ColorRole::Text)
                            .size(Px(14.0)),
                        )
                        .child(
                            div()
                                .flex()
                                .size(Size { w: 40.0, h: 180.0 })
                                .child(slider(fader).vertical()),
                        ),
                )
        },
    )?;
    app.run()
}
