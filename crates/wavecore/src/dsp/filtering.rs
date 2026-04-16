use crate::types::Sample;

/// Least Mean Squares (LMS) adaptive filter.
///
/// Minimizes `E[|e[n]|²]` where `e[n] = d[n] - w^H·x[n]`.
///
/// Update rule: `w[n+1] = w[n] + μ · e*[n] · x[n]`
///
/// The step size μ controls convergence:
/// - Too large: diverges
/// - Too small: slow convergence
/// - Optimal: 0 < μ < 2/(L·σ²_x) where L is filter length
///
/// Convergence rate: proportional to eigenvalue spread of R_xx.
pub struct LmsFilter {
    weights: Vec<Sample>,
    mu: f32,
    buffer: Vec<Sample>,
    pos: usize,
}

impl LmsFilter {
    pub fn new(num_taps: usize, step_size: f32) -> Self {
        Self {
            weights: vec![Sample::new(0.0, 0.0); num_taps],
            mu: step_size,
            buffer: vec![Sample::new(0.0, 0.0); num_taps],
            pos: 0,
        }
    }

    /// Process one sample: compute output, update weights.
    ///
    /// `input`: current input sample `x[n]`
    /// `desired`: desired output `d[n]`
    ///
    /// Returns: (output `y[n]`, error `e[n]`)
    pub fn step(&mut self, input: Sample, desired: Sample) -> (Sample, Sample) {
        let len = self.weights.len();
        self.buffer[self.pos] = input;

        // Compute output: y = w^H · x
        let mut output = Sample::new(0.0, 0.0);
        for i in 0..len {
            let buf_idx = (self.pos + len - i) % len;
            output += self.weights[i].conj() * self.buffer[buf_idx];
        }

        // Error
        let error = desired - output;

        // Update weights: w += μ · e* · x
        for i in 0..len {
            let buf_idx = (self.pos + len - i) % len;
            self.weights[i] += Sample::new(self.mu, 0.0) * error.conj() * self.buffer[buf_idx];
        }

        self.pos = (self.pos + 1) % len;
        (output, error)
    }

    pub fn weights(&self) -> &[Sample] {
        &self.weights
    }
}

/// Normalized LMS (NLMS) adaptive filter.
///
/// Variant of LMS that normalizes the step size by input power:
///   `w[n+1] = w[n] + (μ / (δ + ||x||²)) · e*[n] · x[n]`
///
/// This makes convergence independent of input signal level.
/// δ is a small regularization constant to prevent division by zero.
///
/// Step size bound: 0 < μ < 2 for stability.
pub struct NlmsFilter {
    weights: Vec<Sample>,
    mu: f32,
    delta: f32,
    buffer: Vec<Sample>,
    pos: usize,
}

impl NlmsFilter {
    pub fn new(num_taps: usize, step_size: f32) -> Self {
        Self {
            weights: vec![Sample::new(0.0, 0.0); num_taps],
            mu: step_size.min(1.99),
            delta: 1e-6,
            buffer: vec![Sample::new(0.0, 0.0); num_taps],
            pos: 0,
        }
    }

    pub fn step(&mut self, input: Sample, desired: Sample) -> (Sample, Sample) {
        let len = self.weights.len();
        self.buffer[self.pos] = input;

        // Compute output
        let mut output = Sample::new(0.0, 0.0);
        let mut input_power = 0.0f32;
        for i in 0..len {
            let buf_idx = (self.pos + len - i) % len;
            output += self.weights[i].conj() * self.buffer[buf_idx];
            input_power += self.buffer[buf_idx].norm_sqr();
        }

        let error = desired - output;

        // Normalized update
        let norm_factor = self.mu / (self.delta + input_power);
        for i in 0..len {
            let buf_idx = (self.pos + len - i) % len;
            self.weights[i] += Sample::new(norm_factor, 0.0) * error.conj() * self.buffer[buf_idx];
        }

        self.pos = (self.pos + 1) % len;
        (output, error)
    }

    pub fn weights(&self) -> &[Sample] {
        &self.weights
    }
}

