//! Digital Downconverter (DDC)
//!
//! Frequency-shifts a wideband signal to baseband and decimates to the desired
//! channel bandwidth. The architecture is:
//!
//! ```text
//! input → NCO mixer → CIC decimation → FIR compensation/decimation → output
//! ```
//!
//! ## Numerically Controlled Oscillator (NCO)
//!
//! Phase-accumulator design with LUT-based sin/cos synthesis:
//! - 32-bit phase accumulator (2³² = one revolution, wrapping arithmetic)
//! - 1024-point quarter-wave LUT with linear interpolation
//! - Phase increment: Δφ = round(f/fs × 2³²)
//! - Spurious-free dynamic range from LUT quantization: ~66 dB for 10-bit table
//!   (SFDR ≈ 6.02 × LUT_bits + residual from interpolation)
//!
//! ## CIC + FIR Decimation Chain
//!
//! Uses the CIC decimator from `resample.rs` for bulk rate reduction (typically
//! 8×–256×), followed by an FIR compensation filter that:
//! 1. Corrects the sinc^R passband droop of the CIC
//! 2. Provides the sharp channel selectivity (transition band rejection)
//! 3. Optionally performs a second decimation stage

use crate::dsp::filter_design::FirFilter;
use crate::dsp::resample::{CicDecimator, cic_compensation_fir};
use crate::types::Sample;
use std::f64::consts::PI;

// ============================================================================
// NCO (Numerically Controlled Oscillator)
// ============================================================================

/// LUT size for quarter-wave sin/cos table. 1024 entries covers [0, π/2].
/// Full wave synthesis via quadrant symmetry.
const LUT_SIZE: usize = 1024;

/// Quarter-wave sine lookup table, computed at compile time via const fn
/// would be ideal but we use lazy_static pattern via once-initialized Vec.
/// Instead, we store it in the struct and initialize once.
struct SinLut {
    table: [f32; LUT_SIZE + 1], // +1 for interpolation at boundary
}

impl SinLut {
    fn new() -> Self {
        let mut table = [0.0f32; LUT_SIZE + 1];
        for (i, entry) in table.iter_mut().enumerate() {
            // Quarter-wave: maps [0, LUT_SIZE] → [0, π/2]
            *entry = (PI / 2.0 * i as f64 / LUT_SIZE as f64).sin() as f32;
        }
        Self { table }
    }

    /// Evaluate sin(θ) where θ is a 32-bit phase (0 = 0, 2³² = 2π).
    ///
    /// Uses quadrant decomposition + linear interpolation:
    /// - Top 2 bits select quadrant
    /// - Next 10 bits index into the LUT
    /// - Remaining 20 bits provide fractional index for interpolation
    #[inline]
    fn sin(&self, phase: u32) -> f32 {
        let quadrant = (phase >> 30) & 0x3;
        // Index within the quadrant (top 10 bits after quadrant)
        let index_bits = (phase >> 20) & 0x3FF;
        // Fractional part for linear interpolation (20 bits → [0, 1))
        let frac = (phase & 0xFFFFF) as f32 / (1u32 << 20) as f32;

        let (idx, frac_adj, negate) = match quadrant {
            0 => (index_bits as usize, frac, false), // [0, π/2): sin ascending
            1 => (index_bits as usize, frac, false), // [π/2, π): sin descending = sin(π - θ)
            2 => (index_bits as usize, frac, true),  // [π, 3π/2): −sin ascending
            3 => (index_bits as usize, frac, true),  // [3π/2, 2π): −sin descending
            _ => unreachable!(),
        };

        // For quadrants 1 and 3, we read the table in reverse
        let (table_idx, interp_frac) = if quadrant == 1 || quadrant == 3 {
            (LUT_SIZE - idx, -frac_adj) // Read backwards
        } else {
            (idx, frac_adj)
        };

        // Linear interpolation: y ≈ y₀ + μ·(y₁ − y₀)
        let y0 = self.table[table_idx];
        let y1 = if table_idx < LUT_SIZE {
            self.table[table_idx + 1]
        } else {
            self.table[table_idx]
        };
        let val = y0 + interp_frac * (y1 - y0);

        if negate { -val } else { val }
    }

