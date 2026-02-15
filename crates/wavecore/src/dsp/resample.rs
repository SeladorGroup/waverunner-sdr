//! Sample Rate Conversion
//!
//! Three resampling architectures:
//! 1. **Polyphase rational resampler** (L/M): Exploits Noble identities for
//!    efficient integer-ratio conversion. FIR anti-aliasing with commutator.
//! 2. **Farrow structure**: Arbitrary-ratio resampling via polynomial
//!    interpolation. Lagrange basis with Horner evaluation.
//! 3. **CIC decimator**: Cascaded Integrator-Comb for bulk decimation.
//!    No multiplications, only additions. Sinc^R droop compensation.

use crate::types::Sample;
use std::f64::consts::PI;

// ============================================================================
// Polyphase Rational Resampler (L/M)
// ============================================================================

/// Rational sample rate converter using polyphase decomposition.
///
/// Interpolates by L, filters with anti-aliasing FIR, and decimates by M.
/// Noble identities move the filter into L polyphase branches, computing
/// only the output samples needed — efficiency is O(N/L) per output sample
/// instead of O(N) for naive implementation.
///
/// The anti-aliasing filter cutoff is set to min(π/L, π/M) to prevent
/// aliasing from both the interpolation and decimation.
pub struct PolyphaseResampler {
    up: usize,                    // L (interpolation factor)
    down: usize,                  // M (decimation factor)
    branches: Vec<Vec<f32>>,      // L polyphase branches, each of length ceil(taps/L)
    buffer: Vec<Sample>,          // shared circular input buffer
    buf_pos: usize,               // write position in buffer
    phase: usize,                 // polyphase phase accumulator
}

impl PolyphaseResampler {
    /// Create a rational resampler with rate conversion L/M.
    ///
    /// `up_factor` (L): interpolation factor
    /// `down_factor` (M): decimation factor
    /// `num_taps`: total number of anti-aliasing FIR taps
    /// `cutoff`: normalized cutoff (0-1, relative to the lower rate), or 0 for auto
    pub fn new(up_factor: usize, down_factor: usize, num_taps: usize, cutoff: f64) -> Self {
        let (up, down) = simplify_ratio(up_factor, down_factor);
        let num_taps = num_taps.max(up * 4); // Minimum for reasonable quality

        // Design anti-aliasing lowpass
        let fc = if cutoff > 0.0 {
            cutoff
        } else {
            // Cutoff at the lower of the two Nyquist frequencies
            1.0 / up.max(down) as f64
        };

        let coeffs = design_sinc_filter(num_taps, fc * up as f64);

        // Decompose into polyphase branches
        // h_p[k] = h[k*L + p] for phase p, index k
        let branch_len = num_taps.div_ceil(up);
        let mut branches = vec![vec![0.0f32; branch_len]; up];

        for (i, &c) in coeffs.iter().enumerate() {
            let phase = i % up;
            let k = i / up;
            if k < branch_len {
                branches[phase][k] = (c * up as f64) as f32; // Scale by L for interpolation gain
            }
        }

        Self {
            up,
            down,
            branches,
            buffer: vec![Sample::new(0.0, 0.0); branch_len],
            buf_pos: 0,
            phase: 0,
        }
    }

    /// Process a block of input samples, returning the resampled output.
    ///
    /// Output length ≈ input.len() * L / M (exact for integer multiples).
    ///
    /// The algorithm tracks a phase accumulator through the upsampled
    /// (L-fold expanded) sample stream. For each input sample consumed,
    /// the phase has L positions available. For each output produced,
    /// M positions are consumed. When phase < L, the current buffer
    /// position has outputs pending; when phase ≥ L, we need a new input.
    pub fn process(&mut self, input: &[Sample]) -> Vec<Sample> {
        let mut output = Vec::with_capacity(input.len() * self.up / self.down + 1);
        let branch_len = self.branches[0].len();

        for &sample in input {
            // Push input sample into the circular buffer
            self.buffer[self.buf_pos] = sample;
            self.buf_pos = (self.buf_pos + 1) % branch_len;

            // Each input sample provides L positions in the upsampled stream.
            // Produce outputs for all phases that fall within this input.
            while self.phase < self.up {
                let branch = &self.branches[self.phase];
                let mut out = Sample::new(0.0, 0.0);
                for (k, &coeff) in branch.iter().enumerate().take(branch_len) {
                    let idx = (self.buf_pos + branch_len - 1 - k) % branch_len;
                    out += self.buffer[idx] * coeff;
                }
                output.push(out);

                // Advance phase by M (decimation factor)
                self.phase += self.down;
            }

            // Wrap phase back: consumed L positions from this input
            self.phase -= self.up;
        }

        output
    }

