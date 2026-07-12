//! Splitter example (#84) — a manual visual check of the [`splitter`](kagari_widgets::splitter) widget.
//! Opens a real window via the App shell, so it is a **manual/local** check (not a CI test).
//!
//! Run with: `cargo run -p kagari-widgets --example splitter`
//!
//! A horizontal splitter over two coloured panes; the right pane is itself a vertical splitter. Drag a
//! divider to resize (the grab tracks the cursor), or focus one (Tab) and use the arrow keys. Each pane
//! has a 60px minimum, so a divider stops before a pane vanishes.

use kagari_base::{Px, Size};
use kagari_core::reactive::RwSignal;
use kagari_core::{AnyElement, App, IntoElement, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::splitter;

fn pane(label: &str, bg: ColorRole) -> AnyElement {
    div()
        .flex_grow(1.0)
        .min_w(0.0)
        .min_h(0.0)
        .bg(bg)
        .p_4()
        .child(
            text(label.to_string())
                .color_role(ColorRole::Text)
                .size(Px(15.0)),
        )
        .into_element()
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(
        WindowOptions::default().title("kagari — splitter (drag the dividers)"),
        || {
            // Controlled ratios, created under the window owner.
            let main = RwSignal::new(0.5);
            let side = RwSignal::new(0.4);
            // A fixed 800×500 slot the outer splitter fills.
            div().flex().size(Size { w: 800.0, h: 500.0 }).child(
                splitter(main)
                    .min(60.0)
                    .pane(pane("left", ColorRole::Surface))
                    .pane(
                        splitter(side)
                            .vertical()
                            .min(60.0)
                            .pane(pane("top-right", ColorRole::SurfaceRaised))
                            .pane(pane("bottom-right", ColorRole::SurfaceOverlay)),
                    ),
            )
        },
    )?;
    app.run()
}