    /// Evaluate cos(θ) = sin(θ + π/2).
    #[inline]
    fn cos(&self, phase: u32) -> f32 {
        // cos(θ) = sin(θ + π/2). Adding π/2 in 32-bit phase = adding 2³⁰.
        self.sin(phase.wrapping_add(1u32 << 30))
    }
}

/// Numerically Controlled Oscillator.
///
/// Generates complex exponentials e^{jωn} = cos(ωn) + j·sin(ωn) via a
/// phase accumulator and quarter-wave LUT with linear interpolation.
///
/// The phase accumulator is 32 bits wide, giving a frequency resolution of
/// Δf = fs/2³² Hz. Phase wrapping is handled naturally by unsigned overflow.
pub struct Nco {
    /// Phase accumulator (32-bit, wrapping)
    phase: u32,
    /// Phase increment per sample: Δφ = round(f_hz / f_s × 2³²)
    phase_inc: u32,
    /// Sample rate (for frequency-to-increment conversion)
    sample_rate: f64,
    /// Sine lookup table
    lut: SinLut,
}

impl Nco {
    /// Create an NCO at the given sample rate, initially at 0 Hz.
    pub fn new(sample_rate: f64) -> Self {
        Self {
            phase: 0,
            phase_inc: 0,
            sample_rate,
            lut: SinLut::new(),
        }
    }

    /// Set the NCO output frequency in Hz.
    ///
    /// Negative frequencies produce complex conjugate (I leads Q vs Q leads I).
    /// The phase increment is: Δφ = round(f/fs × 2³²).
    pub fn set_frequency(&mut self, freq_hz: f64) {
        let normalized = freq_hz / self.sample_rate;
        // Convert to 32-bit phase increment. Wrapping handles negative freqs.
        self.phase_inc = (normalized * (1u64 << 32) as f64) as i64 as u32;
    }

    /// Get the current frequency setting in Hz.
    pub fn frequency(&self) -> f64 {
        // Interpret phase_inc as signed for correct sign reporting
        let signed = self.phase_inc as i32 as f64;
        signed * self.sample_rate / (1u64 << 32) as f64
    }

    /// Generate the next complex sample: e^{jφ} = cos(φ) + j·sin(φ).
    #[inline]
    pub fn next_sample(&mut self) -> Sample {
        let cos_val = self.lut.cos(self.phase);
        let sin_val = self.lut.sin(self.phase);
        self.phase = self.phase.wrapping_add(self.phase_inc);
        Sample::new(cos_val, sin_val)
    }

    /// Mix (frequency-shift) a block of samples in-place.
    ///
    /// Multiplies each sample by e^{−j2πft}, shifting the spectrum down
    /// by the NCO frequency. This is the core of the DDC: it moves the
    /// desired channel to baseband.
    pub fn mix(&mut self, samples: &mut [Sample]) {
        for sample in samples.iter_mut() {
            let cos_val = self.lut.cos(self.phase);
            let sin_val = self.lut.sin(self.phase);
            self.phase = self.phase.wrapping_add(self.phase_inc);

            // Complex multiply: (I + jQ) × (cos − jsin) = (I·cos + Q·sin) + j(Q·cos − I·sin)
            // The negative sign on sin gives e^{−jωt} (downconversion)
            let i = sample.re;
            let q = sample.im;
            sample.re = i * cos_val + q * sin_val;
            sample.im = q * cos_val - i * sin_val;
        }
    }

    /// Mix with upconversion: multiply by e^{+j2πft}.
    pub fn mix_up(&mut self, samples: &mut [Sample]) {
        for sample in samples.iter_mut() {
            let cos_val = self.lut.cos(self.phase);
            let sin_val = self.lut.sin(self.phase);
            self.phase = self.phase.wrapping_add(self.phase_inc);

            let i = sample.re;
            let q = sample.im;
            sample.re = i * cos_val - q * sin_val;
            sample.im = q * cos_val + i * sin_val;
        }
    }

    /// Set the NCO phase directly (0.0 = 0, 1.0 = 2π).
    pub fn set_phase(&mut self, phase_normalized: f64) {
        self.phase = (phase_normalized * (1u64 << 32) as f64) as u32;
    }

