//! NumberInput example (#76) — a manual visual check of the [`number_input`](kagari_widgets::number_input)
//! widget. Opens a real window via the App shell, so it is a **manual/local** check (like the
//! `text_input`/`field` examples), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example number_input`
//!
//! Click a field to focus, type a number (Enter or blur commits — invalid input reverts), use the up/down
//! spinner arrows or ArrowUp/ArrowDown to step. The ranged field clamps into [0, 100]; the last row is
//! disabled (muted, display-only) and one is wrapped in a labeled `Field`.

use kagari_core::reactive::RwSignal;
use kagari_core::{App, WindowOptions, div};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{ControlSize, field, number_input};

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — number input"),
        || {
            let count = RwSignal::new(3.0);
            let opacity = RwSignal::new(50.0);
            let small = RwSignal::new(1.0);
            let large = RwSignal::new(10.0);
            let locked = RwSignal::new(42.0);

            div()
                .flex_col()
                .gap_4()
                .p_4()
                .bg(ColorRole::Surface)
                // Unbounded, unit step.
                .child(number_input(count))
                // Ranged + fractional step, clamps into [0, 100].
                .child(number_input(opacity).range(0.0, 100.0).step(5.0))
                // Sizes.
                .child(number_input(small).size(ControlSize::Sm).step(0.5))
                .child(number_input(large).size(ControlSize::Lg))
                // Disabled (display-only, muted).
                .child(number_input(locked).disabled(true))
                // Inside a labeled Field (inspector row).
                .child(field(
                    "Opacity",
                    number_input(opacity).range(0.0, 100.0).step(5.0),
                ))
        },
    )?;
    app.run()
}