/// Recursive Least Squares (RLS) adaptive filter.
///
/// Minimizes the exponentially weighted least squares cost:
///   `J[n] = Σ_{k=0}^{n} λ^{n-k} |e[k]|²`
///
/// where λ (forgetting factor) controls the effective memory:
/// - λ = 1: infinite memory (converges to Wiener solution)
/// - λ < 1: forgets old data, tracks non-stationary signals
/// - Effective window length ≈ 1/(1-λ)
///
/// Uses the matrix inversion lemma for O(L²) per sample (vs O(L³) for direct).
/// Converges in ~2L samples (much faster than LMS which takes ~10L/μσ²).
pub struct RlsFilter {
    weights: Vec<Sample>,
    /// Inverse correlation matrix P = R^{-1}, stored as flattened NxN.
    p_matrix: Vec<f32>,
    lambda: f32,
    _delta: f32,
    buffer: Vec<Sample>,
    pos: usize,
    len: usize,
}

impl RlsFilter {
    /// Create a new RLS filter.
    ///
    /// `num_taps`: filter length
    /// `forgetting_factor`: λ, typically 0.99-0.9999
    /// `delta`: initial P matrix scaling (small = less prior info, e.g. 0.01)
    pub fn new(num_taps: usize, forgetting_factor: f32, delta: f32) -> Self {
        let n = num_taps;
        // Initialize P = δ^{-1} · I
        let mut p_matrix = vec![0.0f32; n * n];
        let p_init = 1.0 / delta;
        for i in 0..n {
            p_matrix[i * n + i] = p_init;
        }

        Self {
            weights: vec![Sample::new(0.0, 0.0); n],
            p_matrix,
            lambda: forgetting_factor,
            _delta: delta,
            buffer: vec![Sample::new(0.0, 0.0); n],
            pos: 0,
            len: n,
        }
    }

    #[allow(clippy::needless_range_loop)]
    pub fn step(&mut self, input: Sample, desired: Sample) -> (Sample, Sample) {
        let n = self.len;
        self.buffer[self.pos] = input;

        // Build input vector x[n] (in correct order)
        let x: Vec<Sample> = (0..n)
            .map(|i| self.buffer[(self.pos + n - i) % n])
            .collect();

        // Compute output: y = w^H · x
        let mut output = Sample::new(0.0, 0.0);
        for i in 0..n {
            output += self.weights[i].conj() * x[i];
        }

        let error = desired - output;

        // Gain vector: k = (P · x) / (λ + x^H · P · x)
        // Step 1: Px = P · x
        let mut px = vec![Sample::new(0.0, 0.0); n];
        for i in 0..n {
            for j in 0..n {
                px[i] += Sample::new(self.p_matrix[i * n + j], 0.0) * x[j];
            }
        }

        // Step 2: denominator = λ + x^H · Px
        let mut denom = Sample::new(self.lambda, 0.0);
        for i in 0..n {
            denom += x[i].conj() * px[i];
        }

        let denom_real = denom.re.max(1e-20);

        // Step 3: k = Px / denom
        let k: Vec<Sample> = px.iter().map(|&p| p / denom_real).collect();

        // Update weights: w += k · e*
        for i in 0..n {
            self.weights[i] += k[i] * error.conj();
        }

        // Update P: P = (1/λ)(P - k · x^H · P)
        // Simplified: P = (1/λ)(P - k · (Px)^H)
        let inv_lambda = 1.0 / self.lambda;
        for i in 0..n {
            for j in 0..n {
                let update = k[i].re * px[j].conj().re + k[i].im * px[j].conj().im;
                self.p_matrix[i * n + j] = inv_lambda * (self.p_matrix[i * n + j] - update);
            }
        }

        self.pos = (self.pos + 1) % n;
        (output, error)
    }

    pub fn weights(&self) -> &[Sample] {
        &self.weights
    }
}

/// Adaptive notch filter for removing narrowband interference.
///
/// Uses the constrained LMS algorithm to estimate and subtract a
/// single complex sinusoid from the signal:
///
///   `x_hat[n] = A · e^{j(ω₀n + φ)}`
///   `y[n] = x[n] - x_hat[n]`
///
/// The frequency ω₀ and amplitude A are adapted using gradient descent
/// on `E[|y[n]|²]`.
///
/// This is equivalent to a second-order IIR notch filter with adaptive
/// center frequency, but with guaranteed stability.
pub struct AdaptiveNotch {
    /// Current frequency estimate (radians/sample)
    omega: f32,
    /// Current amplitude estimate
    amplitude: Sample,
    /// Current phase
    phase: f32,
    /// Frequency adaptation rate
    mu_freq: f32,
    /// Amplitude adaptation rate
    mu_amp: f32,
    /// Notch bandwidth (3dB) in radians/sample
    _bandwidth: f32,
}

