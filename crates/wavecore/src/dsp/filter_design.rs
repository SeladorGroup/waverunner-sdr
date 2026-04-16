//! FIR Filter Design and Runtime
//!
//! Three design methods:
//! 1. **Window method**: Truncated ideal impulse response × window function
//! 2. **Kaiser method**: Automatic window length and shape from specifications
//! 3. **Parks-McClellan (Remez)**: Optimal equiripple via Chebyshev approximation theory
//!
//! All design functions use normalized frequencies in \[0, 1\] where 1 = Nyquist (fs/2).

use std::f64::consts::PI;

use crate::dsp::windows::{WindowType, generate_window, kaiser_design};
use crate::types::Sample;

// ============================================================================
// Types
// ============================================================================

/// Error type for the Remez exchange algorithm.
#[derive(Debug, Clone)]
pub enum RemezError {
    InvalidBands(String),
    ConvergenceFailure {
        iterations: usize,
        delta_change: f64,
    },
    NumericalFailure(String),
}

impl std::fmt::Display for RemezError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBands(msg) => write!(f, "Invalid band specification: {msg}"),
            Self::ConvergenceFailure {
                iterations,
                delta_change,
            } => write!(
                f,
                "Remez failed to converge after {iterations} iterations (Δδ: {delta_change:.2e})"
            ),
            Self::NumericalFailure(msg) => write!(f, "Numerical failure: {msg}"),
        }
    }
}

impl std::error::Error for RemezError {}

/// Band specification for the Remez exchange algorithm.
///
/// Each band defines a contiguous frequency range with a desired response
/// and relative weight. Gaps between bands are transition bands where
/// the algorithm is unconstrained.
#[derive(Debug, Clone)]
pub struct RemezBand {
    /// Start frequency, normalized \[0, 1\] where 1 = Nyquist
    pub start: f64,
    /// End frequency, normalized \[0, 1\] where 1 = Nyquist
    pub end: f64,
    /// Desired magnitude response (typically 0.0 or 1.0)
    pub desired: f64,
    /// Relative weight (higher = tighter approximation in this band)
    pub weight: f64,
}

// ============================================================================
// Window-based FIR Design
// ============================================================================

/// Design a lowpass FIR filter using the window method.
///
/// The ideal lowpass impulse response h\_d\[n\] = sin(ωc·n)/(π·n) is
/// truncated to `num_taps` and multiplied by the specified window.
///
/// Frequencies are normalized: 0 = DC, 1 = Nyquist.
pub fn firwin_lowpass(cutoff: f64, num_taps: usize, window_type: &WindowType) -> Vec<f64> {
    let num_taps = num_taps | 1; // Force odd for Type I
    let m = num_taps / 2;
    let omega_c = PI * cutoff.clamp(0.0, 1.0);
    let window = generate_window(window_type, num_taps);

    (0..num_taps)
        .map(|i| {
            let n = i as f64 - m as f64;
            let h = if n.abs() < 1e-12 {
                omega_c / PI // L'Hôpital: sin(ωn)/(πn) → ω/π as n→0
            } else {
                (omega_c * n).sin() / (PI * n)
            };
            h * window[i]
        })
        .collect()
}

/// Design a highpass FIR filter using spectral inversion.
///
/// h\_hp\[n\] = δ\[n − M\] − h\_lp\[n\], exploiting H\_hp(ω) = 1 − H\_lp(ω).
pub fn firwin_highpass(cutoff: f64, num_taps: usize, window_type: &WindowType) -> Vec<f64> {
    let mut h = firwin_lowpass(cutoff, num_taps, window_type);
    let center = h.len() / 2;
    for v in h.iter_mut() {
        *v = -*v;
    }
    h[center] += 1.0;
    h
}

/// Design a bandpass FIR filter as the difference of two lowpass filters.
///
/// Passes frequencies in \[low, high\] (normalized).
pub fn firwin_bandpass(low: f64, high: f64, num_taps: usize, window_type: &WindowType) -> Vec<f64> {
    let num_taps = num_taps | 1;
    let m = num_taps / 2;
    let omega_lo = PI * low.clamp(0.0, 1.0);
    let omega_hi = PI * high.clamp(0.0, 1.0);
    let window = generate_window(window_type, num_taps);

    (0..num_taps)
        .map(|i| {
            let n = i as f64 - m as f64;
            let h = if n.abs() < 1e-12 {
                (omega_hi - omega_lo) / PI
            } else {
                ((omega_hi * n).sin() - (omega_lo * n).sin()) / (PI * n)
            };
            h * window[i]
        })
        .collect()
}

/// Design a bandstop FIR filter via spectral inversion of a bandpass.
pub fn firwin_bandstop(low: f64, high: f64, num_taps: usize, window_type: &WindowType) -> Vec<f64> {
    let mut h = firwin_bandpass(low, high, num_taps, window_type);
    let center = h.len() / 2;
    for v in h.iter_mut() {
        *v = -*v;
    }
    h[center] += 1.0;
    h
}

