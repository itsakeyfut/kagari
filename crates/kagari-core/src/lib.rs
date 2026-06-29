#![forbid(unsafe_code)]
//! kagari-core — element tree, reactive, damage, events, scheduler, and app shell.
//!
//! So far: the minimal app shell (a single window with wgpu init), the reactive runtime
//! wrapper (`reactive`), the retained node arena (`arena`), and the element tree (`element`).

pub mod app;
pub mod arena;
pub mod element;
pub mod error;
pub mod reactive;

pub use app::App;
pub use error::AppError;
