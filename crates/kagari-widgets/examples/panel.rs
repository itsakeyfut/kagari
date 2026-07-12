//! Panel example (#228) — a manual visual check of the [`panel`](kagari_widgets::panel) widget. Opens a
//! real window via the App shell, so it is a **manual/local** check (not a CI test).
//!
//! Run with: `cargo run -p kagari-widgets --example panel`
//!
//! A collapsible "Inspector" panel with two header actions. Click anywhere on the header (or focus it with
//! Tab and press Space/Enter) to collapse/expand the body. The `+` action bumps the Scale value and `x`
//! resets it — pressing an action does **not** collapse the panel (its click stops propagation).

use kagari_base::{Px, SharedString};
use kagari_core::reactive::prelude::*;
use kagari_core::reactive::{RwSignal, rx};
use kagari_core::{App, IntoElement, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{button, panel};

fn row(label: impl Into<SharedString>) -> impl IntoElement {
    text(label.into())
        .color_role(ColorRole::Text)
        .size(Px(14.0))
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — panel (collapse / actions)"),
        || {
            let collapsed = RwSignal::new(false);
            let scale = RwSignal::new(100i32);
            div().p_4().bg(ColorRole::Bg).child(
                panel("Inspector", move || {
                    div()
                        .flex_col()
                        .gap_2()
                        .child(row("Position:  0, 0"))
                        .child(
                            text(rx(move || {
                                SharedString::from(format!("Scale:     {}%", scale.get()))
                            }))
                            .color_role(ColorRole::Text)
                            .size(Px(14.0)),
                        )
                        .child(row("Rotation:  0"))
                })
                .collapsible(collapsed)
                .actions([
                    button("+")
                        .ghost()
                        .on_click(move || scale.update(|s| *s += 10)),
                    button("x").ghost().on_click(move || scale.set(100)),
                ]),
            )
        },
    )?;
    app.run()
}
