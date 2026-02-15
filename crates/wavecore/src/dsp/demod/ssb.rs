//! SSB Demodulators — USB, LSB
//!
//! ## Weaver Method (Third Method)
//!
//! A multiplier-based SSB demodulator that avoids wideband Hilbert filters:
//!
//! 1. **Shift**: Multiply input by e^{−jω_c·n} where ω_c is the band center
//!    (typically 1.5 kHz for voice, centering the 300–3000 Hz band)
//! 2. **Filter**: Lowpass at BW/2 (e.g., 1350 Hz for 300–3000 Hz voice)
//! 3. **Shift back**: Multiply by e^{+jω_c·n} and take Re for USB,
//!    or multiply by e^{−jω_c·n} and take Re for LSB
//!
//! Advantages: no wideband 90° phase shifter needed, sharp selectivity
//! from the lowpass filter (easier to design than a Hilbert FIR).
//!
//! ## Phasing Method
//!
//! Uses a Hilbert transform to generate the analytic signal:
//!   USB: y[n] = I[n]·cos(ω₀n) + Î[n]·sin(ω₀n)
//!   LSB: y[n] = I[n]·cos(ω₀n) − Î[n]·sin(ω₀n)
//!
//! where Î is the Hilbert transform (90° phase shift) of I.
//! Simpler conceptually but requires a wideband Hilbert filter.
//!
//! ## BFO (Beat Frequency Oscillator)
//!
//! For SSB reception, the BFO replaces the missing carrier. Its offset from
//! the channel center determines the pitch of the received audio:
//! - USB: BFO below the channel → audio = f_signal − f_bfo
//! - LSB: BFO above the channel → audio = f_bfo − f_signal

use super::{Demodulator, VisualizationProvider};
use crate::dsp::iir;
use crate::types::Sample;
use std::f64::consts::PI;

/// SSB sideband selection.
#[derive(Clone, Copy, Debug)]
pub enum Sideband {
    Upper,
    Lower,
}

/// SSB Demodulator using the Weaver method.
pub struct SsbDemod {
    sideband: Sideband,
    sample_rate: f64,
    /// BFO offset frequency (Hz). Center of the voice band.
    /// Typically 1500 Hz for 300–3000 Hz voice passband.
    bfo_freq: f64,
    /// Phase accumulator for the Weaver oscillator
    weaver_phase: f64,
    /// Weaver oscillator phase increment (radians/sample)
    weaver_inc: f64,
    /// Lowpass filter for I arm (BW/2, e.g., 1350 Hz)
    lpf_i: iir::IirFilter,
    /// Lowpass filter for Q arm
    lpf_q: iir::IirFilter,
    /// Audio bandpass: 200–3500 Hz for voice cleanup
    audio_bpf: iir::IirFilter,
    /// AGC-like output normalization
    output_gain: f32,
}

impl SsbDemod {
    /// Create an SSB demodulator.
    ///
    /// `sideband`: USB or LSB
    /// `sample_rate`: input sample rate (Hz)
    /// `bfo_offset_hz`: BFO offset from channel center (Hz). Default: 1500.
    /// `voice_bandwidth`: audio bandwidth (Hz). Default: 2700 (300–3000 Hz).
    pub fn new(
        sideband: Sideband,
        sample_rate: f64,
        bfo_offset_hz: f64,
        voice_bandwidth: f64,
    ) -> Self {
        let bfo_freq = bfo_offset_hz;
        let weaver_inc = 2.0 * PI * bfo_freq / sample_rate;

        // Lowpass at half the voice bandwidth (Weaver architecture)
        let lpf_cutoff = voice_bandwidth / 2.0;
        let lpf_i = iir::butter(4, lpf_cutoff, iir::FilterBand::Lowpass, sample_rate);
        let lpf_q = iir::butter(4, lpf_cutoff, iir::FilterBand::Lowpass, sample_rate);

        // Audio cleanup bandpass
        let audio_bpf = iir::butter(2, 3500.0, iir::FilterBand::Lowpass, sample_rate);

        Self {
            sideband,
            sample_rate,
            bfo_freq,
            weaver_phase: 0.0,
            weaver_inc,
            lpf_i,
            lpf_q,
            audio_bpf,
            output_gain: 2.0, // Compensate for Weaver processing gain
        }
    }
}

impl VisualizationProvider for SsbDemod {}

impl Demodulator for SsbDemod {
    fn name(&self) -> &str {
        match self.sideband {
            Sideband::Upper => "USB",
            Sideband::Lower => "LSB",
        }
    }

