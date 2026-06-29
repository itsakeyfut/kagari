//! Opens a window showing a colored panel with a line of text, built from the element tree.
//!
//! Run with: `cargo run -p kagari-core --example hello`
//!
//! This exercises the #34 paint pass end-to-end: `div().child(text(..))` → build → layout →
//! paint → render. The window's root element is the app's built-in demo tree.

fn main() -> Result<(), kagari_core::AppError> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("kagari_core=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    kagari_core::App::new()?.run()
}
