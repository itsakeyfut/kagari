//! Dev theme hot-reload (#44): opens the demo window watching a theme RON file. Edit the file while
//! the example runs and the panel reskins live, with no recompile (specs §3.7, moat #4).
//!
//! Run with (the `hot-reload` feature is required):
//! `cargo run -p kagari-core --example hot_reload --features hot-reload -- crates/kagari-core/examples/hot-reload-theme.ron`
//!
//! Then edit the `Surface` color in the RON file and save — the window's background follows it.

use kagari_core::{App, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};

fn main() -> Result<(), kagari_core::AppError> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("kagari_core=info,kagari_style=info")
    });
    tracing_subscriber::fmt().with_env_filter(filter).init();

    // The theme file to watch: the first CLI argument, defaulting to the bundled sample.
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "crates/kagari-core/examples/hot-reload-theme.ron".to_string());

    let mut app = App::new()?.watch_theme(path);
    // The panel background is a `Surface` token, so editing the watched theme reskins it live.
    app.open_window(WindowOptions::default().title("hot_reload"), || {
        div().bg(ColorRole::Surface).child(text("Hello, kagari"))
    })?;
    app.run()
}