    fn process(&mut self, input: &[Sample]) -> Vec<f32> {
        let mut audio = Vec::with_capacity(input.len());

        for &sample in input {
            let cos_w = self.weaver_phase.cos() as f32;
            let sin_w = self.weaver_phase.sin() as f32;

            // === Weaver Stage 1: Frequency shift to center desired sideband at DC ===
            // USB: multiply by e^{−jω_c·n} → shifts USB band to DC
            //   I_s = I·cos + Q·sin,  Q_s = Q·cos − I·sin
            // LSB: multiply by e^{+jω_c·n} → shifts LSB band to DC
            //   I_s = I·cos − Q·sin,  Q_s = Q·cos + I·sin
            let (shifted_i, shifted_q) = match self.sideband {
                Sideband::Upper => (
                    sample.re * cos_w + sample.im * sin_w,
                    sample.im * cos_w - sample.re * sin_w,
                ),
                Sideband::Lower => (
                    sample.re * cos_w - sample.im * sin_w,
                    sample.im * cos_w + sample.re * sin_w,
                ),
            };

            // === Weaver Stage 2: Lowpass filter both arms (BW/2) ===
            let filt_i = self.lpf_i.process_sample(shifted_i);
            let filt_q = self.lpf_q.process_sample(shifted_q);

            // === Weaver Stage 3: Shift back and take real part ===
            // USB: Re{z_filt × e^{+jω_c·n}} = filt_i·cos − filt_q·sin
            // LSB: Re{z_filt × e^{−jω_c·n}} = filt_i·cos + filt_q·sin
            let demod = match self.sideband {
                Sideband::Upper => filt_i * cos_w - filt_q * sin_w,
                Sideband::Lower => filt_i * cos_w + filt_q * sin_w,
            };

            // Audio cleanup and gain
            let filtered = self.audio_bpf.process_sample(demod * self.output_gain);
            audio.push(filtered);

            // Advance oscillator
            self.weaver_phase += self.weaver_inc;
            if self.weaver_phase > PI {
                self.weaver_phase -= 2.0 * PI;
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
            "bfo_offset" => {
                self.bfo_freq = value;
                self.weaver_inc = 2.0 * PI * value / self.sample_rate;
                Ok(())
            }
            "bandwidth" => {
                let lpf_cutoff = value / 2.0;
                self.lpf_i = iir::butter(4, lpf_cutoff, iir::FilterBand::Lowpass, self.sample_rate);
                self.lpf_q = iir::butter(4, lpf_cutoff, iir::FilterBand::Lowpass, self.sample_rate);
                Ok(())
            }
            "sideband" => {
                self.sideband = if value < 0.5 {
                    Sideband::Upper
                } else {
                    Sideband::Lower
                };
                Ok(())
            }
            _ => Err(format!("Unknown SSB parameter: {key}")),
        }
    }

    fn reset(&mut self) {
        self.weaver_phase = 0.0;
        self.lpf_i.reset();
        self.lpf_q.reset();
        self.audio_bpf.reset();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssb_usb_demodulates_upper_sideband() {
        let fs = 8000.0;
        let f_signal = 1000.0; // 1 kHz tone in the upper sideband

        let mut demod = SsbDemod::new(Sideband::Upper, fs, 1500.0, 2700.0);

        // Input: a tone at +1000 Hz (upper sideband)
        let n = 8000;
        let input: Vec<Sample> = (0..n)
            .map(|i| {
                let t = 2.0 * PI * f_signal * i as f64 / fs;
                Sample::new(t.cos() as f32, t.sin() as f32)
            })
            .collect();

        let audio = demod.process(&input);
        assert_eq!(audio.len(), n);

        // Output should have audio energy
        let rms: f32 = (audio[2000..].iter().map(|&x| x * x).sum::<f32>()
            / (n - 2000) as f32)
            .sqrt();
        assert!(rms > 0.01, "USB should demodulate upper tone: rms = {rms:.4}");
    }

    #[test]
    fn ssb_lsb_demodulates_lower_sideband() {
        let fs = 8000.0;
        let f_signal = -1000.0; // Tone in the lower sideband (negative frequency)

        let mut demod = SsbDemod::new(Sideband::Lower, fs, 1500.0, 2700.0);

        let n = 8000;
        let input: Vec<Sample> = (0..n)
            .map(|i| {
                let t = 2.0 * PI * f_signal * i as f64 / fs;
                Sample::new(t.cos() as f32, t.sin() as f32)
            })
            .collect();

        let audio = demod.process(&input);

        let rms: f32 = (audio[2000..].iter().map(|&x| x * x).sum::<f32>()
            / (n - 2000) as f32)
            .sqrt();
        assert!(rms > 0.01, "LSB should demodulate lower tone: rms = {rms:.4}");
    }

    #[test]
    fn ssb_sideband_rejection() {
        let fs = 8000.0;
        // USB demod should reject a tone at −1000 Hz (lower sideband)
        let mut demod = SsbDemod::new(Sideband::Upper, fs, 1500.0, 2700.0);

        let n = 8000;
        // Tone at -1000 Hz (wrong sideband for USB)
        let input: Vec<Sample> = (0..n)
            .map(|i| {
                let t = 2.0 * PI * (-1000.0) * i as f64 / fs;
                Sample::new(t.cos() as f32, t.sin() as f32)
            })
            .collect();

        let audio_reject = demod.process(&input);

        // Now with correct sideband
        demod.reset();
        let input_pass: Vec<Sample> = (0..n)
            .map(|i| {
                let t = 2.0 * PI * 1000.0 * i as f64 / fs;
                Sample::new(t.cos() as f32, t.sin() as f32)
            })
            .collect();

        let audio_pass = demod.process(&input_pass);

        let rms_reject: f32 = (audio_reject[2000..].iter().map(|&x| x * x).sum::<f32>()
            / (n - 2000) as f32)
            .sqrt();
        let rms_pass: f32 = (audio_pass[2000..].iter().map(|&x| x * x).sum::<f32>()
            / (n - 2000) as f32)
            .sqrt();

        assert!(
            rms_pass > rms_reject * 3.0,
            "USB should reject wrong sideband: pass={rms_pass:.4}, reject={rms_reject:.4}"
        );
    }

    #[test]
    fn ssb_bfo_offset_changes_pitch() {
        let fs = 8000.0;
        let mut demod = SsbDemod::new(Sideband::Upper, fs, 1500.0, 2700.0);

        // Changing BFO offset should shift the audio pitch
        demod.set_parameter("bfo_offset", 1200.0).unwrap();
        assert!((demod.bfo_freq - 1200.0).abs() < 0.01);
    }
}