// ============================================================================
// Kaiser Window FIR Design
// ============================================================================

/// Design a lowpass FIR with automatically computed Kaiser window parameters.
///
/// Uses Kaiser's empirical formulas to determine β and length N from
/// the desired stopband attenuation and transition bandwidth.
pub fn kaiser_lowpass(cutoff: f64, transition_width: f64, attenuation_db: f64) -> Vec<f64> {
    let (beta, num_taps) = kaiser_design(attenuation_db, transition_width);
    firwin_lowpass(cutoff, num_taps, &WindowType::Kaiser { beta })
}

/// Design a bandpass FIR with automatically computed Kaiser window parameters.
pub fn kaiser_bandpass(
    low: f64,
    high: f64,
    transition_width: f64,
    attenuation_db: f64,
) -> Vec<f64> {
    let (beta, num_taps) = kaiser_design(attenuation_db, transition_width);
    firwin_bandpass(low, high, num_taps, &WindowType::Kaiser { beta })
}

// ============================================================================
// Parks-McClellan / Remez Exchange Algorithm
// ============================================================================

/// Design an optimal equiripple FIR filter using the Parks-McClellan algorithm.
///
/// Implements the Remez exchange algorithm to find the Chebyshev-optimal
/// (minimax) Type I linear-phase FIR filter. The resulting filter has
/// equiripple error in all specified bands.
///
/// For a Type I FIR of length N = 2M+1, the zero-phase response is:
///
///   A(ω) = Σ\_{k=0}^{M} a\_k cos(kω)
///
/// a polynomial of degree M in cos(ω). The algorithm minimizes:
///
///   min\_{a} max\_{ω ∈ bands} |W(ω)\[A(ω) − D(ω)\]|
///
/// via the Chebyshev alternation theorem: the optimal solution has at least
/// r = M + 2 extremal points where the weighted error attains its maximum
/// magnitude with alternating sign.
pub fn remez_fir(
    num_taps: usize,
    bands: &[RemezBand],
    max_iterations: usize,
) -> Result<Vec<f64>, RemezError> {
    let num_taps = num_taps | 1; // Force odd for Type I
    let m = (num_taps - 1) / 2; // Polynomial order
    let r = m + 2; // Number of extremals (Chebyshev alternation theorem)

    validate_bands(bands)?;

    // Dense frequency grid covering all specified bands
    let grid = create_dense_grid(bands, num_taps);
    if grid.len() < r {
        return Err(RemezError::InvalidBands(
            "Grid too sparse for filter order".into(),
        ));
    }

    // Map frequencies to Chebyshev domain: x = cos(πf)
    let grid_x: Vec<f64> = grid.iter().map(|g| (PI * g.freq).cos()).collect();

    // Initialize extremals uniformly distributed on the grid
    let mut ext_idx: Vec<usize> = (0..r).map(|i| i * (grid.len() - 1) / (r - 1)).collect();

    let mut prev_delta = 0.0f64;

    for iteration in 0..max_iterations {
        // Barycentric weights at extremal points (Chebyshev domain)
        let ext_x: Vec<f64> = ext_idx.iter().map(|&i| grid_x[i]).collect();
        let bary = barycentric_weights(&ext_x);

        // Compute equiripple deviation δ using the Neyman-Pearson formulation:
        //   δ = [Σ_j b_j D(ω_j)] / [Σ_j b_j (-1)^j / W(ω_j)]
        let (mut num, mut den) = (0.0, 0.0);
        for (j, &idx) in ext_idx.iter().enumerate() {
            let sign = if j % 2 == 0 { 1.0 } else { -1.0 };
            num += bary[j] * grid[idx].desired;
            den += bary[j] * sign / grid[idx].weight;
        }
        if den.abs() < 1e-300 {
            return Err(RemezError::NumericalFailure(
                "Zero denominator in δ computation".into(),
            ));
        }
        let delta = num / den;

        // Target values at extremals: y_j = D(ω_j) - (-1)^j · δ/W(ω_j)
        let ext_y: Vec<f64> = ext_idx
            .iter()
            .enumerate()
            .map(|(j, &idx)| {
                let sign = if j % 2 == 0 { 1.0 } else { -1.0 };
                grid[idx].desired - sign * delta / grid[idx].weight
            })
            .collect();

        // Evaluate A(ω) on full grid via second barycentric interpolation form
        let a_vals: Vec<f64> = grid_x
            .iter()
            .map(|&x| barycentric_eval(&ext_x, &ext_y, &bary, x))
            .collect();

        // Weighted error on the grid: E(ω) = W(ω)[A(ω) - D(ω)]
        let err: Vec<f64> = (0..grid.len())
            .map(|i| grid[i].weight * (a_vals[i] - grid[i].desired))
            .collect();

        // Exchange step: find new extremal set via alternation theorem
        let new_ext = find_extremals(&err, r);

        // Convergence: relative change in |δ|
        let delta_change = if prev_delta.abs() > 1e-30 {
            (delta.abs() - prev_delta.abs()).abs() / prev_delta.abs()
        } else {
            f64::MAX
        };

        ext_idx = new_ext;
        prev_delta = delta;

        if iteration > 3 && delta_change < 1e-12 {
            break;
        }
    }

    // Extract filter coefficients from the converged approximation
    extract_type1_coefficients(&ext_idx, &grid, &grid_x, prev_delta, m, num_taps)
}

