//! Field example (#236) — a manual visual check of the [`Field`](kagari_widgets::Field) form-field
//! wrapper. Opens a real window via the App shell, so it is a **manual/local** check (like the
//! `radio`/`segmented` examples), not a CI test.
//!
//! Run with: `cargo run -p kagari-widgets --example field`
//!
//! Each field pairs a label with a control (Row = label-left inspector row, Column = stacked). The last
//! field binds an error signal set to `Some(..)`, so its control slot shows a `Danger` border and its
//! message shows the error in `Danger`; the field above shows muted help text instead.

use kagari_core::reactive::RwSignal;
use kagari_core::{App, WindowOptions, div};
use kagari_style::{ColorRole, Styled};
use kagari_widgets::{FieldLayout, checkbox, field, segmented, switch};

#[derive(Clone, Copy, PartialEq)]
enum Quality {
    Draft,
    Full,
}

fn main() -> Result<(), kagari_core::AppError> {
    let mut app = App::new()?;
    app.open_window(WindowOptions::default().title("kagari — field"), || {
        let enabled = RwSignal::new(true);
        let dark = RwSignal::new(false);
        let quality = RwSignal::new(Quality::Draft);
        let named = RwSignal::new(false);
        // A pre-set error (as if validation failed) to show the Danger border + message.
        let name_error = RwSignal::new(Some("This field is required".into()));

        div()
            .flex_col()
            .gap_4()
            .p_4()
            .bg(ColorRole::Surface)
            // Row fields (label left, control right).
            .child(field("Enabled", checkbox(enabled)))
            .child(field("Dark mode", switch(dark)))
            // A Column field (label above the control).
            .child(
                field(
                    "Quality",
                    segmented(quality)
                        .segment(Quality::Draft, "Draft")
                        .segment(Quality::Full, "Full"),
                )
                .layout(FieldLayout::Column),
            )
            // Help text (muted, below the control).
            .child(field("Autosave", switch(RwSignal::new(true))).help("Saves every 30 seconds"))
            // A field in the error state (Danger border + Danger message).
            .child(field("Name", checkbox(named)).error(name_error))
    })?;
    app.run()
}
