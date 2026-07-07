//! Popover example (#81) — a manual visual check of the [`Popover`](kagari_widgets::Popover) widget.
//!
//! Run with: `cargo run -p kagari-widgets --example popover`
//!
//! Click the accent button to toggle the popover open; while open, a bordered panel floats anchored below
//! it. Click outside the panel (or press Esc) to dismiss it — focus returns to the button. The open state
//! is a controlled `RwSignal<bool>` the trigger toggles.

use kagari_core::reactive::RwSignal;
use kagari_core::reactive::prelude::*;
use kagari_core::{AnchorHandle, App, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::popover;

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(WindowOptions::default().title("kagari — popover"), || {
        let anchor = AnchorHandle::new();
        let open = RwSignal::new(false);
        div()
            .flex_col()
            .gap_4()
            .p_8()
            .bg(ColorRole::Surface)
            .child(
                // The trigger: tagged as the anchor + toggles `open`.
                div()
                    .anchor_ref(&anchor)
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(ColorRole::Accent)
                    .on_click(move |_, _| open.set(!open.get_untracked()))
                    .child(text("Toggle popover").color_role(ColorRole::AccentFg)),
            )
            .child(popover(&anchor, open, || {
                // A bordered + shadowed panel — visible on both themes (RK-030: shape via border/shadow).
                div()
                    .p_4()
                    .rounded_md()
                    .bg(ColorRole::SurfaceRaised)
                    .border_w_1()
                    .border_color(Some(ColorRole::Border))
                    .shadow_md()
                    .child(text("Popover content").color_role(ColorRole::Text))
            }))
    })?;
    app.run()
}
