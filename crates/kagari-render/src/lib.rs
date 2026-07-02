//! kagari-render — instanced primitive renderer (wgpu).
//!
//! Composites resolved primitives (`Quad`, `MonochromeSprite`) into an offscreen
//! linear target via per-kind instanced pipelines and the multi-page R8 coverage
//! atlas, then runs the output-transform pass to the swapchain. Further primitives
//! and assets follow in later issues.

mod assets;
mod atlas;
mod color;
pub mod error;
mod path;
mod polychrome;
mod quad;
pub mod renderer;
pub mod scene;
mod shadow;
mod sprite;
mod svg;
mod underline;

pub use assets::{AssetId, AssetLoader, AssetSource, LoadState};
pub use atlas::{Atlas, AtlasCoord, AtlasUsage};
pub use error::RenderError;
pub use path::PathBuilder;
pub use renderer::Renderer;
pub use scene::{
    Background, Batch, Border, MonochromeSprite, PathPrim, PolychromeSprite, PrimitiveKind, Quad,
    RoundedRect, Scene, Shadow, Underline, UnderlineStyle,
};
pub use svg::IconId;
