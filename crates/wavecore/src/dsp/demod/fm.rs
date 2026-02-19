//! FM Demodulators — Narrowband FM, Wideband FM, WFM Stereo
//!
//! ## Quadrature Discriminator (Complex Conjugate Product)
//!
//! The instantaneous frequency of a complex signal z[n] is:
//!
//!   f_inst[n] = fs/(2π) · arg(z[n] · z*[n−1])
//!
//! Expanding the complex product:
//!   z[n]·z*[n−1] = (I[n]·I[n−1] + Q[n]·Q[n−1]) + j(Q[n]·I[n−1] − I[n]·Q[n−1])
//!
//! The argument (atan2 of imaginary over real) gives the phase change per sample,
//! which is proportional to the instantaneous frequency deviation. No explicit
//! division, numerically stable, and fast.
//!
//! ## De-emphasis
//!
//! FM broadcast uses pre-emphasis (6 dB/octave boost above τ) to improve SNR.
//! The receiver applies de-emphasis (6 dB/octave cut):
//! - North America / South Korea: τ = 75 μs
//! - Europe / Japan / Australia: τ = 50 μs
//!
//! H(z) = (1−α) / (1−α·z⁻¹) where α = exp(−1/(τ·fs))
//!
//! ## WFM Stereo
//!
//! The FM stereo multiplex signal contains:
//! - 30 Hz – 15 kHz: L+R (mono-compatible sum)
//! - 19 kHz: pilot tone (−20 dB, phase reference)
//! - 23–53 kHz: L−R on DSB-SC subcarrier at 38 kHz (2× pilot)
//! - 57 kHz: RDS/RBDS (3× pilot), not decoded here
//!
//! Stereo decoding:
//! 1. PLL locks to 19 kHz pilot, generates coherent 38 kHz reference
//! 2. Multiply L−R band by 38 kHz → demodulate to baseband
//! 3. Matrix: L = (L+R + L−R)/2,  R = (L+R − L−R)/2

use super::{Demodulator, VisualizationProvider};
use crate::dsp::iir;
use crate::dsp::pll::Pll;
use crate::types::Sample;
use std::f64::consts::PI;

/// FM demodulator mode.
#[derive(Clone, Copy, Debug)]
pub enum FmMode {
    /// Narrowband FM (12.5 or 25 kHz channel)
    Narrow,
    /// Wideband FM mono (200 kHz channel, ±75 kHz deviation)
    Wide,
    /// Wideband FM stereo (pilot detection + L/R matrix decode)
    WideStereo,
}

/// FM Demodulator.
pub struct FmDemod {
    mode: FmMode,
    sample_rate: f64,
    /// Previous sample for quadrature discriminator
    prev_sample: Sample,
    /// De-emphasis filter (first-order IIR)
    deemph: iir::IirFilter,
    /// De-emphasis filter for right channel (stereo mode)
    deemph_r: iir::IirFilter,
    /// Audio lowpass for NFM
    audio_lpf: iir::IirFilter,
    /// FM deviation gain: maps atan2 output to audio amplitude
    /// For WFM: gain = fs / (2π · Δf_max) where Δf_max = 75 kHz
    deviation_gain: f64,
    /// Squelch threshold (linear power)
    squelch_threshold: f64,
    /// Squelch state
    squelch_open: bool,
    /// Power estimate for squelch
    power_avg: f64,
    power_alpha: f64,
    // --- Stereo fields ---
    /// Pilot PLL (locks to 19 kHz)
    pilot_pll: Pll,
    /// Pilot bandpass filter (narrow band around 19 kHz)
    pilot_bpf: iir::IirFilter,
    /// L+R lowpass (15 kHz)
    stereo_lpf_l: iir::IirFilter,
    /// L−R after demod lowpass (15 kHz)
    stereo_lpf_r: iir::IirFilter,
    /// Pilot detected flag
    pilot_locked: bool,
}

