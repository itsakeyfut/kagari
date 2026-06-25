//! Opens a single window that clears to a solid color.
//!
//! Run with: `cargo run -p kagari-core --example clear_window`

fn main() -> Result<(), kagari_core::AppError> {
    kagari_core::App::new()?.run()
}
