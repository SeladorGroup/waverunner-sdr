//! Radio Demodulators
//!
//! Each demodulator implements the [`Demodulator`] trait, producing mono audio
//! (`Vec<f32>`) from complex IQ samples. Demodulators are composable with the
//! DDC, AGC, and resampling stages in the receive pipeline.
//!
//! ## Implemented Modes
//!
//! | Module | Modes | Technique |
//! |--------|-------|-----------|
//! | [`am`] | AM, AM-Sync | Envelope detection, synchronous (PLL) |
//! | [`fm`] | NFM, WFM, WFM Stereo | Quadrature discriminator, pilot PLL |
//! | [`ssb`] | USB, LSB | Weaver method, phasing method |
//! | [`cw`] | CW | BFO injection + narrow bandpass |

pub mod am;
pub mod cw;
pub mod fm;
pub mod ssb;

use crate::types::Sample;

/// Optional visualization state from demodulators.
///
/// Provides access to internal PLL/carrier tracking state for display
/// in TUI or GPU visualizations. All methods have default no-op
/// implementations, so existing demodulators work without changes.
pub trait VisualizationProvider {
    /// Average phase error magnitude in radians.
    fn phase_error(&self) -> f32 {
        0.0
    }
    /// Estimated carrier/pilot frequency in Hz.
    fn frequency_estimate_hz(&self) -> f64 {
        0.0
    }
    /// Whether the PLL/carrier is locked.
    fn is_locked(&self) -> bool {
        false
    }
}

/// Common interface for all demodulators.
///
/// Input: complex IQ samples at the demodulator's expected input rate
/// (typically the DDC output rate). Output: real-valued audio samples.
pub trait Demodulator: Send + VisualizationProvider {
    /// Human-readable name for display (e.g., "WFM Stereo").
    fn name(&self) -> &str;

    /// Process IQ input, returning demodulated audio.
    ///
    /// For mono modes, returns a single-channel `Vec<f32>`.
    /// For stereo (WFM), returns interleaved L/R samples.
    fn process(&mut self, input: &[Sample]) -> Vec<f32>;

    /// Expected input sample rate in Hz.
    fn sample_rate_in(&self) -> f64;

    /// Output audio sample rate in Hz.
    fn sample_rate_out(&self) -> f64;

    /// Set a demodulator-specific parameter.
    ///
    /// Common keys: "bandwidth", "squelch", "bfo_offset", "volume"
    fn set_parameter(&mut self, key: &str, value: f64) -> Result<(), String>;

    /// Reset internal state (e.g., after frequency change).
    fn reset(&mut self);
}

/// Auto-selected parameters for a demodulation mode.
///
/// These defaults are shared between CLI, TUI, and SessionManager to ensure
/// consistent hardware and DSP configuration across all frontends.
#[derive(Debug, Clone)]
pub struct DemodModeDefaults {
    /// Channel bandwidth in Hz.
    pub channel_bw: f64,
    /// Recommended hardware sample rate in S/s.
    pub sample_rate: f64,
    /// DDC output rate (baseband sample rate) in Hz.
    pub ddc_output_rate: f64,
    /// De-emphasis time constant in μs (0 = disabled).
    pub deemph_us: f64,
}

/// Get default parameters for a demodulation mode.
///
/// Returns `None` for unrecognized mode strings.
pub fn mode_defaults(mode: &str) -> Option<DemodModeDefaults> {
    match mode {
        "am" | "am-sync" => Some(DemodModeDefaults {
            channel_bw: 10_000.0,
            sample_rate: 1_024_000.0,
            ddc_output_rate: 16_000.0,
            deemph_us: 0.0,
        }),
        "fm" => Some(DemodModeDefaults {
            channel_bw: 12_500.0,
            sample_rate: 1_024_000.0,
            ddc_output_rate: 16_000.0,
            deemph_us: 0.0,
        }),
        "wfm" => Some(DemodModeDefaults {
            channel_bw: 200_000.0,
            sample_rate: 1_024_000.0,
            ddc_output_rate: 128_000.0,
            deemph_us: 75.0,
        }),
        "wfm-stereo" => Some(DemodModeDefaults {
            channel_bw: 200_000.0,
            sample_rate: 1_024_000.0,
            ddc_output_rate: 128_000.0,
            deemph_us: 75.0,
        }),
        "usb" | "lsb" => Some(DemodModeDefaults {
            channel_bw: 3_000.0,
            sample_rate: 1_024_000.0,
            ddc_output_rate: 8_000.0,
            deemph_us: 0.0,
        }),
        "cw" => Some(DemodModeDefaults {
            channel_bw: 500.0,
            sample_rate: 1_024_000.0,
            ddc_output_rate: 4_000.0,
            deemph_us: 0.0,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::mode_defaults;

    #[test]
    fn wfm_defaults_use_reduced_live_sample_rate() {
        let mono = mode_defaults("wfm").unwrap();
        let stereo = mode_defaults("wfm-stereo").unwrap();

        assert_eq!(mono.sample_rate, 1_024_000.0);
        assert_eq!(stereo.sample_rate, 1_024_000.0);
        assert_eq!(mono.ddc_output_rate, 128_000.0);
        assert_eq!(stereo.ddc_output_rate, 128_000.0);
    }
}