impl FmDemod {
    /// Create an FM demodulator.
    ///
    /// `mode`: Narrow, Wide, or WideStereo
    /// `sample_rate`: input sample rate (Hz). For WFM, typically 256 kHz.
    ///   For NFM, typically 16 kHz.
    /// `deemph_tau_us`: de-emphasis time constant in μs (75 for US, 50 for EU, 0 to disable)
    pub fn new(mode: FmMode, sample_rate: f64, deemph_tau_us: f64) -> Self {
        let deemph = if deemph_tau_us > 0.0 {
            iir::deemphasis(deemph_tau_us, sample_rate)
        } else {
            iir::first_order_lowpass(sample_rate / 2.0 - 1.0, sample_rate) // passthrough-ish
        };
        let deemph_r = if deemph_tau_us > 0.0 {
            iir::deemphasis(deemph_tau_us, sample_rate)
        } else {
            iir::first_order_lowpass(sample_rate / 2.0 - 1.0, sample_rate)
        };

        let audio_lpf = match mode {
            FmMode::Narrow => iir::butter(4, 4000.0, iir::FilterBand::Lowpass, sample_rate),
            FmMode::Wide | FmMode::WideStereo => {
                iir::butter(4, 15000.0, iir::FilterBand::Lowpass, sample_rate)
            }
        };

        let deviation_gain = match mode {
            FmMode::Narrow => 1.0, // NFM: unity, atan2 output is already audio-range
            FmMode::Wide | FmMode::WideStereo => {
                // WFM: scale atan2 output. Max deviation ±75 kHz at fs.
                // atan2 output is in [−π, π] for ±fs/2 deviation.
                // So gain = 1.0 / (2π · 75000 / fs) to normalize
                sample_rate / (2.0 * PI * 75000.0)
            }
        };

        // Pilot PLL: 19 kHz, narrow BL for clean lock
        let pilot_pll = Pll::new(5.0, 0.707, sample_rate);

        // Pilot bandpass: narrow around 19 kHz (18.5–19.5 kHz)
        let pilot_bpf = if sample_rate > 40000.0 {
            iir::butter(2, 19000.0, iir::FilterBand::Lowpass, sample_rate)
        } else {
            iir::first_order_lowpass(sample_rate / 2.0 - 1.0, sample_rate)
        };

        // Stereo L+R and L−R lowpass (15 kHz)
        let stereo_lpf_l = iir::butter(4, 15000.0, iir::FilterBand::Lowpass, sample_rate);
        let stereo_lpf_r = iir::butter(4, 15000.0, iir::FilterBand::Lowpass, sample_rate);

        let power_alpha = 1.0 - (-1.0 / (0.01 * sample_rate)).exp();

        Self {
            mode,
            sample_rate,
            prev_sample: Sample::new(0.0, 0.0),
            deemph,
            deemph_r,
            audio_lpf,
            deviation_gain,
            squelch_threshold: 0.0, // Disabled
            squelch_open: true,
            power_avg: 0.0,
            power_alpha,
            pilot_pll,
            pilot_bpf,
            stereo_lpf_l,
            stereo_lpf_r,
            pilot_locked: false,
        }
    }

    /// Quadrature FM discriminator.
    ///
    /// Computes the instantaneous frequency from phase change:
    ///   Δφ = arg(z[n] · z*[n−1]) = atan2(Q[n]I[n−1] − I[n]Q[n−1],
    ///                                       I[n]I[n−1] + Q[n]Q[n−1])
    #[inline]
    fn discriminator(&self, current: Sample, previous: Sample) -> f32 {
        // z[n] · conj(z[n-1]):
        let dot = current.re * previous.re + current.im * previous.im;   // Re
        let cross = current.im * previous.re - current.re * previous.im; // Im
        cross.atan2(dot)
    }
}

impl VisualizationProvider for FmDemod {
    fn phase_error(&self) -> f32 {
        self.pilot_pll.phase_error_avg() as f32
    }

