use std::f64::consts::PI;

/// Modified Bessel function of the first kind, order zero.
///
/// Uses the series expansion: I₀(x) = Σ_{k=0}^{∞} [(x/2)^k / k!]²
/// Converges rapidly for typical Kaiser window β values (0-30).
pub fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half_x = x / 2.0;

    for k in 1..50 {
        term *= (half_x / k as f64) * (half_x / k as f64);
        sum += term;
        if term < 1e-20 * sum {
            break;
        }
    }

    sum
}

/// Chebyshev polynomial of the first kind, T_n(x).
///
/// Uses the recurrence: T₀=1, T₁=x, T_{n+1} = 2x·Tₙ - T_{n-1}
/// For |x| > 1, uses the identity T_n(x) = cosh(n·acosh(x)).
pub fn chebyshev_poly(n: usize, x: f64) -> f64 {
    if x.abs() <= 1.0 {
        (n as f64 * x.acos()).cos()
    } else if x > 1.0 {
        (n as f64 * x.acosh()).cosh()
    } else {
        let sign = if n % 2 == 0 { 1.0 } else { -1.0 };
        sign * (n as f64 * (-x).acosh()).cosh()
    }
}

/// Window function type with all parameters.
#[derive(Debug, Clone)]
pub enum WindowType {
    /// Rectangular (no window).
    Rectangular,
    /// Hann (raised cosine). Zero at endpoints.
    Hann,
    /// Hamming. Non-zero at endpoints, lower first sidelobe than Hann.
    Hamming,
    /// Exact Hamming. Coefficients chosen to null first sidelobe exactly.
    ExactHamming,
    /// Blackman. Three-term cosine sum.
    Blackman,
    /// Blackman-Harris 4-term. -92 dB sidelobes.
    BlackmanHarris4,
    /// Blackman-Harris 7-term. -180 dB sidelobes.
    BlackmanHarris7,
    /// Nuttall 4-term. Continuous first derivative at endpoints.
    Nuttall,
    /// Flat-top. Minimal scalloping loss (<0.01 dB) for amplitude measurement.
    FlatTop,
    /// Kaiser. Parameterized by β which controls mainlobe width vs sidelobe level.
    /// β ≈ 0 → rectangular, β ≈ 5 → similar to Hamming, β ≈ 8.6 → similar to Blackman-Harris.
    /// Sidelobe attenuation ≈ -20(β+1) dB (approximate).
    Kaiser { beta: f64 },
    /// Gaussian. Parameterized by σ (standard deviation in samples, fraction of N/2).
    /// σ ≤ 0.5 is typical. Smaller σ → wider mainlobe, lower sidelobes.
    Gaussian { sigma: f64 },
    /// Tukey (cosine-tapered). α=0 → rectangular, α=1 → Hann.
    /// The fraction α of the window that is cosine-tapered.
    Tukey { alpha: f64 },
    /// Dolph-Chebyshev. Equiripple sidelobes at specified attenuation (dB).
    /// Optimal in the minimax sense: minimizes mainlobe width for a given sidelobe level.
    DolphChebyshev { attenuation_db: f64 },
    /// Planck-taper. Uses the smooth Planck function for infinitely differentiable tapering.
    /// ε controls the taper fraction (0 < ε < 0.5).
    PlanckTaper { epsilon: f64 },
}