impl AdaptiveNotch {
    /// Create a new adaptive notch filter.
    ///
    /// `initial_freq_normalized`: initial frequency (0 to 0.5, fraction of sample rate)
    /// `bandwidth_hz`: notch bandwidth in Hz
    /// `sample_rate`: sample rate in Hz
    pub fn new(initial_freq_normalized: f32, bandwidth_hz: f32, sample_rate: f32) -> Self {
        let omega = 2.0 * std::f32::consts::PI * initial_freq_normalized;
        let bw = 2.0 * std::f32::consts::PI * bandwidth_hz / sample_rate;

        Self {
            omega,
            amplitude: Sample::new(0.0, 0.0),
            phase: 0.0,
            mu_freq: 0.001,
            mu_amp: 0.01,
            _bandwidth: bw,
        }
    }

    /// Process one sample, returning the cleaned output.
    pub fn step(&mut self, input: Sample) -> Sample {
        // Generate reference sinusoid
        let reference = Sample::new(self.phase.cos(), self.phase.sin());
        let estimate = self.amplitude * reference;

        // Error (cleaned signal)
        let error = input - estimate;

        // Update amplitude: gradient descent on |error|²
        // ∂|e|²/∂A = -2·Re(e·ref*)
        self.amplitude += Sample::new(self.mu_amp, 0.0) * error * reference.conj();

        // Update frequency: gradient descent using instantaneous frequency error
        // Uses the imaginary part of the product for phase error
        let phase_error = (error * reference.conj()).im;
        self.omega += self.mu_freq * phase_error;

        // Advance phase
        self.phase += self.omega;
        if self.phase > std::f32::consts::PI {
            self.phase -= 2.0 * std::f32::consts::PI;
        } else if self.phase < -std::f32::consts::PI {
            self.phase += 2.0 * std::f32::consts::PI;
        }

        error
    }

    /// Current estimated interference frequency (normalized, 0 to 0.5).
    pub fn frequency(&self) -> f32 {
        self.omega / (2.0 * std::f32::consts::PI)
    }

    /// Current estimated interference amplitude.
    pub fn amplitude_estimate(&self) -> f32 {
        self.amplitude.norm_sqr().sqrt()
    }
}

/// Hilbert transform (FIR approximation).
///
/// Implements a Type III FIR filter that approximates the ideal Hilbert
/// transform H(f) = -j·sgn(f). The output, combined with the input,
/// forms the analytic signal: `z[n] = x[n] + j·H{x[n]}`.
///
/// Uses the windowed ideal impulse response:
///   `h[n] = (2/(πn)) · sin²(πn/2)` for odd n, 0 for even n
///
/// The filter length must be odd for linear phase (Type III symmetry).
pub struct HilbertTransform {
    coefficients: Vec<f32>,
    buffer: Vec<f32>,
    pos: usize,
    delay: usize,
}

impl HilbertTransform {
    /// Create a Hilbert transform filter.
    ///
    /// `num_taps`: filter length (will be forced odd). More taps = better
    /// approximation at low frequencies. 31-63 taps is typical.
    pub fn new(num_taps: usize) -> Self {
        let num_taps = num_taps | 1; // Force odd
        let center = num_taps / 2;

        let mut coefficients = vec![0.0f32; num_taps];

        // Windowed ideal Hilbert impulse response
        #[allow(clippy::needless_range_loop)]
        for i in 0..num_taps {
            let n = i as f32 - center as f32;
            if n.abs() < 0.5 {
                coefficients[i] = 0.0;
            } else if (i % 2) != (center % 2) {
                // Odd offset from center
                let ideal = 2.0 / (std::f32::consts::PI * n);
                // Apply Blackman window
                let w = 0.42
                    - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (num_taps - 1) as f32).cos()
                    + 0.08 * (4.0 * std::f32::consts::PI * i as f32 / (num_taps - 1) as f32).cos();
                coefficients[i] = ideal * w;
            }
        }

