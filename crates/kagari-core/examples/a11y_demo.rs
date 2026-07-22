//! Accessibility demo (#67): a window of a11y-annotated controls, for the manual screen-reader check
//! that CI can't run (a real window + assistive tech is required). Each panel carries a `role` and an
//! `a11y_label`/`a11y_value`, so a screen reader announces its role + name.
//!
//! Run with: `cargo run -p kagari-core --example a11y_demo`
//!
//! Then, with a screen reader active (Windows: Narrator = Ctrl+Win+Enter; scan mode = Caps Lock+Space,
//! then navigate items with Caps Lock+Left/Right), each control should be read as its role + label,
//! e.g. "Play, button" / "Loop, checkbox" / a text field reading "clip_01.mp4".
//!
//! Note (#67 scope): a11y **focus-follow** (the screen-reader cursor tracking the framework focus) is
//! covered at the code level (the derived tree's focus = the `FocusRegistry`'s focused node) and unit
//! tested, but a consumer cannot yet register a focusable from a view (the context-provided focus
//! registry is a #49 follow-up), so this demo exercises role/label/value **readout**, not live focus.

use kagari_base::SharedString;
use kagari_core::{App, Role, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};

fn main() -> Result<(), kagari_core::AppError> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("kagari_core=info,a11y_demo=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let mut app = App::new()?;
    app.open_window(WindowOptions::default().title("a11y_demo"), || {
        div()
            .flex_col()
            .gap_4()
            .p_4()
            .bg(ColorRole::Surface)
            // A labelled button.
            .child(
                div()
                    .p_2()
                    .bg(ColorRole::Accent)
                    .role(Role::Button)
                    .a11y_label("Play")
                    .child(text("Play")),
            )
            // A checkbox (its state would be exposed via node flags post-MVP; MVP carries role+label).
            .child(
                div()
                    .p_2()
                    .bg(ColorRole::SurfaceRaised)
                    .role(Role::CheckBox)
                    .a11y_label("Loop")
                    .child(text("Loop")),
            )
            // A text input, whose current contents are its a11y value.
            .child(
                div()
                    .p_2()
                    .bg(ColorRole::SurfaceRaised)
                    .role(Role::TextInput)
                    .a11y_label("File name")
                    .a11y_value(SharedString::from("clip_01.mp4"))
                    .child(text("clip_01.mp4")),
            )
            // A static label (its text is carried as the node value, accesskit convention).
            .child(
                div()
                    .role(Role::Label)
                    .a11y_label("Status: ready")
                    .child(text("Status: ready")),
            )
    })?;
    app.run()
}
