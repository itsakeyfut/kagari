//! Opens a single window that clears to a solid color.
//!
//! Run with: `cargo run -p kagari-core --example clear_window`
//!
//! For the manual IME test (docs/dev/ime-manual-test.md, #27), run with
//! `RUST_LOG=kagari_core=trace` to see IME events and toggle/conversion key
//! pass-through; scoping to `kagari_core` keeps wgpu/winit trace from flooding output.

use kagari_core::{App, WindowOptions, div};

fn main() -> Result<(), kagari_core::AppError> {
    // Honor RUST_LOG; default to kagari-core info so window-lifecycle logs still show.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("kagari_core=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let mut app = App::new()?;
    // An empty root (`div` is an `FnOnce() -> impl IntoElement`) — the renderer clears the window.
    app.open_window(WindowOptions::default().title("clear_window"), div)?;
    app.run()
}
