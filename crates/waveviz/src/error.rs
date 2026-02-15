//! Error types for the waveviz visualization engine.

/// Errors that can occur during GPU visualization setup and rendering.
#[derive(Debug, thiserror::Error)]
pub enum VizError {
    /// Failed to create or configure a GPU surface.
    #[error("Surface error: {0}")]
    Surface(String),

    /// Failed to request a GPU device/adapter.
    #[error("Device request failed: {0}")]
    RequestDevice(String),

    /// Shader compilation or loading error.
    #[error("Shader error: {0}")]
    Shader(String),

    /// Invalid parameter (e.g., zero-size buffer, out-of-range value).
    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),
}
