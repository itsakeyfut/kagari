//! Opens a window showing a colored panel with a line of text, built from the element tree.
//!
//! Run with: `cargo run -p kagari-core --example hello`
//!
//! This exercises the #34 paint pass end-to-end: `div().child(text(..))` → build → layout →
//! paint → render. The window's root element is the app's built-in demo tree.

use kagari_core::{App, WindowOptions, div, text};
use kagari_style::ColorRole;

fn main() -> Result<(), kagari_core::AppError> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("kagari_core=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let mut app = App::new()?;
    // A colored panel with a line of text — the #34 paint pass end-to-end.
    app.open_window(WindowOptions::default().title("hello"), || {
        div().bg(ColorRole::Surface).child(text("Hello, kagari"))
    })?;
    app.run()
}