/// Generate a window function of length N.
pub fn generate_window(window_type: &WindowType, n: usize) -> Vec<f64> {
    match window_type {
        WindowType::Rectangular => vec![1.0; n],
        WindowType::Hann => cosine_sum_window(n, &[0.5, -0.5]),
        WindowType::Hamming => cosine_sum_window(n, &[0.54, -0.46]),
        WindowType::ExactHamming => cosine_sum_window(n, &[25.0 / 46.0, -21.0 / 46.0]),
        WindowType::Blackman => cosine_sum_window(n, &[0.42, -0.5, 0.08]),
        WindowType::BlackmanHarris4 => {
            cosine_sum_window(n, &[0.35875, -0.48829, 0.14128, -0.01168])
        }
        WindowType::BlackmanHarris7 => cosine_sum_window(
            n,
            &[
                0.27105140069342,
                -0.43329793923448,
                0.21812299954311,
                -0.06592544638803,
                0.01081174209837,
                -0.00077658482522,
                0.00001388721735,
            ],
        ),
        WindowType::Nuttall => cosine_sum_window(n, &[0.355768, -0.487396, 0.144232, -0.012604]),
        WindowType::FlatTop => {
            // Maximal amplitude flatness (ISO 18431-2)
            cosine_sum_window(
                n,
                &[
                    0.21557895,
                    -0.41663158,
                    0.277263158,
                    -0.083578947,
                    0.006947368,
                ],
            )
        }
        WindowType::Kaiser { beta } => kaiser_window(n, *beta),
        WindowType::Gaussian { sigma } => gaussian_window(n, *sigma),
        WindowType::Tukey { alpha } => tukey_window(n, *alpha),
        WindowType::DolphChebyshev { attenuation_db } => dolph_chebyshev_window(n, *attenuation_db),
        WindowType::PlanckTaper { epsilon } => planck_taper_window(n, *epsilon),
    }
}

/// Generate a generalized cosine sum window: w[n] = Σ aₖ·cos(2πkn/(N-1))
fn cosine_sum_window(n: usize, coeffs: &[f64]) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![coeffs.iter().sum::<f64>()];
    }

    let nm1 = (n - 1) as f64;
    (0..n)
        .map(|i| {
            coeffs
                .iter()
                .enumerate()
                .map(|(k, &a)| a * (2.0 * PI * k as f64 * i as f64 / nm1).cos())
                .sum()
        })
        .collect()
}

/// Kaiser window using modified Bessel function I₀.
///
/// w[n] = I₀(β√(1 - ((2n/(N-1)) - 1)²)) / I₀(β)
///
/// The Kaiser window is near-optimal in the sense of maximizing the energy
/// concentration in the mainlobe for a given mainlobe width. It approximates
/// the DPSS (Slepian) window for a single taper.
fn kaiser_window(n: usize, beta: f64) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![1.0];
    }

    let i0_beta = bessel_i0(beta);
    let nm1 = (n - 1) as f64;

    (0..n)
        .map(|i| {
            let t = 2.0 * i as f64 / nm1 - 1.0;
            let arg = beta * (1.0 - t * t).max(0.0).sqrt();
            bessel_i0(arg) / i0_beta
        })
        .collect()
}

/// Gaussian window.
///
/// w[n] = exp(-0.5 · ((n - (N-1)/2) / (σ·(N-1)/2))²)
fn gaussian_window(n: usize, sigma: f64) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }

    let center = (n - 1) as f64 / 2.0;
    let denom = sigma * center;

    (0..n)
        .map(|i| {
            let t = (i as f64 - center) / denom;
            (-0.5 * t * t).exp()
        })
        .collect()
}

/// Tukey (cosine-tapered) window.
///
/// α=0 → rectangular, α=1 → Hann.
/// The first and last α/2 fraction of the window is cosine-tapered.
fn tukey_window(n: usize, alpha: f64) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let alpha = alpha.clamp(0.0, 1.0);
    if alpha < 1e-10 {
        return vec![1.0; n];
    }

    let nm1 = (n - 1) as f64;
    let boundary = alpha * nm1 / 2.0;

    (0..n)
        .map(|i| {
            let x = i as f64;
            if x < boundary {
                0.5 * (1.0 - (PI * x / boundary).cos())
            } else if x > nm1 - boundary {
                0.5 * (1.0 - (PI * (nm1 - x) / boundary).cos())
            } else {
                1.0
            }
        })
        .collect()
}

