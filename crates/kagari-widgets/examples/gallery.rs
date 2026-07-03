//! Widget gallery (#70) — a manual visual check of the kagari widget set. Grows as widgets land
//! (Button is the first). Opens a real window via the App shell, so it is a **manual/local** check
//! (like `kagari-core`'s `hello`/`a11y_demo`), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example gallery`
//!
//! Note: interactive state *visuals* (focus-ring outline, toggle-pressed color) are not rendered yet
//! (relocated to #245), so the disabled/variant backgrounds + labels are what's visually confirmable
//! here today; clicking a button does fire its `on_click` (printed to stdout).

use kagari_core::{App, WindowOptions, div};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{ControlSize, button};

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — widget gallery"),
        || {
            div()
                .flex_col()
                .gap_4()
                .p_4()
                .bg(ColorRole::Surface)
                // Variants (+ disabled).
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(button("Primary").primary())
                        .child(button("Ghost").ghost())
                        .child(button("Danger").danger())
                        .child(button("Disabled").disabled(true)),
                )
                // Sizes.
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(button("Small").size(ControlSize::Sm))
                        .child(button("Medium").size(ControlSize::Md))
                        .child(button("Large").size(ControlSize::Lg)),
                )
                // Interactive: the click fires the callback (mouse or Space/Enter while focused).
                // Wrapped in a row so the button sizes to its content (a lone flex-column child would
                // otherwise stretch to the full window width via the flex `align-items: stretch` default).
                .child(
                    div()
                        .flex()
                        .child(button("Click me").on_click(|| println!("clicked!"))),
                )
        },
    )?;
    app.run()
}
