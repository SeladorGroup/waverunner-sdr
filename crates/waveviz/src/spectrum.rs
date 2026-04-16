//! Spectrum renderer — power spectral density display.
//!
//! Renders a frequency-domain power spectrum as a filled polygon with
//! colormap gradient. Supports peak hold (a second trace showing
//! historical maxima with decay) and noise floor marker.
//!
//! GPU rendering uses a storage buffer for spectrum dBFS values.
//! The vertex shader generates geometry from bin values, and the
//! fragment shader applies the colormap via 1D texture lookup.

/// Configuration for the spectrum renderer.
#[derive(Debug, Clone)]
pub struct SpectrumConfig {
    /// Enable peak hold display.
    pub peak_hold: bool,
    /// Peak hold decay rate in dB/frame.
    pub peak_decay_rate: f32,
    /// Fill below the spectrum line.
    pub fill: bool,
}

impl Default for SpectrumConfig {
    fn default() -> Self {
        Self {
            peak_hold: true,
            peak_decay_rate: 0.5,
            fill: true,
        }
    }
}

/// Spectrum data for one frame.
#[derive(Debug, Clone)]
pub struct SpectrumData {
    /// Power spectrum in dBFS.
    pub spectrum_db: Vec<f32>,
    /// Minimum dB value for display range.
    pub min_db: f32,
    /// Maximum dB value for display range.
    pub max_db: f32,
    /// Noise floor in dB (optional horizontal marker).
    pub noise_floor_db: Option<f32>,
    /// Peak hold envelope (optional).
    pub peak_hold: Option<Vec<f32>>,
}

/// CPU-side spectrum renderer state.
///
/// Holds the current spectrum data and peak hold state. GPU pipeline
/// creation and rendering commands are deferred to integration with
/// the wgpu render pass (handled by the `Renderer`).
pub struct SpectrumRenderer {
    config: SpectrumConfig,
    current_data: SpectrumData,
    peak_hold_state: Vec<f32>,
}

impl SpectrumRenderer {
    /// Create a new spectrum renderer.
    pub fn new(config: SpectrumConfig) -> Self {
        Self {
            config,
            current_data: SpectrumData {
                spectrum_db: Vec::new(),
                min_db: -100.0,
                max_db: 0.0,
                noise_floor_db: None,
                peak_hold: None,
            },
            peak_hold_state: Vec::new(),
        }
    }

    /// Update the spectrum data.
    pub fn update(&mut self, data: &SpectrumData) {
        // Update peak hold
        if self.config.peak_hold {
            if self.peak_hold_state.len() != data.spectrum_db.len() {
                self.peak_hold_state = data.spectrum_db.clone();
            } else {
                for (peak, &current) in self.peak_hold_state.iter_mut().zip(&data.spectrum_db) {
                    if current > *peak {
                        *peak = current;
                    } else {
                        *peak = (*peak - self.config.peak_decay_rate).max(current);
                    }
                }
            }
        }

        self.current_data = data.clone();
        if self.config.peak_hold {
            self.current_data.peak_hold = Some(self.peak_hold_state.clone());
        }
    }

    /// Get current spectrum data (with computed peak hold).
    pub fn data(&self) -> &SpectrumData {
        &self.current_data
    }

    /// Get the configuration.
    pub fn config(&self) -> &SpectrumConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spectrum_config_default() {
        let config = SpectrumConfig::default();
        assert!(config.peak_hold);
        assert!(config.fill);
    }

    #[test]
    fn spectrum_update_stores_data() {
        let mut renderer = SpectrumRenderer::new(SpectrumConfig::default());
        let data = SpectrumData {
            spectrum_db: vec![-50.0, -40.0, -30.0],
            min_db: -100.0,
            max_db: 0.0,
            noise_floor_db: Some(-55.0),
            peak_hold: None,
        };
        renderer.update(&data);
        assert_eq!(renderer.data().spectrum_db, vec![-50.0, -40.0, -30.0]);
        assert!(renderer.data().peak_hold.is_some());
    }

    #[test]
    fn peak_hold_tracks_maxima() {
        let mut renderer = SpectrumRenderer::new(SpectrumConfig {
            peak_hold: true,
            peak_decay_rate: 0.5,
            fill: true,
        });

        // Push a strong signal
        let strong = SpectrumData {
            spectrum_db: vec![-20.0],
            min_db: -100.0,
            max_db: 0.0,
            noise_floor_db: None,
            peak_hold: None,
        };
        renderer.update(&strong);
        assert_eq!(renderer.data().peak_hold.as_ref().unwrap()[0], -20.0);

        // Push a weaker signal — peak should decay slowly
        let weak = SpectrumData {
            spectrum_db: vec![-60.0],
            min_db: -100.0,
            max_db: 0.0,
            noise_floor_db: None,
            peak_hold: None,
        };
        renderer.update(&weak);
        let peak = renderer.data().peak_hold.as_ref().unwrap()[0];
        assert!(peak > -60.0, "Peak should decay slowly, not jump to {peak}");
        assert!(peak < -20.0, "Peak should be below the original");
    }
}