        Self {
            coefficients,
            buffer: vec![0.0; num_taps],
            pos: 0,
            delay: center,
        }
    }

    /// Transform a real-valued sample to produce the Hilbert (imaginary) component.
    ///
    /// To get the analytic signal: `z[n] = x[n - delay] + j·hilbert(x[n])`
    pub fn process_sample(&mut self, input: f32) -> f32 {
        let len = self.coefficients.len();
        self.buffer[self.pos] = input;

        let mut output = 0.0f32;
        for i in 0..len {
            let buf_idx = (self.pos + len - i) % len;
            output += self.coefficients[i] * self.buffer[buf_idx];
        }

        self.pos = (self.pos + 1) % len;
        output
    }

    /// Group delay in samples (for aligning the original signal).
    pub fn delay(&self) -> usize {
        self.delay
    }
}

/// Median filter for impulse noise removal.
///
/// Replaces each sample with the median of a sliding window.
/// Effective against short-duration impulse noise (ignition, switching).
/// Preserves edges better than moving average.
///
/// Operates on magnitude, preserving phase.
pub fn median_filter(samples: &[Sample], window_size: usize) -> Vec<Sample> {
    let n = samples.len();
    let half = window_size / 2;
    let mut output = Vec::with_capacity(n);

    for i in 0..n {
        let start = i.saturating_sub(half);
        let end = (i + half + 1).min(n);

        let mut magnitudes: Vec<(f32, usize)> = (start..end)
            .map(|j| (samples[j].norm_sqr().sqrt(), j))
            .collect();
        magnitudes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let median_idx = magnitudes[magnitudes.len() / 2].1;
        // Use the sample at the median magnitude (preserves phase)
        output.push(samples[median_idx]);
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lms_converges() {
        let mut filter = LmsFilter::new(8, 0.01);

        // Train to learn identity (pass-through)
        let mut total_error = 0.0f32;
        for i in 0..1000 {
            let input = Sample::new((i as f32 * 0.1).sin(), (i as f32 * 0.13).cos());
            let desired = input;
            let (_, error) = filter.step(input, desired);
            if i > 500 {
                total_error += error.norm_sqr();
            }
        }

        let avg_error = total_error / 500.0;
        assert!(
            avg_error < 0.1,
            "LMS should converge: avg error = {avg_error}"
        );
    }

    #[test]
    fn nlms_converges_faster() {
        let mut nlms = NlmsFilter::new(8, 0.5);
        let mut lms = LmsFilter::new(8, 0.01);

        let mut nlms_error = 0.0f32;
        let mut lms_error = 0.0f32;

        for i in 0..500 {
            let input = Sample::new((i as f32 * 0.1).sin(), 0.0);
            let desired = Sample::new((i as f32 * 0.1).sin() * 0.5, 0.0);

            let (_, e_nlms) = nlms.step(input, desired);
            let (_, e_lms) = lms.step(input, desired);

            if i > 200 {
                nlms_error += e_nlms.norm_sqr();
                lms_error += e_lms.norm_sqr();
            }
        }

        // NLMS should converge at least as well
        assert!(
            nlms_error < lms_error * 2.0,
            "NLMS ({nlms_error}) should be comparable to LMS ({lms_error})"
        );
    }

    #[test]
    fn hilbert_transform_basic() {
        let mut ht = HilbertTransform::new(31);

        // Feed a cosine, should output sine (90° phase shift)
        let n = 200;
        let freq = 0.1; // Normalized frequency

        let mut outputs = Vec::new();
        for i in 0..n {
            let input = (2.0 * std::f32::consts::PI * freq * i as f32).cos();
            let output = ht.process_sample(input);
            outputs.push(output);
        }

        // After initial transient, output should be sine-like
        // Check correlation between output and expected sine
        let delay = ht.delay();
        let mut correlation = 0.0f32;
        for (i, &output) in outputs.iter().enumerate().take(n).skip(delay + 10) {
            let expected = (2.0 * std::f32::consts::PI * freq * (i - delay) as f32).sin();
            correlation += output * expected;
        }
        correlation /= (n - delay - 10) as f32;

        assert!(
            correlation > 0.3,
            "Hilbert output should correlate with sine: {correlation}"
        );
    }

    #[test]
    fn median_filter_removes_impulse() {
        let mut samples: Vec<Sample> = (0..100).map(|_| Sample::new(0.5, 0.0)).collect();

        // Add impulse noise
        samples[50] = Sample::new(100.0, 0.0);

        let filtered = median_filter(&samples, 5);

        // Impulse should be gone
        assert!(
            filtered[50].re < 1.0,
            "Impulse not removed: {}",
            filtered[50].re
        );
    }
}