    fn frequency_estimate_hz(&self) -> f64 {
        self.pilot_pll.frequency_hz()
    }

    fn is_locked(&self) -> bool {
        matches!(self.mode, FmMode::WideStereo) && self.pilot_locked
    }
}

impl Demodulator for FmDemod {
    fn name(&self) -> &str {
        match self.mode {
            FmMode::Narrow => "NFM",
            FmMode::Wide => "WFM",
            FmMode::WideStereo => "WFM Stereo",
        }
    }

    fn process(&mut self, input: &[Sample]) -> Vec<f32> {
        match self.mode {
            FmMode::Narrow => self.process_nfm(input),
            FmMode::Wide => self.process_wfm_mono(input),
            FmMode::WideStereo => self.process_wfm_stereo(input),
        }
    }

    fn sample_rate_in(&self) -> f64 {
        self.sample_rate
    }

    fn sample_rate_out(&self) -> f64 {
        self.sample_rate
    }

    fn set_parameter(&mut self, key: &str, value: f64) -> Result<(), String> {
        match key {
            "squelch" => {
                // Squelch in dBFS → linear power
                self.squelch_threshold = 10.0f64.powf(value / 10.0);
                Ok(())
            }
            "deemphasis" => {
                if value > 0.0 {
                    self.deemph = iir::deemphasis(value, self.sample_rate);
                    self.deemph_r = iir::deemphasis(value, self.sample_rate);
                }
                Ok(())
            }
            _ => Err(format!("Unknown FM parameter: {key}")),
        }
    }

    fn reset(&mut self) {
        self.prev_sample = Sample::new(0.0, 0.0);
        self.deemph.reset();
        self.deemph_r.reset();
        self.audio_lpf.reset();
        self.pilot_pll.reset();
        self.pilot_bpf.reset();
        self.stereo_lpf_l.reset();
        self.stereo_lpf_r.reset();
        self.power_avg = 0.0;
        self.squelch_open = true;
        self.pilot_locked = false;
    }
}

impl FmDemod {
    fn process_nfm(&mut self, input: &[Sample]) -> Vec<f32> {
        let mut audio = Vec::with_capacity(input.len());

        for &sample in input {
            // Squelch: measure input power
            let power = (sample.re * sample.re + sample.im * sample.im) as f64;
            self.power_avg += self.power_alpha * (power - self.power_avg);

            if self.squelch_threshold > 0.0 {
                self.squelch_open = self.power_avg > self.squelch_threshold;
            }

            if self.squelch_open {
                let demod = self.discriminator(sample, self.prev_sample);
                let deemph = self.deemph.process_sample(demod);
                let filtered = self.audio_lpf.process_sample(deemph);
                audio.push(filtered);
            } else {
                audio.push(0.0);
            }

            self.prev_sample = sample;
        }

        audio
    }

    fn process_wfm_mono(&mut self, input: &[Sample]) -> Vec<f32> {
        let mut audio = Vec::with_capacity(input.len());

        for &sample in input {
            let demod = self.discriminator(sample, self.prev_sample) as f64
                * self.deviation_gain;
            let deemph = self.deemph.process_sample(demod as f32);
            let filtered = self.audio_lpf.process_sample(deemph);
            audio.push(filtered);
            self.prev_sample = sample;
        }

        audio
    }

