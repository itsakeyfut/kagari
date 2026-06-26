#![forbid(unsafe_code)]
//! kagari-core — element tree, reactive, damage, events, scheduler, and app shell.
//!
//! Phase 1 provides only the minimal app shell (a single window with wgpu init).

pub mod app;
pub mod error;

pub use app::App;
pub use error::AppError;
