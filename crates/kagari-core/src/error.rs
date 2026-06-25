//! Core error type.

/// Errors from the app shell / window lifecycle.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("failed to create wgpu device: {0}")]
    DeviceInit(String),
    #[error("failed to create window: {0}")]
    WindowCreate(String),
}