// --- Remez internal types and helpers ---

struct GridPoint {
    freq: f64,
    desired: f64,
    weight: f64,
}

fn validate_bands(bands: &[RemezBand]) -> Result<(), RemezError> {
    if bands.is_empty() {
        return Err(RemezError::InvalidBands("No bands specified".into()));
    }
    for (i, b) in bands.iter().enumerate() {
        if b.start >= b.end || b.start < 0.0 || b.end > 1.0 {
            return Err(RemezError::InvalidBands(format!(
                "Band {i}: invalid range [{}, {}]",
                b.start, b.end
            )));
        }
        if b.weight <= 0.0 {
            return Err(RemezError::InvalidBands(format!(
                "Band {i}: weight must be positive"
            )));
        }
    }
    for i in 1..bands.len() {
        if bands[i].start < bands[i - 1].end {
            return Err(RemezError::InvalidBands(format!(
                "Band {i} overlaps with band {}",
                i - 1
            )));
        }
    }
    Ok(())
}

/// Create dense frequency grid covering all specified bands.
///
/// Grid density scales with filter length for adequate sampling
/// of the equiripple oscillations in the error function.
fn create_dense_grid(bands: &[RemezBand], num_taps: usize) -> Vec<GridPoint> {
    let density = 16 * num_taps; // Points per unit normalized frequency
    let mut grid = Vec::new();

    for band in bands {
        let bw = band.end - band.start;
        let n_points = ((bw * density as f64).ceil() as usize).max(4);
        for i in 0..n_points {
            let freq = band.start + bw * i as f64 / (n_points - 1).max(1) as f64;
            grid.push(GridPoint {
                freq,
                desired: band.desired,
                weight: band.weight,
            });
        }
    }

    grid
}

/// Compute barycentric interpolation weights in log-domain for stability.
///
/// For nodes x\_0,...,x\_{r-1}, the barycentric weight is:
///   w\_j = 1 / Π\_{k≠j} (x\_j − x\_k)
///
/// Direct products overflow for large r; log-domain computation with
/// median centering prevents this.
fn barycentric_weights(x: &[f64]) -> Vec<f64> {
    let r = x.len();
    if r == 0 {
        return Vec::new();
    }
    if r == 1 {
        return vec![1.0];
    }

    // Accumulate log|Π(x_j - x_k)| and sign for each j
    let mut log_abs_prod = vec![0.0; r];
    let mut signs = vec![1i8; r];

    for j in 0..r {
        for k in 0..r {
            if k != j {
                let diff = x[j] - x[k];
                if diff.abs() < 1e-300 {
                    log_abs_prod[j] = f64::MAX / 2.0;
                } else {
                    log_abs_prod[j] += diff.abs().ln();
                    if diff < 0.0 {
                        signs[j] *= -1;
                    }
                }
            }
        }
    }

    // w_j = sign_j / exp(log_abs_prod_j)
    // Center by subtracting the median to prevent underflow/overflow
    let mut sorted_logs = log_abs_prod.clone();
    sorted_logs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted_logs[r / 2];

    let mut w: Vec<f64> = (0..r)
        .map(|j| {
            let s = if signs[j] > 0 { 1.0 } else { -1.0 };
            s * (-(log_abs_prod[j] - median)).exp()
        })
        .collect();

    // Normalize so max|w| = 1 (barycentric formula is scale-invariant)
    let max_w = w.iter().map(|v| v.abs()).fold(0.0f64, f64::max);
    if max_w > 0.0 {
        for v in &mut w {
            *v /= max_w;
        }
    }

    w
}

/// Evaluate the barycentric interpolant at point x.
///
/// Second barycentric form for O(r) evaluation:
///   L(x) = \[Σ\_j w\_j y\_j / (x − x\_j)\] / \[Σ\_j w\_j / (x − x\_j)\]
///
/// Returns y\_j directly when x coincides with a node (avoiding 0/0).
fn barycentric_eval(nodes: &[f64], values: &[f64], weights: &[f64], x: f64) -> f64 {
    let mut num = 0.0;
    let mut den = 0.0;

    for j in 0..nodes.len() {
        let diff = x - nodes[j];
        if diff.abs() < 1e-14 {
            return values[j];
        }
        let t = weights[j] / diff;
        num += t * values[j];
        den += t;
    }

    if den.abs() < 1e-300 { 0.0 } else { num / den }
}