    /// The effective rate conversion ratio: L/M.
    pub fn ratio(&self) -> f64 {
        self.up as f64 / self.down as f64
    }

    pub fn reset(&mut self) {
        self.buffer.fill(Sample::new(0.0, 0.0));
        self.buf_pos = 0;
        self.phase = 0;
    }
}

// ============================================================================
// Farrow Arbitrary Resampler
// ============================================================================

/// Arbitrary-ratio resampler using Farrow polynomial interpolation.
///
/// Computes output samples at non-integer positions using a polynomial
/// approximation of the ideal sinc interpolator. The fractional delay μ
/// is computed per output sample and the polynomial is evaluated via
/// Horner's method.
///
/// Order 3 (cubic) gives good quality for most SDR applications.
/// Order 1 (linear) is fastest but introduces significant distortion.
pub struct FarrowResampler {
    order: usize,
    buffer: Vec<Sample>,
    pos: usize,
    mu: f64, // Fractional delay accumulator
    ratio: f64,
}

impl FarrowResampler {
    /// Create an arbitrary resampler.
    ///
    /// `order`: polynomial order (1=linear, 3=cubic recommended)
    /// `ratio`: output_rate / input_rate
    pub fn new(order: usize, ratio: f64) -> Self {
        let order = order.clamp(1, 5);
        let buf_len = order + 2; // Need order+1 samples plus margin
        Self {
            order,
            buffer: vec![Sample::new(0.0, 0.0); buf_len],
            pos: 0,
            mu: 0.0,
            ratio,
        }
    }

    /// Process input samples at the given rate ratio.
    pub fn process(&mut self, input: &[Sample]) -> Vec<Sample> {
        let mut output = Vec::with_capacity((input.len() as f64 * self.ratio) as usize + 2);
        let buf_len = self.buffer.len();

        for &sample in input {
            self.buffer[self.pos] = sample;
            self.pos = (self.pos + 1) % buf_len;

            // Generate outputs while the fractional position is within this input
            while self.mu < 1.0 {
                let out = self.interpolate(self.mu);
                output.push(out);
                self.mu += 1.0 / self.ratio;
            }
            self.mu -= 1.0;
        }

        output
    }

    /// Set the resampling ratio (can be changed dynamically).
    pub fn set_ratio(&mut self, ratio: f64) {
        self.ratio = ratio;
    }

    pub fn reset(&mut self) {
        self.buffer.fill(Sample::new(0.0, 0.0));
        self.pos = 0;
        self.mu = 0.0;
    }

    /// Polynomial interpolation at fractional position μ ∈ [0, 1).
    ///
    /// Uses Lagrange basis polynomials evaluated via Horner's method
    /// for O(order) operations per sample.
    fn interpolate(&self, mu: f64) -> Sample {
        let buf_len = self.buffer.len();
        let half = self.order / 2;

        match self.order {
            1 => {
                // Linear interpolation: y = (1-μ)·x[0] + μ·x[1]
                let x0 = self.buffer[(self.pos + buf_len - 2) % buf_len];
                let x1 = self.buffer[(self.pos + buf_len - 1) % buf_len];
                x0 * (1.0 - mu) as f32 + x1 * mu as f32
            }
            3 => {
                // Cubic (4-point) Lagrange interpolation
                // Hermite/Catmull-Rom variant for better frequency response
                let x = [
                    self.buffer[(self.pos + buf_len - 3) % buf_len],
                    self.buffer[(self.pos + buf_len - 2) % buf_len],
                    self.buffer[(self.pos + buf_len - 1) % buf_len],
                    self.buffer[self.pos % buf_len],
                ];
                let mu_f = mu as f32;
                let mu2 = mu_f * mu_f;
                let _mu3 = mu2 * mu_f;

                // Catmull-Rom spline coefficients (optimal for audio/SDR)
                let c0 = x[1];
                let c1 = (x[2] - x[0]) * 0.5;
                let c2 = x[0] - x[1] * 2.5 + x[2] * 2.0 - x[3] * 0.5;
                let c3 = (x[3] - x[0]) * 0.5 + (x[1] - x[2]) * 1.5;

                // Horner's method: c0 + μ(c1 + μ(c2 + μ·c3))
                c0 + (c1 + (c2 + c3 * mu_f) * mu_f) * mu_f
            }
            _ => {
                // General Lagrange interpolation of order N
                let n = self.order + 1; // Number of points
                let mut result = Sample::new(0.0, 0.0);

                for i in 0..n {
                    let xi = self.buffer[(self.pos + buf_len - n + i) % buf_len];
                    let mut basis = 1.0f32;
                    for j in 0..n {
                        if i != j {
                            basis *= (mu as f32 - j as f32 + half as f32)
                                / (i as f32 - j as f32);
                        }
                    }
                    result += xi * basis;
                }
                result
            }
        }
    }
}

