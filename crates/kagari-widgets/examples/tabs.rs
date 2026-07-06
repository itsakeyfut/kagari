//! Tabs example (#85) — a manual visual check of the [`Tabs`](kagari_widgets::Tabs) widget, and the
//! **end-to-end verification of #278** (live `dyn_if`/`dyn_list` reconcile): clicking a tab swaps the
//! panel content *live*. Opens a real window via the App shell, so it is a manual/local check (like the
//! `segmented`/`field` examples), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example tabs`
//!
//! The tab strip is one focus scope: click a tab (or, while focused, use the arrow keys) to change the
//! selection. The active tab shows an Accent underline, and the matching panel renders below — rebuilt
//! live each time you switch (that live swap is exactly what #278 enables).

use kagari_core::reactive::RwSignal;
use kagari_core::{App, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::tabs;

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(WindowOptions::default().title("kagari — tabs"), || {
        let selected = RwSignal::new(0_usize);
        div().flex_col().gap_4().p_4().bg(ColorRole::Surface).child(
            tabs(selected)
                .tab("Home", || {
                    div()
                        .p_4()
                        .bg(ColorRole::SurfaceRaised)
                        .rounded_md()
                        .child(text("Welcome home").color_role(ColorRole::Text))
                })
                .tab("Profile", || {
                    div()
                        .p_4()
                        .bg(ColorRole::SurfaceRaised)
                        .rounded_md()
                        .child(text("Your profile").color_role(ColorRole::Text))
                })
                .tab("Settings", || {
                    div()
                        .p_4()
                        .bg(ColorRole::SurfaceRaised)
                        .rounded_md()
                        .child(text("Settings & preferences").color_role(ColorRole::Text))
                }),
        )
    })?;
    app.run()
}
