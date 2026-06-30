#![forbid(unsafe_code)]
//! kagari-core — element tree, reactive, damage, events, scheduler, and app shell.
//!
//! So far: the minimal app shell (a single window with wgpu init), the reactive runtime
//! wrapper (`reactive`), the retained node arena (`arena`), the element tree (`element`), the
//! paint pass (`paint`) that builds the render scene from the tree, fine-grained damage
//! tracking (`damage`), the hybrid frame scheduler (`scheduler`), and the events/input layer
//! (`event`) — so far the hit-test structure (#47).

pub mod app;
pub mod arena;
pub mod damage;
pub mod element;
pub mod error;
pub mod event;
pub mod paint;
pub mod reactive;
pub mod scheduler;

pub use app::App;
pub use damage::DamageState;
pub use element::{div, text};
pub use error::AppError;
pub use event::{HitRegion, HitTest, InteractFlags};
pub use scheduler::Scheduler;