    /// WFM stereo demodulation.
    ///
    /// After FM discrimination, the composite baseband signal contains:
    /// - L+R: 30 Hz – 15 kHz
    /// - Pilot: 19 kHz sinusoid
    /// - L−R: DSB-SC on 38 kHz = 23–53 kHz
    ///
    /// Steps:
    /// 1. FM discriminate to get composite signal
    /// 2. Extract pilot with bandpass + PLL at 19 kHz
    /// 3. Generate 38 kHz from PLL (double the phase)
    /// 4. Multiply composite by 38 kHz to demodulate L−R
    /// 5. LPF both L+R and L−R to 15 kHz
    /// 6. Matrix decode: L = (sum + diff)/2, R = (sum − diff)/2
    ///
    /// Output: interleaved L, R, L, R, ...
    fn process_wfm_stereo(&mut self, input: &[Sample]) -> Vec<f32> {
        let mut audio = Vec::with_capacity(input.len() * 2); // Stereo = 2× samples

        for &sample in input {
            // Step 1: FM discriminate
            let composite = self.discriminator(sample, self.prev_sample) as f64
                * self.deviation_gain;
            self.prev_sample = sample;

            // Step 2: Extract pilot tone (19 kHz)
            let pilot_filtered = self.pilot_bpf.process_sample(composite as f32);

            // Feed pilot to PLL as complex (real → I, Q=0)
            let pilot_sample = Sample::new(pilot_filtered, 0.0);
            let (_derotated, _error, locked) = self.pilot_pll.step(pilot_sample);
            self.pilot_locked = locked;

            if self.pilot_locked {
                // Step 3: Generate 38 kHz from PLL phase (double it)
                let pilot_phase = self.pilot_pll.phase_rad();
                let subcarrier_phase = 2.0 * pilot_phase; // 38 kHz = 2 × 19 kHz
                let subcarrier = subcarrier_phase.cos() as f32;

                // Step 4: L+R is the low-frequency part of composite
                let sum = self.stereo_lpf_l.process_sample(composite as f32);

                // L−R is composite × 38 kHz subcarrier, then lowpass
                let diff_raw = composite as f32 * subcarrier * 2.0; // ×2 for DSB-SC gain
                let diff = self.stereo_lpf_r.process_sample(diff_raw);

                // Step 5: De-emphasis
                let sum_deemph = self.deemph.process_sample(sum);
                let diff_deemph = self.deemph_r.process_sample(diff);

                // Step 6: Matrix decode
                let left = (sum_deemph + diff_deemph) * 0.5;
                let right = (sum_deemph - diff_deemph) * 0.5;

                audio.push(left);
                audio.push(right);
            } else {
                // No pilot: fall back to mono
                let mono = self.stereo_lpf_l.process_sample(composite as f32);
                let deemph = self.deemph.process_sample(mono);
                audio.push(deemph);
                audio.push(deemph); // Duplicate to both channels
            }
        }

        audio
    }

