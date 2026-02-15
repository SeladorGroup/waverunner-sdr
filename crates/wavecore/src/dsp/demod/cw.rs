//! CW (Continuous Wave / Morse) Demodulator
//!
//! CW signals are on-off keyed (OOK) carriers. Reception involves:
//!
//! 1. **Narrow bandpass filter**: 50–500 Hz bandwidth centered on the expected
//!    tone frequency, rejecting adjacent signals and noise.
//!
//! 2. **BFO injection**: Mixes the filtered signal with a local oscillator
//!    at a fixed offset (typically 700 Hz) to produce an audible beat note.
//!    Without the BFO, CW would be inaudible (it's just a carrier).
//!
//! 3. **AGC**: Fast attack for QSB (fading) conditions. CW signals can
//!    vary rapidly in amplitude.
//!
//! The narrow bandwidth gives CW its characteristic high selectivity and
//! good SNR — a 200 Hz CW filter has 27 dB advantage over a 3 kHz SSB filter
//! (10·log₁₀(3000/200) = 11.8 dB thermal + processing gain).

use super::{Demodulator, VisualizationProvider};
use crate::dsp::agc::Agc;
use crate::dsp::iir;
use crate::types::Sample;
use std::f64::consts::PI;

/// CW Demodulator.
pub struct CwDemod {
    sample_rate: f64,
    /// BFO frequency offset (Hz), typically 700
    bfo_freq: f64,
    /// BFO phase accumulator
    bfo_phase: f64,
    /// BFO phase increment (radians/sample)
    bfo_inc: f64,
    /// Narrow CW bandpass filter
    cw_bpf: iir::IirFilter,
    /// Audio lowpass (remove BFO harmonics and mixer products)
    audio_lpf: iir::IirFilter,
    /// Fast AGC for CW fading
    agc: Agc,
}

impl CwDemod {
    /// Create a CW demodulator.
    ///
    /// `sample_rate`: input sample rate (Hz)
    /// `bfo_offset_hz`: BFO frequency for the audible tone (default: 700 Hz)
    /// `bandwidth_hz`: CW filter bandwidth (default: 200 Hz)
    pub fn new(sample_rate: f64, bfo_offset_hz: f64, bandwidth_hz: f64) -> Self {
        let bfo_inc = 2.0 * PI * bfo_offset_hz / sample_rate;

        // Narrow bandpass filter for CW selectivity
        // Filter centered at DC (signal is at baseband after DDC)
        // Use lowpass at bandwidth/2
        let cw_bpf = iir::butter(
            4,
            bandwidth_hz / 2.0,
            iir::FilterBand::Lowpass,
            sample_rate,
        );

        // Audio lowpass: remove mixer products above 1.5 kHz
        let audio_lpf = iir::butter(2, 1500.0, iir::FilterBand::Lowpass, sample_rate);

        // Fast AGC for CW: quick attack (1ms), moderate decay (50ms)
        let agc = Agc::new(-20.0, 0.001, 0.05, sample_rate);

        Self {
            sample_rate,
            bfo_freq: bfo_offset_hz,
            bfo_phase: 0.0,
            bfo_inc,
            cw_bpf,
            audio_lpf,
            agc,
        }
    }
}

impl VisualizationProvider for CwDemod {}

impl Demodulator for CwDemod {
    fn name(&self) -> &str {
        "CW"
    }

    fn process(&mut self, input: &[Sample]) -> Vec<f32> {
        let mut audio = Vec::with_capacity(input.len());

        // Apply AGC to the IQ input
        let mut iq = input.to_vec();
        self.agc.process(&mut iq);

        for &sample in &iq {
            // Narrow bandpass filter
            let filtered = self.cw_bpf.process_sample(sample.re);

            // BFO mixing: multiply by sin(2π·f_bfo·t) to produce audible beat
            let bfo = self.bfo_phase.sin() as f32;
            let beat = filtered * bfo;

            self.bfo_phase += self.bfo_inc;
            if self.bfo_phase > PI {
                self.bfo_phase -= 2.0 * PI;
            }

            // Audio lowpass
            let out = self.audio_lpf.process_sample(beat);
            audio.push(out);
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
            "bfo_offset" => {
                self.bfo_freq = value;
                self.bfo_inc = 2.0 * PI * value / self.sample_rate;
                Ok(())
            }
            "bandwidth" => {
                self.cw_bpf = iir::butter(
                    4,
                    value / 2.0,
                    iir::FilterBand::Lowpass,
                    self.sample_rate,
                );
                Ok(())
            }
            _ => Err(format!("Unknown CW parameter: {key}")),
        }
    }

