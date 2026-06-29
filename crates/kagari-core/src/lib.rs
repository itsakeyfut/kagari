#![forbid(unsafe_code)]
//! kagari-core — element tree, reactive, damage, events, scheduler, and app shell.
//!
//! So far: the minimal app shell (a single window with wgpu init) and the reactive runtime
//! wrapper (`reactive`).

pub mod app;
pub mod error;
pub mod reactive;

pub use app::App;
pub use error::AppError;