/// Dolph-Chebyshev window.
///
/// Produces equiripple sidelobes at the specified attenuation level.
/// This is the optimal window in the Chebyshev (minimax) sense:
/// for a given sidelobe level, it achieves the narrowest mainlobe.
///
/// Uses the frequency-domain definition via inverse DFT of the Chebyshev
/// polynomial evaluated at mapped frequencies, then normalizes.
fn dolph_chebyshev_window(n: usize, attenuation_db: f64) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![1.0];
    }

    let nn = n as f64;
    let atten_linear = 10.0f64.powf(attenuation_db.abs() / 20.0);

    // β = cosh(acosh(10^(A/20)) / (N-1))
    let beta = (atten_linear.acosh() / (nn - 1.0)).cosh();

    // Compute window via inverse DFT of Chebyshev polynomial
    // W[k] = T_{N-1}(beta * cos(pi*k/N)), then w[n] = IDFT{W[k]}
    let order = n - 1;
    let mut w = vec![0.0; n];

    #[allow(clippy::needless_range_loop)]
    for i in 0..n {
        let mut sum = 0.0;
        for k in 0..n {
            let freq = PI * k as f64 / nn;
            let x = beta * freq.cos();
            let cheb = chebyshev_poly(order, x);
            let phase = 2.0 * PI * i as f64 * k as f64 / nn;
            sum += cheb * phase.cos();
        }
        w[i] = sum / nn;
    }

    // Normalize to peak = 1.0
    let max_val = w.iter().cloned().fold(0.0f64, f64::max);
    if max_val > 0.0 {
        for v in &mut w {
            *v /= max_val;
        }
    }

    w
}

/// Planck-taper window.
///
/// Infinitely differentiable (C∞) at the taper boundaries, making it ideal
/// for applications requiring minimal spectral leakage into high-frequency bins.
///
/// Uses the Planck function: Z(t) = 1 / (1 + exp(ε·(1/t + 1/(t-ε))))
fn planck_taper_window(n: usize, epsilon: f64) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let epsilon = epsilon.clamp(0.0, 0.5);
    if epsilon < 1e-10 {
        return vec![1.0; n];
    }

    let nm1 = (n - 1) as f64;

    (0..n)
        .map(|i| {
            let t = i as f64 / nm1;
            if t <= 0.0 || t >= 1.0 {
                0.0
            } else if t < epsilon {
                let z = epsilon * (1.0 / t + 1.0 / (t - epsilon));
                1.0 / (1.0 + z.exp())
            } else if t > 1.0 - epsilon {
                let t_flip = 1.0 - t;
                let z = epsilon * (1.0 / t_flip + 1.0 / (t_flip - epsilon));
                1.0 / (1.0 + z.exp())
            } else {
                1.0
            }
        })
        .collect()
}

/// Compute window metrics for analysis.
#[derive(Debug, Clone)]
pub struct WindowMetrics {
    /// Coherent gain: sum(w) / N. Ratio of windowed to unwindowed DC response.
    pub coherent_gain: f64,
    /// Equivalent noise bandwidth in bins.
    /// ENBW = N · Σ(w²) / (Σw)². Rectangular = 1.0, Hann = 1.5, BH4 = 2.0.
    pub enbw_bins: f64,
    /// Processing gain = 10·log10(N/ENBW) in dB. How much SNR the window preserves.
    pub processing_gain_db: f64,
    /// Scalloping loss in dB. Worst-case amplitude error when a tone falls
    /// between two bins. Flat-top ≈ 0.01 dB, Hann ≈ 1.42 dB.
    pub scalloping_loss_db: f64,
    /// Window energy: Σ(w²).
    pub energy: f64,
}

/// Compute metrics for a given window.
pub fn window_metrics(window: &[f64]) -> WindowMetrics {
    let n = window.len() as f64;
    if window.is_empty() {
        return WindowMetrics {
            coherent_gain: 0.0,
            enbw_bins: 0.0,
            processing_gain_db: 0.0,
            scalloping_loss_db: 0.0,
            energy: 0.0,
        };
    }

    let sum: f64 = window.iter().sum();
    let sum_sq: f64 = window.iter().map(|w| w * w).sum();
    let coherent_gain = sum / n;
    let enbw_bins = n * sum_sq / (sum * sum);
    let processing_gain_db = 10.0 * (n / (n * sum_sq / (sum * sum))).log10();

    // Scalloping loss: evaluate the DTFT at half-bin offset
    // W(0.5/N) / W(0) where W is the window's Fourier transform
    let half_bin_freq = PI / n;
    let mut w_half_re = 0.0;
    let mut w_half_im = 0.0;
    for (i, &w) in window.iter().enumerate() {
        let phase = half_bin_freq * i as f64;
        w_half_re += w * phase.cos();
        w_half_im += w * phase.sin();
    }
    let w_half_mag = (w_half_re * w_half_re + w_half_im * w_half_im).sqrt();
    let scalloping_loss_db = if sum > 0.0 {
        -20.0 * (w_half_mag / sum).log10()
    } else {
        0.0
    };

    WindowMetrics {
        coherent_gain,
        enbw_bins,
        processing_gain_db,
        scalloping_loss_db,
        energy: sum_sq,
    }
}

