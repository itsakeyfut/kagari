//! Button example (#70) — a manual visual check of the [`Button`](kagari_widgets::Button) widget (and
//! icons, #246). Opens a real window via the App shell, so it is a **manual/local** check (like
//! `kagari-core`'s `hello`/`a11y_demo`), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example button`
//!
//! Interactive state visuals are wired (#245): focus a button (Tab, once focus navigation is on) to
//! see the focus-ring border, and click the toggle button to see its pressed (Accent) color. Clicking
//! any button also fires its `on_click` (printed to stdout).

use kagari_core::reactive::RwSignal;
use kagari_core::{App, IconId, WindowOptions, div, icon};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{ControlSize, button};

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — button"),
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
                // Wrapped in a row so each button sizes to its content (a lone flex-column child would
                // otherwise stretch to the full window width via the flex `align-items: stretch` default).
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(button("Click me").on_click(|| println!("clicked!")))
                        // A toggle button: its background follows the bound signal (pressed = Accent,
                        // unpressed = Surface) — #245's reactive role styling.
                        .child(button("Bold").toggle(RwSignal::new(false))),
                )
                // Icons (#246): a leading icon on a button (tinted to the label) + standalone icons.
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(button("Confirm").icon(IconId::Check))
                        .child(icon(IconId::Close))
                        .child(icon(IconId::ChevronDown).color_role(ColorRole::Accent)),
                )
        },
    )?;
    app.run()
}
