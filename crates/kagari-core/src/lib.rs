#![forbid(unsafe_code)]
//! kagari-core — element tree, reactive, damage, events, scheduler, and app shell.
//!
//! So far: the minimal app shell (a single window with wgpu init), the reactive runtime
//! wrapper (`reactive`), the retained node arena (`arena`), the element tree (`element`), the
//! paint pass (`paint`) that builds the render scene from the tree, and fine-grained damage
//! tracking (`damage`).

pub mod app;
pub mod arena;
pub mod damage;
pub mod element;
pub mod error;
pub mod paint;
pub mod reactive;

pub use app::App;
pub use damage::DamageState;
pub use element::{div, text};
pub use error::AppError;
