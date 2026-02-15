//! GPU context and main renderer.
//!
//! The [`GpuContext`] manages the wgpu device/queue and can be either
//! provided externally (when embedding in Tauri/winit) or created
//! internally for headless/test use.
//!
//! The [`Renderer`] owns all sub-renderers and shared resources like
//! the colormap texture and sampler.

use crate::colormap::{Colormap, ColormapLut};
use crate::constellation::{ConstellationConfig, ConstellationRenderer};
use crate::error::VizError;
use crate::spectrum::{SpectrumConfig, SpectrumData, SpectrumRenderer};
use crate::waterfall::{WaterfallConfig, WaterfallRenderer};

/// Shared GPU context wrapping device and queue.
pub struct GpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub format: wgpu::TextureFormat,
}

impl GpuContext {
    /// Wrap an existing device/queue (for embedding in Tauri/winit).
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        Self {
            device,
            queue,
            format,
        }
    }

    /// Request a new headless GPU context (for tests/standalone).
    pub async fn request_headless() -> Result<Self, VizError> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| VizError::RequestDevice("No GPU adapter found".to_string()))?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("waveviz-headless"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .map_err(|e| VizError::RequestDevice(e.to_string()))?;

        Ok(Self {
            device,
            queue,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
        })
    }
}

/// Configuration for the visualization renderer.
pub struct VizConfig {
    pub spectrum: SpectrumConfig,
    pub waterfall: WaterfallConfig,
    pub constellation: ConstellationConfig,
    pub colormap: Colormap,
}

impl Default for VizConfig {
    fn default() -> Self {
        Self {
            spectrum: SpectrumConfig::default(),
            waterfall: WaterfallConfig::default(),
            constellation: ConstellationConfig::default(),
            colormap: Colormap::Turbo,
        }
    }
}

/// Main renderer combining spectrum, waterfall, and constellation.
pub struct Renderer {
    pub spectrum: SpectrumRenderer,
    pub waterfall: WaterfallRenderer,
    pub constellation: ConstellationRenderer,
    colormap_lut: ColormapLut,
}

impl Renderer {
    /// Create a new renderer with the given GPU context and configuration.
    pub fn new(config: VizConfig) -> Self {
        let colormap_lut = ColormapLut::new(config.colormap);

        Self {
            spectrum: SpectrumRenderer::new(config.spectrum),
            waterfall: WaterfallRenderer::new(config.waterfall),
            constellation: ConstellationRenderer::new(config.constellation),
            colormap_lut,
        }
    }

    /// Update spectrum data.
    pub fn update_spectrum(&mut self, data: &SpectrumData) {
        self.spectrum.update(data);
    }

    /// Append a new waterfall row.
    pub fn update_waterfall(&mut self, row: &[f32]) {
        self.waterfall.push_row(row);
    }

    /// Update constellation points.
    pub fn update_constellation(&mut self, points: &[(f32, f32)]) {
        self.constellation.update(points);
    }

    /// Get a reference to the colormap LUT.
    pub fn colormap(&self) -> &ColormapLut {
        &self.colormap_lut
    }

    /// Change the active colormap.
    pub fn set_colormap(&mut self, colormap: Colormap) {
        self.colormap_lut = ColormapLut::new(colormap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viz_config_default() {
        let config = VizConfig::default();
        assert_eq!(config.colormap, Colormap::Turbo);
    }

    #[test]
    fn renderer_construction() {
        let renderer = Renderer::new(VizConfig::default());
        assert_eq!(renderer.colormap().table[0][3], 255);
    }

    #[test]
    fn renderer_update_spectrum() {
        let mut renderer = Renderer::new(VizConfig::default());
        let data = SpectrumData {
            spectrum_db: vec![-50.0; 1024],
            min_db: -100.0,
            max_db: 0.0,
            noise_floor_db: Some(-60.0),
            peak_hold: None,
        };
        renderer.update_spectrum(&data);
        assert_eq!(renderer.spectrum.data().spectrum_db.len(), 1024);
    }

    #[test]
    fn renderer_update_waterfall() {
        let mut renderer = Renderer::new(VizConfig::default());
        renderer.update_waterfall(&vec![-50.0; 512]);
        assert_eq!(renderer.waterfall.row_count(), 1);
    }

    #[test]
    fn renderer_update_constellation() {
        let mut renderer = Renderer::new(VizConfig::default());
        let points = vec![(0.5, 0.3), (-0.2, 0.1)];
        renderer.update_constellation(&points);
        assert_eq!(renderer.constellation.point_count(), 2);
    }

    #[test]
    fn renderer_set_colormap() {
        let mut renderer = Renderer::new(VizConfig::default());
        renderer.set_colormap(Colormap::Viridis);
        // Viridis starts purple (high blue)
        assert!(renderer.colormap().table[0][2] > 50);
    }
}
