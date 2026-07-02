//! Live drag-and-drop demo (#178): drag the swatch onto the drop zone (logs the payload), and drop
//! OS files onto the file zone (logged as a `FileDrop`). The manual "ignition test" for the live drag
//! flow — CI can't run a real window + GPU, so this is verified by hand.
//!
//! Run with: `cargo run -p kagari-core --example dnd_demo`
//!
//! Then: press-and-drag the "drag me" swatch (a drag-image follows the cursor) onto "drop here"
//! (logs on over / leave / drop); press Esc mid-drag to cancel; drop files onto "drop files here".

use kagari_core::{App, FileDrop, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};

/// The payload dragged from the swatch.
#[derive(Clone, Debug)]
struct Swatch(&'static str);

fn main() -> Result<(), kagari_core::AppError> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("kagari_core=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let mut app = App::new()?;
    app.open_window(WindowOptions::default().title("dnd_demo"), || {
        div().gap_4().p_4().children([
            // Source: a draggable swatch with a cursor-tracked drag-image.
            div()
                .bg(ColorRole::Accent)
                .p_4()
                .drag_source(|| Swatch("accent"))
                .drag_image(|| div().bg(ColorRole::Accent).p_2().child(text("dragging")))
                .child(text("drag me")),
            // Target: accepts a `Swatch`; logs hover over/leave and the drop.
            div()
                .bg(ColorRole::Surface)
                .p_4()
                .drop_target::<Swatch>(|s: Swatch, _cx| {
                    tracing::info!(payload = s.0, "dropped swatch")
                })
                .on_drag_over(|_cx| tracing::info!("drag entered the drop zone"))
                .on_drag_leave(|_cx| tracing::info!("drag left the drop zone"))
                .child(text("drop here")),
            // File target: accepts OS file drops.
            div()
                .bg(ColorRole::SurfaceRaised)
                .p_4()
                .drop_target::<FileDrop>(|f: FileDrop, _cx| {
                    tracing::info!(count = f.0.len(), "dropped files")
                })
                .child(text("drop files here")),
        ])
    })?;
    app.run()
}
