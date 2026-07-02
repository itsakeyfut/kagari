//! Live drag-and-drop demo (#178): drag the swatch onto the drop zone. The manual "ignition test" for
//! the live drag flow — CI can't run a real window + GPU, so this is verified by hand, **watching the
//! console**: press-and-drag "drag me" (a drag-image follows the cursor) onto "drop here" and you get
//! `drag entered the drop zone` → `dropped swatch` (and `drag left the drop zone`); press Esc mid-drag
//! to cancel. Feedback is logged (not visual) because a token-driven reactive `bg` is not yet a thing
//! — `Styled::bg` is static — so the demo relies on the console.
//!
//! OS file drop onto "drop files here" is wired but currently a no-op: winit gives no drop position,
//! so the file cannot be routed to the target (see #206 / the DnD-maturity epic #211).
//!
//! Run with: `cargo run -p kagari-core --example dnd_demo`

use kagari_core::{App, FileDrop, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};

/// The payload dragged from the swatch.
#[derive(Clone, Debug)]
struct Swatch(&'static str);

fn main() -> Result<(), kagari_core::AppError> {
    // Enable BOTH kagari-core and this example's own logs: the handler `tracing::info!`s below are
    // emitted under the `dnd_demo` target, so a `kagari_core=info`-only filter would hide them.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("kagari_core=info,dnd_demo=info"));
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
                .child(text("drop files here (no-op until #206)")),
        ])
    })?;
    app.run()
}