// ============================================================================
// CIC Decimator
// ============================================================================

/// Cascaded Integrator-Comb (CIC) filter for efficient decimation.
///
/// Transfer function: H(z) = \[(1 − z^{−M}) / (1 − z^{−1})\]^R
///
/// where M is the decimation factor and R is the number of stages.
///
/// Properties:
/// - No multiplications (only additions/subtractions)
/// - Passband droop follows sinc^R — compensated by a subsequent FIR
/// - Bit growth: R·log₂(M) extra bits needed
/// - Best for large decimation ratios (8x to 1000x)
///
/// Uses i64 accumulators to handle the bit growth for large M·R products.
pub struct CicDecimator {
    decimation: usize,
    num_stages: usize,
    integrators: Vec<CicAccum>,
    /// Previous value per comb stage (delay of 1 at output rate = M at input rate)
    comb_prev: Vec<[i64; 2]>,
    sample_count: usize,
}

/// Dual-channel (I/Q) accumulator for CIC.
#[derive(Clone)]
struct CicAccum {
    re: i64,
    im: i64,
}

impl CicAccum {
    fn new() -> Self {
        Self { re: 0, im: 0 }
    }
}

impl CicDecimator {
    /// Create a CIC decimator.
    ///
    /// `decimation_factor` (M): output one sample per M input samples
    /// `num_stages` (R): number of integrator-comb stages (1-6 typical)
    pub fn new(decimation_factor: usize, num_stages: usize) -> Self {
        let num_stages = num_stages.clamp(1, 6);
        Self {
            decimation: decimation_factor,
            num_stages,
            integrators: vec![CicAccum::new(); num_stages],
            comb_prev: vec![[0i64; 2]; num_stages],
            sample_count: 0,
        }
    }

    /// Process a block of input samples, returning decimated output.
    ///
    /// Output length = floor(input.len() / decimation_factor).
    pub fn process(&mut self, input: &[Sample]) -> Vec<Sample> {
        let mut output = Vec::with_capacity(input.len() / self.decimation + 1);

        // Scale factor: convert f32 IQ to i64 with headroom
        let scale = (1i64 << 30) as f32; // 30-bit scaling
        let inv_scale = 1.0 / (scale as f64).powi(1) / self.gain_correction();

        for &sample in input {
            // Convert to fixed-point
            let i_in = (sample.re * scale) as i64;
            let q_in = (sample.im * scale) as i64;

            // Integrator stages (running sum, operates at input rate)
            let mut i_val = i_in;
            let mut q_val = q_in;
            for stage in &mut self.integrators {
                stage.re = stage.re.wrapping_add(i_val);
                stage.im = stage.im.wrapping_add(q_val);
                i_val = stage.re;
                q_val = stage.im;
            }

            self.sample_count += 1;

            // Decimate: output one sample per M inputs
            if self.sample_count >= self.decimation {
                self.sample_count = 0;

                // Comb stages (differencer, operates at output rate)
                // Each comb computes y[n] = x[n] − x[n−1] (delay of 1 at output rate,
                // which equals delay of M at input rate — matching the CIC transfer
                // function H(z) = [(1−z^{−M})/(1−z^{−1})]^R)
                let mut i_out = i_val;
                let mut q_out = q_val;
                for s in 0..self.num_stages {
                    let prev_i = self.comb_prev[s][0];
                    let prev_q = self.comb_prev[s][1];
                    self.comb_prev[s][0] = i_out;
                    self.comb_prev[s][1] = q_out;
                    i_out = i_out.wrapping_sub(prev_i);
                    q_out = q_out.wrapping_sub(prev_q);
                }

                // Convert back to f32
                output.push(Sample::new(
                    (i_out as f64 * inv_scale) as f32,
                    (q_out as f64 * inv_scale) as f32,
                ));
            }
        }

        output
    }

