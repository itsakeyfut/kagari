//! Live input demo (#177): a window with one interactive panel that logs mouse clicks, wheel, and
//! gestures, and shows the pointer (hand) cursor over the panel. The manual "ignition test" for the
//! live winit input wiring — CI can't run a real window + GPU, so this is verified by hand.
//!
//! Run with: `cargo run -p kagari-core --example input_demo`
//!
//! Then, over the panel: move the pointer (cursor becomes a hand), press a mouse button, scroll the
//! wheel (→ pan), and ctrl+scroll (→ zoom). Each is logged. `RUST_LOG=kagari_core=info` is the default.

use kagari_core::{App, CursorIcon, GestureEvent, MouseEvent, MouseKind, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};

fn main() -> Result<(), kagari_core::AppError> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("kagari_core=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let mut app = App::new()?;
    app.open_window(WindowOptions::default().title("input_demo"), || {
        div()
            .bg(ColorRole::Surface)
            .cursor(CursorIcon::Pointer)
            .on_mouse_down(|ev: &MouseEvent, _cx| {
                if let MouseKind::Down(button) = ev.kind {
                    tracing::info!(?button, x = ev.pos.x, y = ev.pos.y, "mouse down");
                }
            })
            .on_wheel(|ev: &MouseEvent, _cx| {
                if let MouseKind::Wheel { dx, dy } = ev.kind {
                    tracing::info!(dx, dy, "wheel");
                }
            })
            .on_gesture(|ev: &GestureEvent, _cx| match ev {
                GestureEvent::Pan { dx, dy } => tracing::info!(dx, dy, "pan gesture"),
                GestureEvent::Zoom { factor, .. } => tracing::info!(factor, "zoom gesture"),
                _ => {}
            })
            .child(text("Move / click / scroll / ctrl+scroll over me"))
    })?;
    app.run()
}
