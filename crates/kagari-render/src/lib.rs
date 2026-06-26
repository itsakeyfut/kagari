//! kagari-render — instanced primitive renderer (wgpu).
//!
//! Composites resolved primitives (`Quad`, `MonochromeSprite`) into an offscreen
//! linear target via per-kind instanced pipelines and the multi-page R8 coverage
//! atlas, then runs the output-transform pass to the swapchain. Further primitives
//! and assets follow in later issues.

mod atlas;
mod color;
pub mod error;
mod quad;
pub mod renderer;
pub mod scene;
mod sprite;
mod underline;

pub use atlas::{Atlas, AtlasCoord};
pub use error::RenderError;
pub use renderer::Renderer;
pub use scene::{
    Background, Batch, Border, MonochromeSprite, PrimitiveKind, Quad, RoundedRect, Scene,
    Underline, UnderlineStyle,
};