/// Find r extremal indices from the weighted error vector.
///
/// Identifies local extrema with alternating signs (Chebyshev alternation
/// theorem), then selects the r with largest magnitude by trimming
/// from the ends of the alternating chain.
fn find_extremals(error: &[f64], r: usize) -> Vec<usize> {
    let n = error.len();
    if n <= r {
        return (0..n).collect();
    }

    // Collect all local extrema (peaks and valleys), including endpoints
    let mut peaks: Vec<(usize, f64)> = Vec::new();

    // Left endpoint
    if n > 1 && error[0].abs() >= error[1].abs() {
        peaks.push((0, error[0]));
    }

    // Interior: local extrema where |E| is locally maximal with consistent sign
    for i in 1..n - 1 {
        let is_local_max = error[i] > 0.0 && error[i] >= error[i - 1] && error[i] >= error[i + 1];
        let is_local_min = error[i] < 0.0 && error[i] <= error[i - 1] && error[i] <= error[i + 1];
        if is_local_max || is_local_min {
            peaks.push((i, error[i]));
        }
    }

    // Right endpoint
    if n > 1 && error[n - 1].abs() >= error[n - 2].abs() {
        peaks.push((n - 1, error[n - 1]));
    }

    if peaks.is_empty() {
        return (0..r).map(|i| i * (n - 1) / (r - 1)).collect();
    }

    // Build alternating chain: scan left to right, keeping extrema
    // that alternate in sign with maximal magnitude
    let mut chain: Vec<(usize, f64)> = Vec::new();
    for &(idx, val) in &peaks {
        if chain.is_empty() {
            chain.push((idx, val));
        } else {
            let last_val = chain.last().unwrap().1;
            if val * last_val < 0.0 {
                // Opposite sign: extend the alternating chain
                chain.push((idx, val));
            } else if val.abs() > last_val.abs() {
                // Same sign but larger: replace (keep the stronger extremum)
                *chain.last_mut().unwrap() = (idx, val);
            }
        }
    }

    // Trim to exactly r by removing the end with smaller |E|
    while chain.len() > r {
        let first_e = chain[0].1.abs();
        let last_e = chain.last().unwrap().1.abs();
        if first_e <= last_e {
            chain.remove(0);
        } else {
            chain.pop();
        }
    }

    // If under-count, fill gaps with uniformly spaced grid points
    let mut result: Vec<usize> = chain.iter().map(|&(i, _)| i).collect();
    while result.len() < r {
        // Insert midpoint at the largest gap
        let mut max_gap = 0;
        let mut gap_pos = 0;
        for i in 0..result.len().saturating_sub(1) {
            let gap = result[i + 1] - result[i];
            if gap > max_gap {
                max_gap = gap;
                gap_pos = i;
            }
        }
        if max_gap > 1 {
            let mid = (result[gap_pos] + result[gap_pos + 1]) / 2;
            result.insert(gap_pos + 1, mid);
        } else {
            // Can't split; extend at boundaries
            let last = *result.last().unwrap();
            if last + 1 < n {
                result.push(last + 1);
            } else if result[0] > 0 {
                result.insert(0, result[0] - 1);
            } else {
                break;
            }
        }
    }

    result
}