    /// Whether the stereo pilot tone is currently locked.
    pub fn pilot_locked(&self) -> bool {
        self.pilot_locked
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fm_discriminator_detects_frequency() {
        let fs = 48000.0;
        let mut demod = FmDemod::new(FmMode::Narrow, fs, 0.0);

        // Generate FM signal: constant frequency offset of 1 kHz
        // z[n] = e^{j·2π·1000·n/fs}
        let n = 4800;
        let f_dev = 1000.0;
        let input: Vec<Sample> = (0..n)
            .map(|i| {
                let phase = 2.0 * PI * f_dev * i as f64 / fs;
                Sample::new(phase.cos() as f32, phase.sin() as f32)
            })
            .collect();

        let audio = demod.process(&input);

        // Discriminator output should be proportional to frequency deviation
        // atan2 output for constant freq = 2π·f/fs
        let expected = (2.0 * PI * f_dev / fs) as f32;

        // Check steady-state (skip first sample)
        let mean: f32 = audio[10..].iter().sum::<f32>() / (audio.len() - 10) as f32;
        assert!(
            (mean - expected).abs() < 0.05,
            "Discriminator output should be ~{expected:.4}: got {mean:.4}"
        );
    }

    #[test]
    fn fm_demod_tone_modulation() {
        let fs = 48000.0;
        let f_mod = 1000.0;   // 1 kHz modulating tone
        let f_dev = 5000.0;   // ±5 kHz deviation (NFM)

        let mut demod = FmDemod::new(FmMode::Narrow, fs, 0.0);

        // FM signal: phase = 2π·∫f(t)dt where f(t) = f_dev·sin(2πf_mod·t)
        // Instantaneous phase: β·cos(2πf_mod·t) where β = f_dev/f_mod (modulation index)
        let beta = f_dev / f_mod;
        let n = 48000;
        let input: Vec<Sample> = (0..n)
            .map(|i| {
                let t = i as f64 / fs;
                let phase = beta * (2.0 * PI * f_mod * t).sin();
                Sample::new(phase.cos() as f32, phase.sin() as f32)
            })
            .collect();

        let audio = demod.process(&input);

        // Output should contain the 1 kHz tone
        let skip = 2000;
        let mut cos_corr = 0.0f64;
        let mut sin_corr = 0.0f64;
        for (i, &sample) in audio.iter().enumerate().take(n).skip(skip) {
            let t = i as f64 / fs;
            cos_corr += sample as f64 * (2.0 * PI * f_mod * t).cos();
            sin_corr += sample as f64 * (2.0 * PI * f_mod * t).sin();
        }
        let tone_power = (cos_corr * cos_corr + sin_corr * sin_corr) / (n - skip) as f64;

        assert!(
            tone_power > 0.001,
            "Should recover 1 kHz modulation: power = {tone_power:.6}"
        );
    }

    #[test]
    fn fm_squelch_mutes_on_silence() {
        let fs = 16000.0;
        let mut demod = FmDemod::new(FmMode::Narrow, fs, 0.0);
        // Set squelch at −30 dBFS
        demod.set_parameter("squelch", -30.0).unwrap();

        // Very weak signal (below squelch)
        let input = vec![Sample::new(0.0001, 0.0); 16000];
        let audio = demod.process(&input);

        // Output should be mostly zeros
        let rms: f32 = (audio[1000..].iter().map(|&x| x * x).sum::<f32>()
            / (audio.len() - 1000) as f32)
            .sqrt();
        assert!(rms < 0.001, "Squelched output should be silent: rms = {rms:.6}");
    }

    #[test]
    fn wfm_stereo_output_length() {
        let fs = 256000.0;
        let mut demod = FmDemod::new(FmMode::WideStereo, fs, 75.0);

        let input = vec![Sample::new(0.5, 0.3); 2560];
        let audio = demod.process(&input);

        // Stereo: output should be 2× input length (interleaved L/R)
        assert_eq!(
            audio.len(),
            2560 * 2,
            "Stereo output should be 2× input: got {}",
            audio.len()
        );
    }

    #[test]
    fn wfm_deemphasis_attenuates_high_freq() {
        let fs = 256000.0;
        // Compare output with and without de-emphasis
        let mut demod_de = FmDemod::new(FmMode::Wide, fs, 75.0);
        let mut demod_nd = FmDemod::new(FmMode::Wide, fs, 0.0);

        // FM signal with high-frequency modulation (10 kHz)
        let f_mod = 10000.0;
        let beta = 75000.0 / f_mod; // Modulation index for max deviation
        let n = 25600;
        let input: Vec<Sample> = (0..n)
            .map(|i| {
                let t = i as f64 / fs;
                let phase = beta * (2.0 * PI * f_mod * t).sin();
                Sample::new(phase.cos() as f32, phase.sin() as f32)
            })
            .collect();

        let audio_de = demod_de.process(&input);
        let audio_nd = demod_nd.process(&input);

        // De-emphasis should reduce high-frequency content
        let rms_de: f32 = (audio_de[5000..].iter().map(|&x| x * x).sum::<f32>()
            / (audio_de.len() - 5000) as f32)
            .sqrt();
        let rms_nd: f32 = (audio_nd[5000..].iter().map(|&x| x * x).sum::<f32>()
            / (audio_nd.len() - 5000) as f32)
            .sqrt();

        assert!(
            rms_de < rms_nd,
            "De-emphasis should reduce 10 kHz: de={rms_de:.4}, no_de={rms_nd:.4}"
        );
    }
}