    /// Get current phase in radians [0, 2π).
    pub fn phase_rad(&self) -> f64 {
        self.phase as f64 / (1u64 << 32) as f64 * 2.0 * PI
    }

    /// Adjust phase by adding a delta (in radians).
    pub fn adjust_phase(&mut self, delta_rad: f64) {
        let delta_u32 = (delta_rad / (2.0 * PI) * (1u64 << 32) as f64) as i64 as u32;
        self.phase = self.phase.wrapping_add(delta_u32);
    }

    /// Adjust frequency by adding a delta (in Hz).
    pub fn adjust_frequency(&mut self, delta_hz: f64) {
        let delta_inc = (delta_hz / self.sample_rate * (1u64 << 32) as f64) as i64 as u32;
        self.phase_inc = self.phase_inc.wrapping_add(delta_inc);
    }

    pub fn reset(&mut self) {
        self.phase = 0;
    }
}

// ============================================================================
// DDC (Digital Downconverter)
// ============================================================================

/// Digital Downconverter: NCO + multi-stage decimation.
///
/// Translates a channel at `center_freq` to baseband and decimates from the
/// input sample rate to the desired output rate. The decimation is split into:
///
/// 1. **CIC stage**: Handles the bulk decimation (factor M₁). Efficient (no
///    multiplies) but introduces sinc^R passband droop.
/// 2. **FIR stage**: Compensates CIC droop, provides sharp channel selectivity,
///    and performs the final decimation (factor M₂).
///
/// Total decimation = M₁ × M₂ ≈ input_rate / output_rate.
pub struct Ddc {
    nco: Nco,
    cic: Option<CicDecimator>,
    cic_decimation: usize,
    comp_filter: Option<FirFilter>,
    fir_decimation: usize,
    fir_counter: usize,
    input_rate: f64,
    output_rate: f64,
}

impl Ddc {
    /// Create a DDC for the given channel.
    ///
    /// `center_freq`: channel center frequency offset from DC (Hz). This is the
    ///   offset from the tuner center, not the absolute RF frequency.
    /// `input_rate`: sample rate of the input signal (Hz)
    /// `output_rate`: desired output sample rate (Hz)
    /// `bandwidth`: desired channel bandwidth (Hz). Used to design the FIR filter.
    pub fn new(center_freq: f64, input_rate: f64, output_rate: f64, bandwidth: f64) -> Self {
        let total_decimation = (input_rate / output_rate).round() as usize;
        let total_decimation = total_decimation.max(1);

        // Create NCO for frequency translation
        let mut nco = Nco::new(input_rate);
        nco.set_frequency(center_freq);

        // Factor the total decimation into CIC × FIR stages
        // CIC is efficient for large factors; FIR handles the fine selectivity.
        // Strategy: CIC takes the largest power-of-2 factor ≤ total/2 (min 1),
        // leaving the remainder for FIR.
        let (cic_dec, fir_dec) = factor_decimation(total_decimation);

        // Build CIC if needed (3-stage for good balance of droop vs rejection)
        let cic_stages = 3;
        let cic = if cic_dec > 1 {
            Some(CicDecimator::new(cic_dec, cic_stages))
        } else {
            None
        };

        // Build compensation + channel FIR at the CIC output rate
        let intermediate_rate = input_rate / cic_dec as f64;
        let comp_filter = if cic_dec > 1 {
            // CIC compensation FIR that also acts as the channel filter
            // Passband: bandwidth/2, transition to intermediate Nyquist
            let passband_frac = bandwidth / intermediate_rate;
            let num_taps = (64 * fir_dec).min(511) | 1; // Odd, proportional to decimation

            // Start with CIC compensation kernel
            let comp_coeffs =
                cic_compensation_fir(cic_dec, cic_stages, num_taps, passband_frac.min(0.9));

            Some(FirFilter::new(&comp_coeffs))
        } else if fir_dec > 1 {
            // No CIC, just a lowpass FIR for channel selection
            // cutoff = (bw/2) / (fs/2) = bw/fs, normalized to Nyquist
            let cutoff = bandwidth / input_rate;
            let num_taps = (64 * fir_dec).min(511) | 1;
            let coeffs = crate::dsp::filter_design::firwin_lowpass(
                cutoff,
                num_taps,
                &crate::dsp::windows::WindowType::BlackmanHarris4,
            );
            Some(FirFilter::new(&coeffs))
        } else {
            None
        };

        Self {
            nco,
            cic,
            cic_decimation: cic_dec,
            comp_filter,
            fir_decimation: fir_dec,
            fir_counter: 0,
            input_rate,
            output_rate,
        }
    }