/// Extract Type I FIR coefficients from the converged Remez approximation.
///
/// Evaluates A(ω) at M+1 uniform points via barycentric interpolation,
/// then applies the DCT-I inverse to recover cosine coefficients a\_k.
/// Finally maps a\_k to the symmetric impulse response h\[n\].
///
/// DCT-I inverse:
///   a\[k\] = (ε\_k / M) Σ\_{j=0}^{M} c\_j · A(ω\_j) · cos(πjk/M)
/// where c\_j = 1/2 for j ∈ {0, M}, else 1;  ε\_k = 1 for k ∈ {0, M}, else 2.
///
/// Impulse response mapping:
///   h\[M\] = a\[0\],  h\[M±k\] = a\[k\]/2  for k ≥ 1.
fn extract_type1_coefficients(
    ext_idx: &[usize],
    grid: &[GridPoint],
    grid_x: &[f64],
    delta: f64,
    m: usize,
    num_taps: usize,
) -> Result<Vec<f64>, RemezError> {
    // Recompute interpolation from final extremals
    let ext_x: Vec<f64> = ext_idx.iter().map(|&i| grid_x[i]).collect();
    let bary = barycentric_weights(&ext_x);
    let ext_y: Vec<f64> = ext_idx
        .iter()
        .enumerate()
        .map(|(j, &idx)| {
            let sign = if j % 2 == 0 { 1.0 } else { -1.0 };
            grid[idx].desired - sign * delta / grid[idx].weight
        })
        .collect();

    // Sample A(ω) at M+1 uniform points: ω_k = πk/M for k = 0,...,M
    let mut a_samples = vec![0.0; m + 1];
    if m == 0 {
        // Degenerate: 1-tap filter, just the DC value
        a_samples[0] = barycentric_eval(&ext_x, &ext_y, &bary, 1.0); // cos(0) = 1
        let mut h = vec![0.0; num_taps];
        h[0] = a_samples[0];
        return Ok(h);
    }

    for (k, a_sample) in a_samples.iter_mut().enumerate().take(m + 1) {
        let omega = PI * k as f64 / m as f64;
        let x = omega.cos();
        *a_sample = barycentric_eval(&ext_x, &ext_y, &bary, x);
    }

    // DCT-I inverse to recover cosine coefficients a[0..M]
    let mut cos_coeffs = vec![0.0; m + 1];
    for (k, coeff) in cos_coeffs.iter_mut().enumerate().take(m + 1) {
        let mut sum = 0.0;
        for (j, &a_val) in a_samples.iter().enumerate().take(m + 1) {
            let c_j = if j == 0 || j == m { 0.5 } else { 1.0 };
            sum += c_j * a_val * (PI * j as f64 * k as f64 / m as f64).cos();
        }
        let e_k = if k == 0 || k == m { 1.0 } else { 2.0 };
        *coeff = e_k * sum / m as f64;
    }

    // Convert cosine coefficients to symmetric impulse response
    // A(ω) = a[0] + Σ_{k=1}^M a[k]cos(kω)  →  h[M] = a[0], h[M±k] = a[k]/2
    let mut h = vec![0.0; num_taps];
    h[m] = cos_coeffs[0];
    for k in 1..=m {
        h[m - k] = cos_coeffs[k] / 2.0;
        h[m + k] = cos_coeffs[k] / 2.0;
    }

    Ok(h)
}

// ============================================================================
// FIR Filter Runtime
// ============================================================================

/// Stateful FIR filter with circular buffer for real-time IQ processing.
///
/// Implements direct-form convolution: y\[n\] = Σ\_{k=0}^{L-1} h\[k\]·x\[n−k\]
///
/// Design functions return f64 for precision; the runtime converts to f32.
pub struct FirFilter {
    coeffs: Vec<f32>,
    buffer: Vec<Sample>,
    pos: usize,
}

impl FirFilter {
    /// Create from f64 design coefficients (converted to f32 internally).
    pub fn new(coeffs: &[f64]) -> Self {
        let coeffs_f32: Vec<f32> = coeffs.iter().map(|&c| c as f32).collect();
        let len = coeffs_f32.len();
        Self {
            coeffs: coeffs_f32,
            buffer: vec![Sample::new(0.0, 0.0); len],
            pos: 0,
        }
    }

    /// Create from pre-converted f32 coefficients.
    pub fn from_f32(coeffs: Vec<f32>) -> Self {
        let len = coeffs.len();
        Self {
            coeffs,
            buffer: vec![Sample::new(0.0, 0.0); len],
            pos: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.coeffs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.coeffs.is_empty()
    }

    /// Feed one sample into the delay line without computing the output.
    /// Use with `compute_output()` for efficient decimation (polyphase style):
    /// push M-1 samples, then call `process_sample` for the M-th.
    pub fn push_sample(&mut self, input: Sample) {
        self.buffer[self.pos] = input;
        self.pos = (self.pos + 1) % self.coeffs.len();
    }

    /// Filter one IQ sample. O(L) per sample.
    pub fn process_sample(&mut self, input: Sample) -> Sample {
        let len = self.coeffs.len();
        self.buffer[self.pos] = input;

        let mut output = Sample::new(0.0, 0.0);
        for k in 0..len {
            let idx = (self.pos + len - k) % len;
            output += self.buffer[idx] * self.coeffs[k];
        }

        self.pos = (self.pos + 1) % len;
        output
    }

    /// Filter a block of IQ samples.
    pub fn process_block(&mut self, input: &[Sample]) -> Vec<Sample> {
        input.iter().map(|&s| self.process_sample(s)).collect()
    }

    /// Compute the frequency response at `num_points` uniformly spaced
    /// frequencies from 0 to Nyquist.
    ///
    /// Returns (frequency\_normalized, magnitude\_dB, phase\_radians).
    ///
    /// Evaluates H(e^{jω}) = Σ\_{n=0}^{L-1} h\[n\]·e^{−jωn} directly.
    pub fn frequency_response(&self, num_points: usize) -> Vec<(f64, f64, f64)> {
        (0..num_points)
            .map(|k| {
                let freq = k as f64 / num_points as f64;
                let omega = PI * freq;

                let mut h_re = 0.0f64;
                let mut h_im = 0.0f64;
                for (n, &coeff) in self.coeffs.iter().enumerate() {
                    let phase = omega * n as f64;
                    h_re += coeff as f64 * phase.cos();
                    h_im -= coeff as f64 * phase.sin();
                }

                let magnitude = (h_re * h_re + h_im * h_im).sqrt();
                let magnitude_db = if magnitude > 1e-20 {
                    20.0 * magnitude.log10()
                } else {
                    -400.0
                };
                let phase = h_im.atan2(h_re);
                (freq, magnitude_db, phase)
            })
            .collect()
    }

    /// Group delay in samples. For Type I linear phase: (L−1)/2.
    pub fn group_delay(&self) -> f64 {
        (self.coeffs.len() - 1) as f64 / 2.0
    }

    /// Reset internal state to zero.
    pub fn reset(&mut self) {
        self.buffer.fill(Sample::new(0.0, 0.0));
        self.pos = 0;
    }

    /// Access the filter coefficients.
    pub fn coefficients(&self) -> &[f32] {
        &self.coeffs
    }
}

/// Stateful FIR filter for real-valued (f32) signals.
///
/// Used in the demodulation chain for audio filtering (de-emphasis,
/// anti-alias, CIC compensation, etc.).
pub struct RealFirFilter {
    coeffs: Vec<f32>,
    buffer: Vec<f32>,
    pos: usize,
}

impl RealFirFilter {
    pub fn new(coeffs: &[f64]) -> Self {
        let coeffs_f32: Vec<f32> = coeffs.iter().map(|&c| c as f32).collect();
        let len = coeffs_f32.len();
        Self {
            coeffs: coeffs_f32,
            buffer: vec![0.0; len],
            pos: 0,
        }
    }

