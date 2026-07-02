//! Background → UI bridge demo (#66): a background `std::thread` (a stand-in for the consumer's async
//! runtime) posts closures via the `UiProxy`; each runs on the UI thread and logs. The manual test for
//! the bridge — **watch the console**: three `background work delivered to the UI thread` lines appear,
//! one per second, produced by work posted from another thread. CI can't run a real window/loop.
//!
//! Run with: `cargo run -p kagari-core --example bridge_demo`

use std::time::Duration;

use kagari_core::{App, WindowOptions, div, text};
use kagari_style::{ColorRole, Styled};

fn main() -> Result<(), kagari_core::AppError> {
    // The posted closures `tracing::info!` under the `bridge_demo` target, so enable it alongside
    // kagari-core (a `kagari_core=info`-only filter would hide the example's own logs).
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("kagari_core=info,bridge_demo=info")
    });
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let mut app = App::new()?;
    // Hand a `UiProxy` to a background thread before the loop runs; it posts work once the app is up
    // (the proxy goes live when `run` populates the bridge cell).
    let proxy = app.ui_proxy();
    std::thread::spawn(move || {
        for tick in 1..=3 {
            std::thread::sleep(Duration::from_secs(1));
            proxy.spawn(move || tracing::info!(tick, "background work delivered to the UI thread"));
        }
    });

    app.open_window(WindowOptions::default().title("bridge_demo"), || {
        div()
            .bg(ColorRole::Surface)
            .p_4()
            .child(text("bridge_demo — watch the console"))
    })?;
    app.run()
}