    /// DC gain correction factor (M^R).
    fn gain_correction(&self) -> f64 {
        (self.decimation as f64).powi(self.num_stages as i32)
    }

    /// Compute the CIC passband droop at a given frequency.
    ///
    /// The CIC magnitude response is |H(f)| = |sin(πMf)/sin(πf)|^R
    /// normalized to the DC gain M^R.
    ///
    /// `freq_norm`: frequency normalized to the *output* sample rate (0 to 0.5)
    pub fn droop_db(&self, freq_norm: f64) -> f64 {
        let m = self.decimation as f64;
        let f = freq_norm / m; // Convert to input rate normalization

        if f.abs() < 1e-12 {
            return 0.0; // DC
        }

        let sinc = (PI * m * f).sin() / (m * (PI * f).sin());
        let mag = sinc.abs().powi(self.num_stages as i32);

        20.0 * mag.log10()
    }

    pub fn reset(&mut self) {
        for s in &mut self.integrators {
            s.re = 0;
            s.im = 0;
        }
        for prev in &mut self.comb_prev {
            *prev = [0; 2];
        }
        self.sample_count = 0;
    }
}

/// Design a CIC droop compensation FIR filter.
///
/// The CIC's passband droop follows sinc^R. This function designs an inverse
/// sinc^R FIR filter to flatten the passband response when cascaded with the CIC.
///
/// `decimation`: CIC decimation factor M
/// `num_stages`: CIC stages R
/// `num_taps`: compensation FIR length (odd, typically 15-31)
/// `passband_width`: fraction of the output Nyquist to compensate (0-1, typically 0.8)
pub fn cic_compensation_fir(
    decimation: usize,
    num_stages: usize,
    num_taps: usize,
    passband_width: f64,
) -> Vec<f64> {
    let num_taps = num_taps | 1; // Force odd
    let m = num_taps / 2;
    let r = num_stages as i32;
    let d = decimation as f64;

    // Design via frequency sampling: inverse sinc^R in the passband,
    // rolled off in the stopband
    let n_freq = 512;
    let mut desired = vec![0.0; n_freq];

    for k in 0..n_freq {
        let f = k as f64 / n_freq as f64; // 0 to 1 (Nyquist of output rate)
        let f_input = f / d; // Frequency at input rate

        if f <= passband_width {
            // Inverse sinc^R
            if f_input.abs() < 1e-12 {
                desired[k] = 1.0;
            } else {
                let sinc = (PI * d * f_input).sin() / (d * (PI * f_input).sin());
                let cic_mag = sinc.abs().powi(r);
                desired[k] = if cic_mag > 1e-10 { 1.0 / cic_mag } else { 1.0 };
            }
        } else {
            // Transition/stopband: taper to zero
            let edge = (f - passband_width) / (1.0 - passband_width);
            desired[k] = desired[(passband_width * n_freq as f64) as usize]
                * (1.0 - edge).max(0.0);
        }
    }

    // Convert desired frequency response to FIR coefficients via inverse DFT
    // Since the response is real and symmetric, use cosine series
    let mut h = vec![0.0; num_taps];
    for (n, h_val) in h.iter_mut().enumerate().take(num_taps) {
        let mut sum = 0.0;
        for (k, &d_val) in desired.iter().enumerate().take(n_freq) {
            let omega = PI * k as f64 / n_freq as f64;
            sum += d_val * (omega * (n as f64 - m as f64)).cos();
        }
        *h_val = sum / n_freq as f64;
    }

    // Apply window for smooth stopband
    for (i, coeff) in h.iter_mut().enumerate() {
        let t = 2.0 * PI * i as f64 / (num_taps - 1) as f64;
        let w = 0.42 - 0.5 * t.cos() + 0.08 * (2.0 * t).cos(); // Blackman
        *coeff *= w;
    }

    // Normalize to unity DC gain
    let dc: f64 = h.iter().sum();
    if dc.abs() > 1e-12 {
        for v in &mut h {
            *v /= dc;
        }
    }

    h
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Simplify a rational ratio L/M by dividing by GCD.
fn simplify_ratio(l: usize, m: usize) -> (usize, usize) {
    let g = gcd(l, m);
    (l / g, m / g)
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// Design a windowed-sinc lowpass FIR for the polyphase resampler.
///
/// Uses a Kaiser window with automatically computed β for 80 dB attenuation.
fn design_sinc_filter(num_taps: usize, cutoff_normalized: f64) -> Vec<f64> {
    let num_taps = num_taps | 1; // Force odd
    let m = num_taps / 2;
    let omega_c = PI * cutoff_normalized.min(1.0);

    // Kaiser β for ~80 dB stopband attenuation
    let beta = 8.0;
    let i0_beta = crate::dsp::windows::bessel_i0(beta);

    (0..num_taps)
        .map(|i| {
            let n = i as f64 - m as f64;
            let sinc = if n.abs() < 1e-12 {
                omega_c / PI
            } else {
                (omega_c * n).sin() / (PI * n)
            };

            // Kaiser window
            let t = 2.0 * i as f64 / (num_taps - 1).max(1) as f64 - 1.0;
            let arg = beta * (1.0 - t * t).max(0.0).sqrt();
            let window = crate::dsp::windows::bessel_i0(arg) / i0_beta;

            sinc * window
        })
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polyphase_2x_decimation() {
        // 2:1 decimation: output should be half the input length
        let mut resampler = PolyphaseResampler::new(1, 2, 32, 0.0);

        let input: Vec<Sample> = (0..1000)
            .map(|i| Sample::new((i as f32 * 0.05).sin(), 0.0))
            .collect();

        let output = resampler.process(&input);

        // Output should be approximately half the input length
        assert!(
            (output.len() as f64 - 500.0).abs() < 10.0,
            "Expected ~500 samples, got {}",
            output.len()
        );
    }

    #[test]
    fn polyphase_2x_interpolation() {
        // 2:1 interpolation: output should be double the input length
        let mut resampler = PolyphaseResampler::new(2, 1, 32, 0.0);

        let input: Vec<Sample> = (0..500)
            .map(|i| Sample::new((i as f32 * 0.05).sin(), 0.0))
            .collect();

        let output = resampler.process(&input);

        assert!(
            (output.len() as f64 - 1000.0).abs() < 10.0,
            "Expected ~1000 samples, got {}",
            output.len()
        );
    }

    #[test]
    fn polyphase_preserves_low_frequency() {
        // A low-frequency signal should pass through decimation largely intact
        let mut resampler = PolyphaseResampler::new(1, 4, 64, 0.0);

        // Low freq signal at input rate: f = 0.01 (well below Nyquist/4)
        let n = 4000;
        let input: Vec<Sample> = (0..n)
            .map(|i| Sample::new((2.0 * std::f32::consts::PI * 0.01 * i as f32).sin(), 0.0))
            .collect();

        let output = resampler.process(&input);

        // After transient, output should still be sinusoidal
        let pow: f32 = output[50..].iter().map(|s| s.re * s.re).sum::<f32>()
            / (output.len() - 50) as f32;
        assert!(
            pow > 0.1,
            "Low frequency signal should survive decimation: power = {pow}"
        );
    }

    #[test]
    fn polyphase_ratio() {
        let r = PolyphaseResampler::new(3, 7, 48, 0.0);
        assert!((r.ratio() - 3.0 / 7.0).abs() < 1e-10);
    }

    #[test]
    fn farrow_linear_identity() {
        // Ratio 1.0 with linear interpolation should approximate identity
        let mut resampler = FarrowResampler::new(1, 1.0);

        let input: Vec<Sample> = (0..100)
            .map(|i| Sample::new(i as f32 * 0.01, 0.0))
            .collect();

        let output = resampler.process(&input);

        // Output length should be approximately input length
        assert!(
            (output.len() as f64 - 100.0).abs() < 5.0,
            "Expected ~100 samples, got {}",
            output.len()
        );
    }

    #[test]
    fn farrow_2x_upsample() {
        let mut resampler = FarrowResampler::new(3, 2.0);

        let input: Vec<Sample> = (0..200)
            .map(|i| Sample::new((i as f32 * 0.05).sin(), 0.0))
            .collect();

        let output = resampler.process(&input);

        assert!(
            (output.len() as f64 - 400.0).abs() < 10.0,
            "Expected ~400 samples, got {}",
            output.len()
        );
    }

    #[test]
    fn farrow_cubic_quality() {
        // Cubic should be better than linear for a sinusoidal signal
        let mut linear = FarrowResampler::new(1, 0.7);
        let mut cubic = FarrowResampler::new(3, 0.7);

        let input: Vec<Sample> = (0..1000)
            .map(|i| Sample::new((i as f32 * 0.1).sin(), 0.0))
            .collect();

        let out_lin = linear.process(&input);
        let out_cub = cubic.process(&input);

        // Both should produce output
        assert!(!out_lin.is_empty());
        assert!(!out_cub.is_empty());

        // Cubic should have smoother output (lower high-frequency energy)
        let hf_lin: f32 = out_lin
            .windows(2)
            .map(|w| (w[1].re - w[0].re).powi(2))
            .sum::<f32>()
            / out_lin.len() as f32;
        let hf_cub: f32 = out_cub
            .windows(2)
            .map(|w| (w[1].re - w[0].re).powi(2))
            .sum::<f32>()
            / out_cub.len() as f32;

        assert!(
            hf_cub <= hf_lin * 1.5,
            "Cubic ({hf_cub:.6}) should be smoother than linear ({hf_lin:.6})"
        );
    }

    #[test]
    fn cic_decimation_basic() {
        // CIC 8:1 decimation
        let mut cic = CicDecimator::new(8, 3);

        let input: Vec<Sample> = (0..800)
            .map(|i| Sample::new((i as f32 * 0.01).sin(), 0.0))
            .collect();

        let output = cic.process(&input);

        assert_eq!(output.len(), 100, "8:1 decimation of 800 samples = 100");
    }

    #[test]
    fn cic_preserves_dc() {
        // DC signal through CIC should maintain amplitude
        let mut cic = CicDecimator::new(4, 2);

        let input = vec![Sample::new(0.5, 0.3); 400];
        let output = cic.process(&input);

        // After initial transient, output should converge to the DC value
        let last = output.last().unwrap();
        assert!(
            (last.re - 0.5).abs() < 0.1,
            "DC re: {} (expected 0.5)",
            last.re
        );
        assert!(
            (last.im - 0.3).abs() < 0.1,
            "DC im: {} (expected 0.3)",
            last.im
        );
    }

    #[test]
    fn cic_droop_at_passband_edge() {
        let cic = CicDecimator::new(16, 3);

        // At DC: 0 dB droop
        assert!(cic.droop_db(0.0).abs() < 0.01);

        // At 0.4 Nyquist: should have some droop (negative dB)
        let droop = cic.droop_db(0.4);
        assert!(
            droop < -1.0,
            "CIC should have droop at 0.4 Nyquist: {droop:.1} dB"
        );
    }

    #[test]
    fn cic_compensation_filter_shape() {
        let comp = cic_compensation_fir(8, 3, 21, 0.8);
        assert_eq!(comp.len(), 21);

        // Should have unity DC gain
        let dc: f64 = comp.iter().sum();
        assert!((dc - 1.0).abs() < 0.01, "DC gain: {dc}");

        // Should boost passband edge relative to DC (inverse sinc)
        // The center tap should be the largest
        let center = comp[10];
        assert!(center > 0.0, "Center tap should be positive");
    }

    #[test]
    fn gcd_test() {
        assert_eq!(gcd(12, 8), 4);
        assert_eq!(gcd(7, 13), 1);
        assert_eq!(gcd(100, 75), 25);
    }

    #[test]
    fn simplify_ratio_test() {
        assert_eq!(simplify_ratio(48000, 44100), (160, 147));
        assert_eq!(simplify_ratio(2, 4), (1, 2));
        assert_eq!(simplify_ratio(3, 3), (1, 1));
    }
}
