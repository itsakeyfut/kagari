//! kagari-render — instanced primitive renderer (wgpu).
//!
//! Phase 1 provides the offscreen linear target + output-transform pass; the Quad
//! pipeline, atlas, and assets follow in later issues.

mod atlas;
mod color;
pub mod error;
mod quad;
pub mod renderer;
pub mod scene;
mod sprite;

pub use atlas::{Atlas, AtlasCoord};
pub use error::RenderError;
pub use renderer::Renderer;
pub use scene::{
    Background, Batch, Border, MonochromeSprite, PrimitiveKind, Quad, RoundedRect, Scene,
};