    fn reset(&mut self) {
        self.bfo_phase = 0.0;
        self.cw_bpf.reset();
        self.audio_lpf.reset();
        self.agc.reset();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cw_produces_audible_tone() {
        let fs = 4000.0;
        let bfo = 700.0;
        let mut demod = CwDemod::new(fs, bfo, 200.0);

        // Input: continuous carrier at DC (CW signal present)
        let n = 4000; // 1 second
        let input = vec![Sample::new(0.5, 0.0); n];

        let audio = demod.process(&input);
        assert_eq!(audio.len(), n);

        // Output should contain energy at approximately bfo_freq (700 Hz)
        let skip = 500;
        let mut cos_sum = 0.0f64;
        let mut sin_sum = 0.0f64;
        for i in skip..n {
            let t = i as f64 / fs;
            cos_sum += audio[i] as f64 * (2.0 * PI * bfo * t).cos();
            sin_sum += audio[i] as f64 * (2.0 * PI * bfo * t).sin();
        }
        let tone_power = (cos_sum * cos_sum + sin_sum * sin_sum) / (n - skip) as f64;

        assert!(
            tone_power > 0.0001,
            "CW should produce 700 Hz tone: power = {tone_power:.6}"
        );
    }

    #[test]
    fn cw_keying_on_off() {
        let fs = 4000.0;
        let mut demod = CwDemod::new(fs, 700.0, 200.0);

        // On-off keying: 500ms on, 500ms off
        let n = 4000;
        let input: Vec<Sample> = (0..n)
            .map(|i| {
                if i < 2000 {
                    Sample::new(0.5, 0.0) // Key down
                } else {
                    Sample::new(0.0, 0.0) // Key up
                }
            })
            .collect();

        let audio = demod.process(&input);

        // First half should have more energy than second half
        let pow_on: f32 = audio[500..2000].iter().map(|&x| x * x).sum::<f32>() / 1500.0;
        let pow_off: f32 = audio[2500..].iter().map(|&x| x * x).sum::<f32>() / 1500.0;

        assert!(
            pow_on > pow_off * 5.0,
            "Key-down should be louder: on={pow_on:.6}, off={pow_off:.6}"
        );
    }

    #[test]
    fn cw_bfo_changes_pitch() {
        let fs = 4000.0;
        let mut demod = CwDemod::new(fs, 700.0, 200.0);
        demod.set_parameter("bfo_offset", 500.0).unwrap();
        assert!((demod.bfo_freq - 500.0).abs() < 0.01);
    }

    #[test]
    fn cw_narrow_filter_rejects_adjacent() {
        let fs = 4000.0;
        let bw = 200.0;
        let mut demod = CwDemod::new(fs, 700.0, bw);

        // Signal at DC (in-band for the CW filter)
        let n = 4000;
        let input_in: Vec<Sample> = (0..n)
            .map(|_| Sample::new(0.5, 0.0))
            .collect();

        let audio_in = demod.process(&input_in);

        // Signal at 500 Hz offset (out of 200 Hz CW filter)
        demod.reset();
        let input_out: Vec<Sample> = (0..n)
            .map(|i| {
                let t = 2.0 * PI * 500.0 * i as f64 / fs;
                Sample::new(t.cos() as f32, t.sin() as f32)
            })
            .collect();

        let audio_out = demod.process(&input_out);

        let pow_in: f32 = audio_in[500..].iter().map(|&x| x * x).sum::<f32>() / 3500.0;
        let pow_out: f32 = audio_out[500..].iter().map(|&x| x * x).sum::<f32>() / 3500.0;

        // In-band should be significantly stronger
        assert!(
            pow_in > pow_out * 2.0,
            "CW filter should reject 500 Hz offset: in={pow_in:.6}, out={pow_out:.6}"
        );
    }
}