/// Design a Kaiser window with specific parameters.
///
/// Given desired stopband attenuation (dB) and transition width (normalized, 0-1),
/// computes the required β and window length N.
///
/// Based on Kaiser's empirical formulas:
/// - A > 50: β = 0.1102(A - 8.7)
/// - 21 ≤ A ≤ 50: β = 0.5842(A - 21)^0.4 + 0.07886(A - 21)
/// - A < 21: β = 0
///
/// N = (A - 7.95) / (2.285 · Δω) where Δω is transition width in radians.
pub fn kaiser_design(attenuation_db: f64, transition_width: f64) -> (f64, usize) {
    let a = attenuation_db.abs();

    let beta = if a > 50.0 {
        0.1102 * (a - 8.7)
    } else if a >= 21.0 {
        0.5842 * (a - 21.0).powf(0.4) + 0.07886 * (a - 21.0)
    } else {
        0.0
    };

    let delta_omega = 2.0 * PI * transition_width;
    let n = ((a - 7.95) / (2.285 * delta_omega)).ceil() as usize;
    let n = n.max(1) | 1; // Ensure odd for symmetry

    (beta, n)
}

/// Generate DPSS (Discrete Prolate Spheroidal Sequence) / Slepian sequences.
///
/// These are the eigenvectors of the N×N matrix with entries
/// sin(2πW(m-n)) / (π(m-n)), where W is the half-bandwidth parameter.
///
/// DPSS windows maximize energy concentration in the frequency band [-W, W].
/// The first K sequences (K ≤ 2NW) are used in multitaper spectral estimation.
///
/// Uses the tridiagonal matrix formulation:
///   d[n] = ((N-1-2n)/2)² · cos(2πW)   (diagonal)
///   e[n] = n(N-n)/2                     (off-diagonal)
///
/// Then finds eigenvectors via inverse iteration.
pub fn dpss(n: usize, half_bandwidth: f64, num_tapers: usize) -> Vec<Vec<f64>> {
    if n == 0 || num_tapers == 0 {
        return Vec::new();
    }

    let nw = half_bandwidth;

    // Build the tridiagonal matrix
    let mut diag = vec![0.0; n];
    let mut off_diag = vec![0.0; n.saturating_sub(1)];

    let cos_2pw = (2.0 * PI * nw / n as f64).cos();
    for (i, d) in diag.iter_mut().enumerate() {
        let t = (n as f64 - 1.0 - 2.0 * i as f64) / 2.0;
        *d = t * t * cos_2pw;
    }
    for (i, od) in off_diag.iter_mut().enumerate() {
        let k = (i + 1) as f64;
        *od = k * (n as f64 - k) / 2.0;
    }

    // Find eigenvalues using bisection on the tridiagonal matrix
    // Then find eigenvectors using inverse iteration
    let eigenvalues = tridiag_eigenvalues(&diag, &off_diag, num_tapers);

    let mut tapers = Vec::with_capacity(num_tapers);
    for &eigenval in &eigenvalues {
        let eigvec = tridiag_inverse_iteration(&diag, &off_diag, eigenval, n);
        tapers.push(eigvec);
    }

    // Enforce sign convention: first taper is positive at center,
    // second is positive-then-negative, etc.
    for (k, taper) in tapers.iter_mut().enumerate() {
        let center = n / 2;
        let sign_check = if k % 2 == 0 {
            taper[center]
        } else {
            // For odd tapers, check the first quarter
            taper[n / 4]
        };
        if sign_check < 0.0 {
            for v in taper.iter_mut() {
                *v = -*v;
            }
        }
    }

    tapers
}

