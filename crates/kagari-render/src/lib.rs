//! kagari-render — instanced primitive renderer (wgpu).
//!
//! Phase 1 provides the offscreen linear target + output-transform pass; the Quad
//! pipeline, atlas, and assets follow in later issues.

mod color;
pub mod error;
pub mod renderer;
pub mod scene;

pub use error::RenderError;
pub use renderer::Renderer;
pub use scene::{Background, Batch, Border, PrimitiveKind, Quad, RoundedRect, Scene};
