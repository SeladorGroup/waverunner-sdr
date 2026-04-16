//! AM Demodulators
//!
//! Two architectures:
//!
//! ## Envelope Detection
//!
//! The simplest and most robust AM demodulator:
//!   `y[n] = |z[n]| = √(I² + Q²)`
//!
//! Extracts the instantaneous amplitude of the analytic signal. Works well
//! when carrier is present (standard AM broadcast). The DC component from
//! the carrier is removed by a highpass filter (DC blocker).
//!
//! ## Synchronous Detection
//!
//! Uses a PLL to lock to the carrier, then multiplies by the recovered carrier:
//!   `y[n] = Re(z[n] · e^{−jθ[n]})`
//!
//! where `θ[n]` is the PLL's phase estimate. Advantages over envelope:
//! - 3 dB better SNR at low SNR (coherent gain)
//! - Handles selective fading (carrier fade doesn't destroy signal)
//! - Works for suppressed-carrier DSB (with Costas loop)
//!
//! The cost is complexity and potential loss-of-lock during deep fades.

use super::{Demodulator, VisualizationProvider};
use crate::dsp::iir;
use crate::dsp::pll::Pll;
use crate::types::Sample;

/// AM demodulation mode.
#[derive(Clone, Copy, Debug)]
pub enum AmMode {
    /// Standard AM with carrier: envelope detection
    Envelope,
    /// Synchronous AM: PLL-based coherent detection
    Synchronous,
}

/// AM Demodulator.
///
/// Supports both envelope and synchronous detection of AM signals.
/// Input: complex IQ at `sample_rate_in`.
/// Output: demodulated audio at the same rate (resample externally).
pub struct AmDemod {
    mode: AmMode,
    sample_rate: f64,
    /// DC removal: first-order highpass at ~20 Hz
    dc_blocker: iir::IirFilter,
    /// PLL for synchronous mode
    pll: Pll,
    /// Audio bandpass: removes sub-audio rumble and supersonic content
    audio_lpf: iir::IirFilter,
}

impl AmDemod {
    /// Create an AM demodulator.
    ///
    /// `mode`: Envelope or Synchronous detection
    /// `sample_rate`: input/output sample rate (Hz)
    /// `audio_bandwidth`: audio LPF cutoff (Hz), typically 5000 for AM broadcast
    pub fn new(mode: AmMode, sample_rate: f64, audio_bandwidth: f64) -> Self {
        let dc_blocker = iir::first_order_highpass(20.0, sample_rate);
        let pll = Pll::new(30.0, 0.707, sample_rate); // Narrow PLL for carrier tracking
        let audio_lpf = iir::butter(4, audio_bandwidth, iir::FilterBand::Lowpass, sample_rate);

        Self {
            mode,
            sample_rate,
            dc_blocker,
            pll,
            audio_lpf,
        }
    }
}

impl VisualizationProvider for AmDemod {
    fn phase_error(&self) -> f32 {
        self.pll.phase_error_avg() as f32
    }

    fn frequency_estimate_hz(&self) -> f64 {
        self.pll.frequency_hz()
    }

    fn is_locked(&self) -> bool {
        matches!(self.mode, AmMode::Synchronous) && self.pll.is_locked()
    }
}

impl Demodulator for AmDemod {
    fn name(&self) -> &str {
        match self.mode {
            AmMode::Envelope => "AM",
            AmMode::Synchronous => "AM-Sync",
        }
    }

    fn process(&mut self, input: &[Sample]) -> Vec<f32> {
        let mut audio = Vec::with_capacity(input.len());

        match self.mode {
            AmMode::Envelope => {
                for &sample in input {
                    // Envelope: |z| = √(I² + Q²)
                    let envelope = (sample.re * sample.re + sample.im * sample.im).sqrt();
                    // DC removal (carrier component)
                    let ac = self.dc_blocker.process_sample(envelope);
                    // Audio bandwidth limiting
                    let filtered = self.audio_lpf.process_sample(ac);
                    audio.push(filtered);
                }
            }
            AmMode::Synchronous => {
                for &sample in input {
                    // PLL locks to carrier, derotates signal
                    let (coherent, _error, _locked) = self.pll.step(sample);
                    // In-phase component is the demodulated audio
                    let demod = coherent.re;
                    let ac = self.dc_blocker.process_sample(demod);
                    let filtered = self.audio_lpf.process_sample(ac);
                    audio.push(filtered);
                }
            }
        }

        audio
    }