/// Find the K largest eigenvalues of a symmetric tridiagonal matrix
/// using the Sturm sequence bisection method.
fn tridiag_eigenvalues(diag: &[f64], off_diag: &[f64], k: usize) -> Vec<f64> {
    let n = diag.len();
    let k = k.min(n);

    // Gershgorin bounds for eigenvalue range
    let mut lo = f64::MAX;
    let mut hi = f64::MIN;
    for i in 0..n {
        let radius = if i > 0 { off_diag[i - 1].abs() } else { 0.0 }
            + if i < n - 1 { off_diag[i].abs() } else { 0.0 };
        lo = lo.min(diag[i] - radius);
        hi = hi.max(diag[i] + radius);
    }
    lo -= 1.0;
    hi += 1.0;

    // Count eigenvalues less than x using Sturm sequence
    let count_less = |x: f64| -> usize {
        let mut count = 0;
        let mut d = diag[0] - x;
        if d < 0.0 {
            count += 1;
        }
        for i in 1..n {
            if d.abs() < 1e-300 {
                d = 1e-300;
            }
            d = diag[i] - x - off_diag[i - 1] * off_diag[i - 1] / d;
            if d < 0.0 {
                count += 1;
            }
        }
        count
    };

    // Find each of the K largest eigenvalues by bisection
    let mut eigenvalues = Vec::with_capacity(k);
    for i in 0..k {
        // We want eigenvalue #(n-1-i) (0-indexed from smallest)
        let target = n - 1 - i;
        let mut a = lo;
        let mut b = hi;

        for _ in 0..200 {
            let mid = (a + b) / 2.0;
            if (b - a) < 1e-14 * b.abs().max(1.0) {
                break;
            }
            if count_less(mid) <= target {
                a = mid;
            } else {
                b = mid;
            }
        }
        eigenvalues.push((a + b) / 2.0);
    }

    eigenvalues
}

/// Find eigenvector for a given eigenvalue using inverse iteration.
fn tridiag_inverse_iteration(diag: &[f64], off_diag: &[f64], eigenval: f64, n: usize) -> Vec<f64> {
    // Shift the matrix: (A - λI)
    let d: Vec<f64> = diag.iter().map(|&di| di - eigenval).collect();
    let e: Vec<f64> = off_diag.to_vec();

    // LU factorization of tridiagonal (A - λI) with perturbation for singularity
    let mut l = vec![0.0; n.saturating_sub(1)];
    let mut u_diag = vec![0.0; n];
    let u_super = e.clone();

    u_diag[0] = if d[0].abs() < 1e-14 { 1e-14 } else { d[0] };
    for i in 1..n {
        l[i - 1] = e[i - 1] / u_diag[i - 1];
        u_diag[i] = d[i] - l[i - 1] * u_super[i - 1];
        if u_diag[i].abs() < 1e-14 {
            u_diag[i] = 1e-14;
        }
    }

    // Start with a random-ish vector
    let mut x: Vec<f64> = (0..n).map(|i| (i as f64 * 0.7 + 0.3).sin()).collect();

    // 3 iterations of inverse iteration
    for _ in 0..3 {
        // Forward substitution: L·y = x
        for i in 1..n {
            x[i] -= l[i - 1] * x[i - 1];
        }
        // Back substitution: U·x_new = y
        x[n - 1] /= u_diag[n - 1];
        for i in (0..n - 1).rev() {
            x[i] = (x[i] - u_super[i] * x[i + 1]) / u_diag[i];
        }
        // Normalize
        let norm: f64 = x.iter().map(|v| v * v).sum::<f64>().sqrt();
        if norm > 0.0 {
            for v in &mut x {
                *v /= norm;
            }
        }
    }

    x
}

