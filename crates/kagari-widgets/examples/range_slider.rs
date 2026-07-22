//! RangeSlider example (#234) — a manual visual check of the [`range_slider`](kagari_widgets::range_slider)
//! widget. Opens a real window via the App shell, so it is a **manual/local** check (not a CI test).
//!
//! Run with: `cargo run -p kagari-widgets --example range_slider`
//!
//! A continuous range, a stepped one, a disabled one, and a vertical range — each bound to a `(lo, hi)` signal
//! whose value is shown live. Drag a thumb (or click the track to move the nearer thumb), or Tab to a thumb
//! and use the arrow / Page / Home / End keys. The thumbs cannot cross.

use kagari_base::{Px, SharedString, Size};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{App, IntoElement, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::range_slider;

/// A labelled row: the title + live `(lo, hi)` value above a fixed-width horizontal range slider.
fn labeled(title: &str, value: RwSignal<(f32, f32)>, rs: impl IntoElement) -> impl IntoElement {
    let title = title.to_string();
    div()
        .flex_col()
        .gap_1()
        .child(
            text(rx(move || {
                let (lo, hi) = value.get();
                SharedString::from(format!("{title}: {lo:.2} – {hi:.2}"))
            }))
            .color_role(ColorRole::Text)
            .size(Px(14.0)),
        )
        .child(div().flex().size(Size { w: 360.0, h: 24.0 }).child(rs))
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default()
            .title("kagari — range slider (drag a thumb or Tab + arrow keys)")
            .inner_size(Size { w: 560.0, h: 440.0 }),
        || {
            let volume = RwSignal::new((0.2_f32, 0.8_f32));
            let steps = RwSignal::new((2.0_f32, 8.0_f32));
            let muted = RwSignal::new((0.3_f32, 0.6_f32));
            let fader = RwSignal::new((0.3_f32, 0.7_f32));
            div()
                .flex_col()
                .gap_4()
                .p_4()
                .bg(ColorRole::Bg)
                .child(labeled(
                    "Range (0..1, continuous)",
                    volume,
                    range_slider(volume),
                ))
                .child(labeled(
                    "Steps (0..10, step 2)",
                    steps,
                    range_slider(steps).range(0.0, 10.0).step(2.0),
                ))
                .child(labeled(
                    "Disabled",
                    muted,
                    range_slider(muted).disabled(true),
                ))
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .items_center()
                        .child(
                            text(rx(move || {
                                let (lo, hi) = fader.get();
                                SharedString::from(format!("Vertical range: {lo:.2} – {hi:.2}"))
                            }))
                            .color_role(ColorRole::Text)
                            .size(Px(14.0)),
                        )
                        .child(
                            div()
                                .flex()
                                .size(Size { w: 40.0, h: 200.0 })
                                .child(range_slider(fader).vertical()),
                        ),
                )
        },
    )?;
    app.run()
}