    /// Process a block of input samples through the DDC chain.
    ///
    /// Returns baseband samples at approximately `output_rate`.
    pub fn process(&mut self, input: &[Sample]) -> Vec<Sample> {
        // Step 1: Frequency translation (NCO mix to baseband)
        let mut mixed = input.to_vec();
        self.nco.mix(&mut mixed);

        // Step 2: CIC decimation (bulk rate reduction)
        let after_cic = if let Some(ref mut cic) = self.cic {
            cic.process(&mixed)
        } else {
            mixed
        };

        // Step 3: FIR compensation/channel filter + optional decimation
        if let Some(ref mut fir) = self.comp_filter {
            if self.fir_decimation > 1 {
                // Polyphase-style decimation: feed delay line for non-output
                // samples, only compute full FIR convolution on output samples.
                let mut output = Vec::with_capacity(after_cic.len() / self.fir_decimation + 1);
                for &sample in &after_cic {
                    self.fir_counter += 1;
                    if self.fir_counter >= self.fir_decimation {
                        self.fir_counter = 0;
                        output.push(fir.process_sample(sample));
                    } else {
                        fir.push_sample(sample);
                    }
                }
                output
            } else {
                // Filter only, no decimation
                let filtered = after_cic;
                fir.process_block(&filtered)
            }
        } else {
            after_cic
        }
    }

    /// Retune the DDC to a new center frequency.
    pub fn set_frequency(&mut self, center_freq: f64) {
        self.nco.set_frequency(center_freq);
    }

    /// Current NCO frequency in Hz.
    pub fn frequency(&self) -> f64 {
        self.nco.frequency()
    }

    /// Input sample rate.
    pub fn input_rate(&self) -> f64 {
        self.input_rate
    }

    /// Output sample rate.
    pub fn output_rate(&self) -> f64 {
        self.output_rate
    }

    /// Total decimation factor.
    pub fn decimation(&self) -> usize {
        self.cic_decimation * self.fir_decimation
    }

    pub fn reset(&mut self) {
        self.nco.reset();
        if let Some(ref mut cic) = self.cic {
            cic.reset();
        }
        if let Some(ref mut fir) = self.comp_filter {
            fir.reset();
        }
        self.fir_counter = 0;
    }
}