    pub fn from_f32(coeffs: Vec<f32>) -> Self {
        let len = coeffs.len();
        Self {
            coeffs,
            buffer: vec![0.0; len],
            pos: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.coeffs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.coeffs.is_empty()
    }

    pub fn process_sample(&mut self, input: f32) -> f32 {
        let len = self.coeffs.len();
        self.buffer[self.pos] = input;

        let mut output = 0.0f32;
        for k in 0..len {
            let idx = (self.pos + len - k) % len;
            output += self.buffer[idx] * self.coeffs[k];
        }

        self.pos = (self.pos + 1) % len;
        output
    }

    pub fn process_block(&mut self, input: &[f32]) -> Vec<f32> {
        input.iter().map(|&s| self.process_sample(s)).collect()
    }

    pub fn process_in_place(&mut self, samples: &mut [f32]) {
        for s in samples.iter_mut() {
            *s = self.process_sample(*s);
        }
    }

    pub fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.pos = 0;
    }

    pub fn coefficients(&self) -> &[f32] {
        &self.coeffs
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firwin_lowpass_symmetry_and_gain() {
        let h = firwin_lowpass(0.5, 31, &WindowType::Hann);
        assert_eq!(h.len(), 31);

        // Type I symmetry: h[n] = h[N-1-n]
        for i in 0..15 {
            assert!(
                (h[i] - h[30 - i]).abs() < 1e-12,
                "Not symmetric at {i}: {} vs {}",
                h[i],
                h[30 - i]
            );
        }

        // DC gain = Σh[n] = H(0) ≈ 1.0 (lowpass passes DC regardless of cutoff)
        let dc_gain: f64 = h.iter().sum();
        assert!(
            (dc_gain - 1.0).abs() < 0.05,
            "DC gain {dc_gain} not near 1.0"
        );
    }

    #[test]
    fn firwin_lowpass_frequency_response() {
        let h = firwin_lowpass(0.4, 101, &WindowType::BlackmanHarris4);
        let filt = FirFilter::new(&h);
        let resp = filt.frequency_response(512);

        // Passband (f < 0.30): near 0 dB
        for &(f, mag_db, _) in &resp {
            if f < 0.30 && f > 0.01 {
                assert!(mag_db > -1.0, "Passband ripple at f={f}: {mag_db} dB");
            }
        }

        // Stopband (f > 0.55): well attenuated
        for &(f, mag_db, _) in &resp {
            if f > 0.55 {
                assert!(mag_db < -40.0, "Stopband rejection at f={f}: {mag_db} dB");
            }
        }
    }

    #[test]
    fn firwin_highpass_spectral_inversion_identity() {
        let h_lp = firwin_lowpass(0.3, 51, &WindowType::Hamming);
        let h_hp = firwin_highpass(0.3, 51, &WindowType::Hamming);

        // h_lp + h_hp = δ[n - M]
        let m = 25;
        for i in 0..51 {
            let sum = h_lp[i] + h_hp[i];
            let expected = if i == m { 1.0 } else { 0.0 };
            assert!(
                (sum - expected).abs() < 1e-12,
                "Spectral inversion identity violated at {i}: {sum} vs {expected}"
            );
        }
    }

    #[test]
    fn firwin_bandpass_shape() {
        let h = firwin_bandpass(0.3, 0.5, 101, &WindowType::BlackmanHarris4);
        let filt = FirFilter::new(&h);
        let resp = filt.frequency_response(512);

        // Below band: attenuated
        for &(f, mag_db, _) in &resp {
            if f < 0.2 {
                assert!(mag_db < -30.0, "Low stopband at f={f}: {mag_db} dB");
            }
        }

        // In-band: passes
        for &(f, mag_db, _) in &resp {
            if f > 0.35 && f < 0.45 {
                assert!(mag_db > -3.0, "Passband loss at f={f}: {mag_db} dB");
            }
        }
    }

    #[test]
    fn firwin_bandstop_identity() {
        let h_bp = firwin_bandpass(0.3, 0.5, 51, &WindowType::Hann);
        let h_bs = firwin_bandstop(0.3, 0.5, 51, &WindowType::Hann);
        let m = 25;

        for i in 0..51 {
            let sum = h_bp[i] + h_bs[i];
            let expected = if i == m { 1.0 } else { 0.0 };
            assert!(
                (sum - expected).abs() < 1e-12,
                "Bandstop identity violated at {i}"
            );
        }
    }

    #[test]
    fn kaiser_lowpass_meets_spec() {
        let h = kaiser_lowpass(0.3, 0.1, 60.0);
        let filt = FirFilter::new(&h);
        let resp = filt.frequency_response(1024);

        for &(f, mag_db, _) in &resp {
            if f > 0.4 {
                assert!(
                    mag_db < -50.0,
                    "Kaiser stopband at f={f}: {mag_db} dB (want < -50)"
                );
            }
        }
    }

    #[test]
    fn remez_lowpass_equiripple() {
        let bands = vec![
            RemezBand {
                start: 0.0,
                end: 0.3,
                desired: 1.0,
                weight: 1.0,
            },
            RemezBand {
                start: 0.5,
                end: 1.0,
                desired: 0.0,
                weight: 1.0,
            },
        ];

        let h = remez_fir(31, &bands, 50).expect("Remez should converge");
        assert_eq!(h.len(), 31);

        // Symmetry
        for i in 0..15 {
            assert!(
                (h[i] - h[30 - i]).abs() < 1e-8,
                "Not symmetric at {i}: {} vs {}",
                h[i],
                h[30 - i]
            );
        }

        let filt = FirFilter::new(&h);
        let resp = filt.frequency_response(512);

        // Passband: linear deviation from 1.0
        let max_pass_err: f64 = resp
            .iter()
            .filter(|(f, _, _)| *f <= 0.3)
            .map(|(_, db, _)| (10.0f64.powf(*db / 20.0) - 1.0).abs())
            .fold(0.0f64, f64::max);

        // Stopband: linear deviation from 0.0
        let max_stop: f64 = resp
            .iter()
            .filter(|(f, _, _)| *f >= 0.5)
            .map(|(_, db, _)| 10.0f64.powf(*db / 20.0))
            .fold(0.0f64, f64::max);

        assert!(
            max_pass_err < 0.15,
            "Passband ripple too large: {max_pass_err}"
        );
        assert!(max_stop < 0.15, "Stopband rejection too low: {max_stop}");
    }

    #[test]
    fn remez_bandpass() {
        let bands = vec![
            RemezBand {
                start: 0.0,
                end: 0.2,
                desired: 0.0,
                weight: 1.0,
            },
            RemezBand {
                start: 0.3,
                end: 0.5,
                desired: 1.0,
                weight: 1.0,
            },
            RemezBand {
                start: 0.6,
                end: 1.0,
                desired: 0.0,
                weight: 1.0,
            },
        ];

        let h = remez_fir(51, &bands, 50).expect("Remez bandpass should converge");
        let filt = FirFilter::new(&h);
        let resp = filt.frequency_response(512);

        // Passband center near 0 dB
        let center_db = resp
            .iter()
            .filter(|(f, _, _)| (*f - 0.4).abs() < 0.02)
            .map(|(_, db, _)| *db)
            .fold(f64::MIN, f64::max);
        assert!(center_db > -3.0, "Passband center: {center_db} dB");
    }

    #[test]
    fn remez_weighted_bands() {
        let bands_equal = vec![
            RemezBand {
                start: 0.0,
                end: 0.3,
                desired: 1.0,
                weight: 1.0,
            },
            RemezBand {
                start: 0.5,
                end: 1.0,
                desired: 0.0,
                weight: 1.0,
            },
        ];
        let bands_weighted = vec![
            RemezBand {
                start: 0.0,
                end: 0.3,
                desired: 1.0,
                weight: 1.0,
            },
            RemezBand {
                start: 0.5,
                end: 1.0,
                desired: 0.0,
                weight: 10.0,
            },
        ];

        let h_eq = remez_fir(31, &bands_equal, 50).unwrap();
        let h_wt = remez_fir(31, &bands_weighted, 50).unwrap();

        let filt_eq = FirFilter::new(&h_eq);
        let filt_wt = FirFilter::new(&h_wt);

        // Max stopband level
        let stop_eq: f64 = filt_eq
            .frequency_response(512)
            .iter()
            .filter(|(f, _, _)| *f >= 0.5)
            .map(|(_, db, _)| *db)
            .fold(f64::MIN, f64::max);

        let stop_wt: f64 = filt_wt
            .frequency_response(512)
            .iter()
            .filter(|(f, _, _)| *f >= 0.5)
            .map(|(_, db, _)| *db)
            .fold(f64::MIN, f64::max);

        assert!(
            stop_wt < stop_eq,
            "Weighted ({stop_wt:.1} dB) should beat equal ({stop_eq:.1} dB)"
        );
    }

    #[test]
    fn remez_invalid_bands() {
        // Overlapping bands
        let bands = vec![
            RemezBand {
                start: 0.0,
                end: 0.5,
                desired: 1.0,
                weight: 1.0,
            },
            RemezBand {
                start: 0.3,
                end: 1.0,
                desired: 0.0,
                weight: 1.0,
            },
        ];
        assert!(remez_fir(31, &bands, 50).is_err());

        // Empty bands
        assert!(remez_fir(31, &[], 50).is_err());
    }

    #[test]
    fn fir_filter_impulse_response() {
        let coeffs = vec![0.25, 0.5, 0.25];
        let mut filt = FirFilter::new(&coeffs);

        let y0 = filt.process_sample(Sample::new(1.0, 0.0));
        let y1 = filt.process_sample(Sample::new(0.0, 0.0));
        let y2 = filt.process_sample(Sample::new(0.0, 0.0));

        assert!((y0.re - 0.25).abs() < 1e-5, "h[0] = {}", y0.re);
        assert!((y1.re - 0.5).abs() < 1e-5, "h[1] = {}", y1.re);
        assert!((y2.re - 0.25).abs() < 1e-5, "h[2] = {}", y2.re);
    }

    #[test]
    fn fir_filter_block_matches_sample() {
        let h = firwin_lowpass(0.3, 21, &WindowType::Hann);
        let mut filt1 = FirFilter::new(&h);
        let mut filt2 = FirFilter::new(&h);

        let input: Vec<Sample> = (0..100)
            .map(|i| Sample::new((i as f32 * 0.3).sin(), (i as f32 * 0.7).cos()))
            .collect();

        let out1: Vec<Sample> = input.iter().map(|&s| filt1.process_sample(s)).collect();
        let out2 = filt2.process_block(&input);

        for (a, b) in out1.iter().zip(out2.iter()) {
            assert!((a.re - b.re).abs() < 1e-6);
            assert!((a.im - b.im).abs() < 1e-6);
        }
    }

    #[test]
    fn fir_filter_group_delay() {
        let h = firwin_lowpass(0.3, 51, &WindowType::Hann);
        let filt = FirFilter::new(&h);
        assert!((filt.group_delay() - 25.0).abs() < 1e-10);
    }

    #[test]
    fn fir_filter_frequency_response_shape() {
        let h = firwin_lowpass(0.25, 51, &WindowType::BlackmanHarris4);
        let filt = FirFilter::new(&h);
        let resp = filt.frequency_response(256);

        let dc_db = resp[0].1;
        let nyquist_db = resp[255].1;
        assert!(
            dc_db > nyquist_db + 20.0,
            "DC ({dc_db:.1} dB) should be much higher than Nyquist ({nyquist_db:.1} dB)"
        );
    }

    #[test]
    fn real_fir_filter_basic() {
        let h = firwin_lowpass(0.3, 21, &WindowType::Hann);
        let mut filt = RealFirFilter::new(&h);

        // Feed a low-frequency sine (should pass)
        let lo: Vec<f32> = (0..200).map(|i| (0.1 * i as f32).sin()).collect();
        let out_lo = filt.process_block(&lo);

        filt.reset();

        // Feed a high-frequency sine (should be attenuated)
        let hi: Vec<f32> = (0..200).map(|i| (2.5 * i as f32).sin()).collect();
        let out_hi = filt.process_block(&hi);

        // Compare steady-state power (skip transient)
        let pow_lo: f32 = out_lo[100..].iter().map(|x| x * x).sum::<f32>() / 100.0;
        let pow_hi: f32 = out_hi[100..].iter().map(|x| x * x).sum::<f32>() / 100.0;

        assert!(
            pow_lo > pow_hi * 10.0,
            "Lowpass should pass low freq ({pow_lo:.4}) and reject high ({pow_hi:.4})"
        );
    }
}
