//! Renderer error type.

/// Errors from the renderer. Recoverable GPU conditions (surface/device loss)
/// are surfaced so the app shell can reconfigure or rebuild (specs §1.11 / §2.9).
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("surface lost")]
    SurfaceLost,
    #[error("device lost; must recreate all resources")]
    DeviceLost,
    #[error("atlas full and not growable: {0}")]
    AtlasFull(&'static str),
    #[error("shader compile error: {label} {message}")]
    ShaderCompile {
        label: &'static str,
        message: String,
    },
    #[error("asset decode failed: {path}")]
    AssetDecode { path: String },
}