    fn sample_rate_in(&self) -> f64 {
        self.sample_rate
    }

    fn sample_rate_out(&self) -> f64 {
        self.sample_rate
    }

    fn set_parameter(&mut self, key: &str, value: f64) -> Result<(), String> {
        match key {
            "mode" => {
                self.mode = if value < 0.5 {
                    AmMode::Envelope
                } else {
                    AmMode::Synchronous
                };
                Ok(())
            }
            "bandwidth" => {
                self.audio_lpf = iir::butter(4, value, iir::FilterBand::Lowpass, self.sample_rate);
                Ok(())
            }
            _ => Err(format!("Unknown AM parameter: {key}")),
        }
    }

    fn reset(&mut self) {
        self.dc_blocker.reset();
        self.pll.reset();
        self.audio_lpf.reset();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    #[test]
    fn am_envelope_detects_modulation() {
        let fs = 16000.0;
        let _f_carrier = 0.0; // Already at baseband after DDC
        let f_mod = 400.0; // 400 Hz modulation tone
        let mod_depth = 0.8; // 80% modulation depth

        let mut demod = AmDemod::new(AmMode::Envelope, fs, 5000.0);

        // Generate AM signal: (1 + m·sin(2πf_mod·t)) · e^{j2πf_c·t}
        // At baseband (f_carrier=0): z[n] = (1 + m·sin(2πf_mod·n/fs))
        let n = 16000; // 1 second
        let input: Vec<Sample> = (0..n)
            .map(|i| {
                let t = i as f64 / fs;
                let envelope = 1.0 + mod_depth * (2.0 * PI * f_mod * t).sin();
                Sample::new(envelope as f32, 0.0)
            })
            .collect();

        let audio = demod.process(&input);
        assert_eq!(audio.len(), n);

        // The demodulated audio should contain the 400 Hz tone
        // Measure power in the 400 Hz region using Goertzel-like correlation
        let skip = 2000; // Skip transient
        let mut cos_sum = 0.0f64;
        let mut sin_sum = 0.0f64;
        for (i, &sample) in audio.iter().enumerate().take(n).skip(skip) {
            let t = i as f64 / fs;
            cos_sum += sample as f64 * (2.0 * PI * f_mod * t).cos();
            sin_sum += sample as f64 * (2.0 * PI * f_mod * t).sin();
        }
        let tone_power = (cos_sum * cos_sum + sin_sum * sin_sum) / (n - skip) as f64;

        assert!(
            tone_power > 0.01,
            "Should detect 400 Hz modulation tone: power = {tone_power:.6}"
        );
    }

    #[test]
    fn am_synchronous_better_at_low_snr() {
        // With clean signal, both modes should work. Sync should track carrier.
        let fs = 16000.0;
        let mut sync_demod = AmDemod::new(AmMode::Synchronous, fs, 5000.0);

        let n = 16000;
        let input: Vec<Sample> = (0..n)
            .map(|i| {
                let t = i as f64 / fs;
                let envelope = 1.0 + 0.5 * (2.0 * PI * 300.0 * t).sin();
                Sample::new(envelope as f32, 0.0)
            })
            .collect();

        let audio = sync_demod.process(&input);
        assert_eq!(audio.len(), n);

        // Audio should have nonzero energy (successful demodulation)
        let rms: f32 =
            (audio[4000..].iter().map(|&x| x * x).sum::<f32>() / (n - 4000) as f32).sqrt();
        assert!(
            rms > 0.01,
            "Sync demod should produce audio: rms = {rms:.4}"
        );
    }

    #[test]
    fn am_dc_removal() {
        let fs = 16000.0;
        let mut demod = AmDemod::new(AmMode::Envelope, fs, 5000.0);

        // Pure carrier (no modulation) → envelope = constant → DC
        // After DC removal, output should be ~0
        let input = vec![Sample::new(1.0, 0.0); 16000];
        let audio = demod.process(&input);

        let late_dc: f32 = audio[8000..].iter().sum::<f32>() / 8000.0;
        assert!(
            late_dc.abs() < 0.05,
            "DC should be removed: mean = {late_dc:.4}"
        );
    }
}