/// Convert f64 window to f32 for use with Sample (Complex<f32>).
pub fn window_to_f32(window: &[f64]) -> Vec<f32> {
    window.iter().map(|&w| w as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bessel_i0_known_values() {
        // I₀(0) = 1
        assert!((bessel_i0(0.0) - 1.0).abs() < 1e-12);
        // I₀(1) ≈ 1.2660658777520082
        assert!((bessel_i0(1.0) - 1.2660658777520082).abs() < 1e-10);
        // I₀(3) ≈ 4.880792585865024
        assert!((bessel_i0(3.0) - 4.880792585865024).abs() < 1e-8);
    }

    #[test]
    fn hann_window_properties() {
        let w = generate_window(&WindowType::Hann, 256);
        assert_eq!(w.len(), 256);
        // Zero at endpoints
        assert!(w[0].abs() < 1e-10);
        assert!(w[255].abs() < 1e-10);
        // Peak at center
        assert!((w[127] - 1.0).abs() < 0.01);
        // Symmetric
        for i in 0..128 {
            assert!((w[i] - w[255 - i]).abs() < 1e-10);
        }
    }

    #[test]
    fn kaiser_window_properties() {
        let w = generate_window(&WindowType::Kaiser { beta: 8.6 }, 256);
        assert_eq!(w.len(), 256);
        // Symmetric
        for i in 0..128 {
            assert!((w[i] - w[255 - i]).abs() < 1e-10);
        }
        // Peak near center (for even N, no sample lands on exact center,
        // so the max is slightly below 1.0; tolerance accounts for this)
        let max_val = w.iter().cloned().fold(0.0f64, f64::max);
        assert!((max_val - 1.0).abs() < 1e-4);
    }

    #[test]
    fn blackman_harris_sidelobes() {
        // BH4 should have very low sidelobes
        let w = generate_window(&WindowType::BlackmanHarris4, 256);
        let metrics = window_metrics(&w);
        // ENBW should be ~2.0 bins for BH4
        assert!((metrics.enbw_bins - 2.0).abs() < 0.1);
    }

    #[test]
    fn flat_top_scalloping() {
        let w = generate_window(&WindowType::FlatTop, 256);
        let metrics = window_metrics(&w);
        // Flat-top should have very low scalloping loss
        assert!(metrics.scalloping_loss_db < 0.1);
    }

    #[test]
    fn tukey_extremes() {
        // α=0 → rectangular
        let rect = generate_window(&WindowType::Tukey { alpha: 0.0 }, 100);
        for &v in &rect {
            assert!((v - 1.0).abs() < 1e-10);
        }

        // α=1 → Hann
        let hann_tukey = generate_window(&WindowType::Tukey { alpha: 1.0 }, 100);
        let hann = generate_window(&WindowType::Hann, 100);
        for (a, b) in hann_tukey.iter().zip(hann.iter()) {
            assert!((a - b).abs() < 1e-10);
        }
    }

    #[test]
    fn dpss_basic() {
        let tapers = dpss(64, 4.0, 3);
        assert_eq!(tapers.len(), 3);
        for taper in &tapers {
            assert_eq!(taper.len(), 64);
            // Each taper should be unit norm
            let norm: f64 = taper.iter().map(|v| v * v).sum::<f64>().sqrt();
            assert!((norm - 1.0).abs() < 1e-6, "Taper norm = {norm}");
        }
    }

    #[test]
    fn dpss_orthogonality() {
        let tapers = dpss(64, 4.0, 3);
        // Tapers should be approximately orthogonal
        for i in 0..tapers.len() {
            for j in (i + 1)..tapers.len() {
                let dot: f64 = tapers[i]
                    .iter()
                    .zip(tapers[j].iter())
                    .map(|(a, b)| a * b)
                    .sum();
                assert!(
                    dot.abs() < 0.1,
                    "Tapers {i} and {j} not orthogonal: dot = {dot}"
                );
            }
        }
    }

    #[test]
    fn kaiser_design_test() {
        let (beta, n) = kaiser_design(60.0, 0.05);
        // For 60 dB attenuation: β ≈ 0.1102*(60-8.7) ≈ 5.65
        assert!((beta - 5.65).abs() < 0.1);
        assert!(n > 30); // Should need a reasonable number of taps
    }

    #[test]
    fn window_metrics_rectangular() {
        let w = generate_window(&WindowType::Rectangular, 256);
        let m = window_metrics(&w);
        assert!((m.coherent_gain - 1.0).abs() < 1e-10);
        assert!((m.enbw_bins - 1.0).abs() < 1e-10);
    }

    #[test]
    fn dolph_chebyshev_symmetric() {
        let w = generate_window(
            &WindowType::DolphChebyshev {
                attenuation_db: 60.0,
            },
            64,
        );
        assert_eq!(w.len(), 64);
        // Should be symmetric
        for i in 0..32 {
            assert!(
                (w[i] - w[63 - i]).abs() < 1e-6,
                "Not symmetric at {i}: {} vs {}",
                w[i],
                w[63 - i]
            );
        }
    }
}
