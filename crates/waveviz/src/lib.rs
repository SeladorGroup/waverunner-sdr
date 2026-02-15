//! GPU-accelerated SDR visualization engine.
//!
//! `waveviz` provides wgpu-based renderers for common SDR displays:
//! - **Spectrum**: power spectral density with peak hold and noise floor
//! - **Waterfall**: scrolling time-frequency heatmap
//! - **Constellation**: IQ scatter plot with persistence
//!
//! Designed to be embedded in Tauri/winit applications or used standalone.
//! The [`Renderer`] wraps all sub-renderers and manages shared GPU resources
//! (colormap texture, bind groups).

pub mod colormap;
pub mod constellation;
pub mod error;
pub mod renderer;
pub mod spectrum;
pub mod waterfall;