/// Factor a total decimation into CIC × FIR stages.
///
/// For factors ≤ 16: FIR-only with polyphase decimation gives clean
/// alias rejection. The polyphase approach only computes outputs that
/// are kept, so CPU cost = (input_rate / M) × num_taps — practical
/// for factors up to 16.
///
/// For factors > 16: CIC handles bulk decimation, FIR cleans up.
fn factor_decimation(total: usize) -> (usize, usize) {
    if total <= 1 {
        return (1, 1);
    }
    // FIR-only with polyphase for moderate factors (clean alias rejection)
    if total <= 16 {
        return (1, total);
    }

    // For larger factors, split CIC × FIR.
    // Always leave at least 2× for FIR anti-aliasing.
    let max_cic = total / 2;
    let mut cic = 1;
    while cic * 2 <= max_cic && cic < 256 {
        cic *= 2;
    }
    let fir = total.div_ceil(cic);
    // Adjust CIC if FIR factor would be too large
    if fir > 16 {
        let cic2 = total.div_ceil(16);
        let cic2 = cic2.next_power_of_two().min(256);
        let fir2 = total.div_ceil(cic2);
        return (cic2, fir2.max(1));
    }

    (cic, fir.max(1))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nco_frequency_accuracy() {
        let mut nco = Nco::new(48000.0);
        nco.set_frequency(1000.0);

        // Generate 48 samples (1 ms at 48 kHz) and check the tone frequency
        // by measuring the phase advance per sample.
        let s0 = nco.next_sample();
        // Skip 47 samples
        for _ in 0..47 {
            nco.next_sample();
        }
        // After 48 samples at 1 kHz, we should be at phase = 48/48 = 1.0 cycle
        // The NCO phase should have advanced by 2π
        let phase = nco.phase_rad();
        // 48 samples × (1000/48000) cycles/sample = 1.0 cycle = 2π rad
        // But phase_rad wraps to [0, 2π), so it should be near 0 (after one full cycle)
        assert!(
            !(0.1..=2.0 * PI - 0.1).contains(&phase),
            "Phase after one cycle should be near 0: {phase:.4}"
        );

        // Also verify the first sample is close to cos(0) + j·sin(0) = (1, 0)
        assert!(
            (s0.re - 1.0).abs() < 0.01,
            "First sample I should be ~1.0: {}",
            s0.re
        );
        assert!(
            s0.im.abs() < 0.01,
            "First sample Q should be ~0.0: {}",
            s0.im
        );
    }

    #[test]
    fn nco_negative_frequency() {
        let mut nco = Nco::new(48000.0);
        nco.set_frequency(-1000.0);

        // First sample should still be (1, 0)
        let s0 = nco.next_sample();
        assert!((s0.re - 1.0).abs() < 0.01);

        // Second sample: negative frequency means Q goes negative (clockwise rotation)
        let s1 = nco.next_sample();
        assert!(
            s1.im < 0.0,
            "Negative frequency should give negative Q: {}",
            s1.im
        );
    }

    #[test]
    fn nco_mix_shifts_frequency() {
        let fs = 48000.0;
        let f_signal = 5000.0;
        let f_nco = 5000.0;

        let mut nco = Nco::new(fs);
        nco.set_frequency(f_nco);

        // Create a complex tone at f_signal
        let n = 4800;
        let mut samples: Vec<Sample> = (0..n)
            .map(|i| {
                let t = 2.0 * PI * f_signal * i as f64 / fs;
                Sample::new(t.cos() as f32, t.sin() as f32)
            })
            .collect();

        // Mix down: should produce DC (signal at 0 Hz)
        nco.mix(&mut samples);

        // After mixing, the signal should be approximately constant (DC)
        // Check the last portion (skip transient from LUT interpolation)
        let mean_i: f32 = samples[100..].iter().map(|s| s.re).sum::<f32>() / (n - 100) as f32;
        let mean_q: f32 = samples[100..].iter().map(|s| s.im).sum::<f32>() / (n - 100) as f32;

        // Variance should be very low (near-DC signal)
        let var: f32 = samples[100..]
            .iter()
            .map(|s| (s.re - mean_i).powi(2) + (s.im - mean_q).powi(2))
            .sum::<f32>()
            / (n - 100) as f32;

        assert!(
            var < 0.01,
            "Mixed signal should be near-DC: variance = {var:.6}"
        );
    }

    #[test]
    fn nco_quadrature_orthogonality() {
        // I and Q should be 90° apart: verify I² + Q² ≈ 1 for all samples
        let mut nco = Nco::new(48000.0);
        nco.set_frequency(7777.0); // Arbitrary frequency

        let max_error: f32 = (0..10000)
            .map(|_| {
                let s = nco.next_sample();
                (s.re * s.re + s.im * s.im - 1.0).abs()
            })
            .fold(0.0f32, f32::max);

        assert!(
            max_error < 0.01,
            "NCO magnitude should be ~1.0, max error: {max_error:.6}"
        );
    }

    #[test]
    fn nco_lut_sfdr() {
        // Measure SFDR: generate a tone and compute via FFT
        // The LUT should give > 60 dB SFDR for a 1024-point table
        let mut nco = Nco::new(48000.0);
        nco.set_frequency(3000.0); // Bin-centered in a 1024-point FFT

        let n = 1024;
        let samples: Vec<Sample> = (0..n).map(|_| nco.next_sample()).collect();

        // Compute power spectrum (use simple DFT for small size)
        let mut max_spur = 0.0f64;
        let mut fundamental = 0.0f64;
        let fund_bin = (3000.0 / 48000.0 * n as f64).round() as usize;

        for k in 0..n / 2 {
            let mut sum_re = 0.0f64;
            let mut sum_im = 0.0f64;
            for (i, sample) in samples.iter().enumerate() {
                let angle = -2.0 * PI * k as f64 * i as f64 / n as f64;
                sum_re += sample.re as f64 * angle.cos() - sample.im as f64 * angle.sin();
                sum_im += sample.re as f64 * angle.sin() + sample.im as f64 * angle.cos();
            }
            let power = sum_re * sum_re + sum_im * sum_im;

            if k == fund_bin || (k as isize - fund_bin as isize).unsigned_abs() <= 1 {
                fundamental = fundamental.max(power);
            } else if k > 0 {
                max_spur = max_spur.max(power);
            }
        }

        let sfdr = 10.0 * (fundamental / max_spur.max(1e-20)).log10();
        assert!(sfdr > 55.0, "NCO SFDR should be > 55 dB, got {sfdr:.1} dB");
    }

    #[test]
    fn ddc_decimation_ratio() {
        let ddc = Ddc::new(0.0, 2_048_000.0, 16000.0, 10000.0);

        // Decimation should be approximately 2048000/16000 = 128
        let dec = ddc.decimation();
        assert!(
            (120..=136).contains(&dec),
            "Decimation should be ~128, got {dec}"
        );
    }

    #[test]
    fn ddc_output_length() {
        let mut ddc = Ddc::new(0.0, 2_048_000.0, 16000.0, 10000.0);
        let dec = ddc.decimation();

        // Feed exactly dec * 100 samples
        let n = dec * 100;
        let input = vec![Sample::new(0.5, 0.0); n];
        let output = ddc.process(&input);

        // Should get approximately 100 output samples
        assert!(
            output.len() >= 90 && output.len() <= 110,
            "Expected ~100 output samples, got {}",
            output.len()
        );
    }

    #[test]
    fn ddc_shifts_signal_to_baseband() {
        let fs = 256000.0;
        let f_offset = 50000.0;
        let output_rate = 16000.0;

        let mut ddc = Ddc::new(f_offset, fs, output_rate, 8000.0);

        // Create a signal at f_offset — after DDC it should be at baseband
        let n = 25600; // 100ms
        let input: Vec<Sample> = (0..n)
            .map(|i| {
                let t = 2.0 * PI * f_offset * i as f64 / fs;
                Sample::new(t.cos() as f32, t.sin() as f32)
            })
            .collect();

        let output = ddc.process(&input);

        // The output should be approximately DC (constant amplitude)
        // Skip initial transient
        let skip = output.len() / 4;
        if output.len() > skip + 10 {
            let mean_re: f32 =
                output[skip..].iter().map(|s| s.re).sum::<f32>() / (output.len() - skip) as f32;

            let var: f32 = output[skip..]
                .iter()
                .map(|s| (s.re - mean_re).powi(2))
                .sum::<f32>()
                / (output.len() - skip) as f32;

            assert!(
                var < 0.1,
                "DDC output should be near-DC: variance = {var:.4}"
            );
        }
    }

    #[test]
    fn factor_decimation_small() {
        assert_eq!(factor_decimation(1), (1, 1));
        assert_eq!(factor_decimation(2), (1, 2));
        assert_eq!(factor_decimation(3), (1, 3));
    }

    #[test]
    fn factor_decimation_powers_of_2() {
        let (cic, fir) = factor_decimation(128);
        assert_eq!(cic * fir, 128);
        assert!(cic >= 4, "CIC should be used for 128x: cic={cic}");
    }

    #[test]
    fn factor_decimation_product_matches() {
        for total in [4, 8, 16, 32, 64, 100, 128, 200, 256] {
            let (cic, fir) = factor_decimation(total);
            // Product should be >= total (ceiling allowed)
            assert!(
                cic * fir >= total && cic * fir <= total + cic,
                "factor_decimation({total}) = ({cic}, {fir}), product = {}",
                cic * fir
            );
        }
    }
}
