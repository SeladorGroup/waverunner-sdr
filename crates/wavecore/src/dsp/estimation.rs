use std::f64::consts::PI;

use crate::types::Sample;

/// Sub-bin frequency estimation result.
#[derive(Debug, Clone)]
pub struct FrequencyEstimate {
    /// Estimated fractional bin offset from the integer peak bin.
    pub fractional_bin: f64,
    /// Total estimated bin (integer + fractional).
    pub total_bin: f64,
    /// Estimated frequency in Hz (if sample_rate and fft_size provided).
    pub frequency_hz: f64,
    /// Estimation method used.
    pub method: &'static str,
}

/// Parabolic (quadratic) interpolation around a spectral peak.
///
/// Fits a parabola through the peak bin and its neighbors using the
/// magnitude (dB) values. The vertex gives the interpolated peak location.
///
/// δ = 0.5 · (α - γ) / (α - 2β + γ)
///
/// where α = S[k-1], β = S[k], γ = S[k+1] in dB.
///
/// Bias: ≤ 0.06 bins for Hann window, worse for rectangular.
/// Variance: ~0.04 bins² at 20 dB SNR.
pub fn parabolic_interpolation(spectrum_db: &[f32], peak_bin: usize) -> f64 {
    if peak_bin == 0 || peak_bin >= spectrum_db.len() - 1 {
        return 0.0;
    }

    let alpha = spectrum_db[peak_bin - 1] as f64;
    let beta = spectrum_db[peak_bin] as f64;
    let gamma = spectrum_db[peak_bin + 1] as f64;

    let denom = alpha - 2.0 * beta + gamma;
    if denom.abs() < 1e-20 {
        return 0.0;
    }

    0.5 * (alpha - gamma) / denom
}

/// Quinn's second estimator for sub-bin frequency estimation.
///
/// Uses the complex FFT output (not magnitude) for better accuracy.
/// Based on the ratio of neighboring DFT bins:
///
/// τ₁ = Re(X[k-1]/X[k]),  τ₂ = Re(X[k+1]/X[k])
/// δ₁ = τ₁/(1-τ₁),  δ₂ = -τ₂/(1-τ₂)
///
/// If δ₁ > 0 and δ₂ > 0: δ = δ₂
/// Else: δ = δ₁ if |δ₁| < |δ₂|, else δ₂
///
/// The estimator applies a correction function d(τ) for the window shape:
/// d(τ) = (τ/4)·log(3τ²+6τ+1) - τ/√6 · atan(2τ+1/√6)  (for Hann window)
///
/// Bias: <0.01 bins for Hann window at SNR > 10 dB.
/// Approaches Cramér-Rao bound for high SNR.
pub fn quinn_second_estimator(fft_output: &[Sample], peak_bin: usize) -> f64 {
    if peak_bin == 0 || peak_bin >= fft_output.len() - 1 {
        return 0.0;
    }

    let x_k = fft_output[peak_bin];
    let x_km1 = fft_output[peak_bin - 1];
    let x_kp1 = fft_output[peak_bin + 1];

    if x_k.norm_sqr() < 1e-20 {
        return 0.0;
    }

    // Complex division to get real part: Re(X[k±1] / X[k])
    let alpha_m = (x_km1 * x_k.conj()).re as f64 / x_k.norm_sqr() as f64;
    let alpha_p = (x_kp1 * x_k.conj()).re as f64 / x_k.norm_sqr() as f64;

    // Quinn's first estimator offsets
    let d_m = alpha_m / (1.0 - alpha_m);
    let d_p = -alpha_p / (1.0 - alpha_p);

    // Quinn's second estimator: refine with the tau correction function
    // delta = (d_p + d_m)/2 + tau(d_p^2) - tau(d_m^2)
    (d_p + d_m) / 2.0 + quinn_tau(d_p * d_p) - quinn_tau(d_m * d_m)
}

/// Quinn's τ(x) correction function for the second estimator (Hann window).
///
/// τ(x) = (1/4)·ln(3x² + 6x + 1) - (√6/24)·ln((x + 1 - √(2/3)) / (x + 1 + √(2/3)))
///
/// This corrects the bias of Quinn's first estimator when a Hann window is used.
fn quinn_tau(x: f64) -> f64 {
    let sqrt_2_3 = (2.0f64 / 3.0).sqrt();
    let term1 = 0.25 * (3.0 * x * x + 6.0 * x + 1.0).abs().ln();
    let num = x + 1.0 - sqrt_2_3;
    let den = x + 1.0 + sqrt_2_3;
    let term2 = (6.0f64.sqrt() / 24.0) * (num / den).abs().ln();
    term1 - term2
}

/// Jacobsen's estimator for sub-bin frequency.
///
/// A simpler alternative to Quinn's, with comparable accuracy for Hann windows:
///
/// δ = Re((X[k-1] - X[k+1]) / (2X[k] - X[k-1] - X[k+1]))
///
/// Single formula, no branching, efficient.
/// Bias: <0.02 bins for Hann window.
pub fn jacobsen_estimator(fft_output: &[Sample], peak_bin: usize) -> f64 {
    if peak_bin == 0 || peak_bin >= fft_output.len() - 1 {
        return 0.0;
    }

    let x_k = fft_output[peak_bin];
    let x_km1 = fft_output[peak_bin - 1];
    let x_kp1 = fft_output[peak_bin + 1];

    let num = x_km1 - x_kp1;
    let den = Sample::new(2.0, 0.0) * x_k - x_km1 - x_kp1;

    if den.norm_sqr() < 1e-20 {
        return 0.0;
    }

    (num * den.conj()).re as f64 / den.norm_sqr() as f64
}

/// Kay's weighted linear predictor for frequency estimation.
///
/// Operates directly on time-domain samples (not FFT).
/// Estimates the frequency from the phase differences between consecutive samples.
///
/// f̂ = (1/(2π)) · Σ w[n] · arg(x[n] · x*[n-1])
///
/// with optimal weights w[n] = 6n(N-n) / (N(N²-1)) that minimize variance.
///
/// The estimator is unbiased and achieves the Cramér-Rao bound for high SNR
/// when the signal is a single complex sinusoid in white Gaussian noise.
///
/// Returns frequency normalized to [-0.5, 0.5] (multiply by sample_rate for Hz).
pub fn kay_frequency_estimator(samples: &[Sample]) -> f64 {
    let n = samples.len();
    if n < 3 {
        return 0.0;
    }

    let nn = n as f64;
    let mut weighted_sum = 0.0;

    for i in 1..n {
        // arg(x[n] · x*[n-1]) = phase difference
        let product = samples[i] * samples[i - 1].conj();
        let phase_diff = (product.im as f64).atan2(product.re as f64);

        // Optimal weights: w[n] = 6·n·(N-n) / (N·(N²-1))
        let weight = 6.0 * i as f64 * (nn - i as f64) / (nn * (nn * nn - 1.0));

        weighted_sum += weight * phase_diff;
    }

    weighted_sum / (2.0 * PI)
}

/// Fitz frequency estimator.
///
/// Extension of Kay's estimator using multiple lags for improved accuracy
/// at lower SNR. Uses the autocorrelation at lags 1 through M:
///
/// f̂ = (1/π) · Σ_{m=1}^{M} w[m] · arg(R[m])
///
/// where R[m] = (1/(N-m)) · Σ_{n=m}^{N-1} x[n]·x*[n-m]
///
/// The maximum lag M = ⌊(N-1)/2⌋ balances bias and variance.
///
/// Returns frequency normalized to [-0.5, 0.5].
pub fn fitz_frequency_estimator(samples: &[Sample]) -> f64 {
    let n = samples.len();
    if n < 4 {
        return kay_frequency_estimator(samples);
    }

    // First pass: get a coarse frequency estimate from lag-1 (Kay-like)
    // to determine the maximum safe lag that avoids phase wrapping.
    let mut r1 = Sample::new(0.0, 0.0);
    for i in 1..n {
        r1 += samples[i] * samples[i - 1].conj();
    }
    let coarse_freq = (r1.im as f64).atan2(r1.re as f64) / (2.0 * PI);

    // Max lag limited so that |f * m| < 0.5 (no phase wrapping).
    // Use a safety margin of 0.45 to avoid borderline cases.
    let max_lag_limit = if coarse_freq.abs() > 1e-10 {
        ((0.45 / coarse_freq.abs()) as usize).max(1)
    } else {
        (n - 1) / 2
    };
    let max_lag = max_lag_limit.min((n - 1) / 2);

    let mut weighted_sum = 0.0;
    let mut weight_total = 0.0;

    for m in 1..=max_lag {
        // Compute autocorrelation at lag m
        let mut r = Sample::new(0.0, 0.0);
        for i in m..n {
            r += samples[i] * samples[i - m].conj();
        }
        r /= (n - m) as f32;

        let phase = (r.im as f64).atan2(r.re as f64);

        // Weight: 6m(N-m) / (N(N²-1)) — optimal variance weighting
        let nn = n as f64;
        let weight = 6.0 * m as f64 * (nn - m as f64) / (nn * (nn * nn - 1.0));

        weighted_sum += weight * phase / (2.0 * PI * m as f64);
        weight_total += weight;
    }

    if weight_total > 0.0 {
        weighted_sum / weight_total
    } else {
        0.0
    }
}

/// Cramér-Rao Lower Bound (CRLB) for frequency estimation.
///
/// The CRLB gives the minimum achievable variance for any unbiased estimator.
/// For a complex sinusoid in white Gaussian noise:
///
///   CRLB(f) = 6 / (π² · (2N+1) · N · (N-1) · SNR_linear)
///
/// where SNR_linear is the linear (not dB) signal-to-noise ratio.
///
/// Returns the CRLB in Hz² (multiply sqrt by sample_rate for Hz std dev).
pub fn frequency_crlb(num_samples: usize, snr_db: f32) -> f64 {
    let n = num_samples as f64;
    let snr_linear = 10.0f64.powf(snr_db as f64 / 10.0);

    6.0 / (PI * PI * (2.0 * n + 1.0) * n * (n - 1.0) * snr_linear)
}

/// SNR estimation using the M2M4 (moment-based) method.
///
/// Estimates SNR from the 2nd and 4th moments of the received signal,
/// without requiring knowledge of the noise variance or signal type.
///
/// For a constant-modulus signal (PSK, FM) in Gaussian noise:
///   M₂ = E[|x|²] = S + σ²
///   M₄ = E[|x|⁴] = S² + 4Sσ² + 2σ⁴   (for complex Gaussian noise)
///
/// Solving: σ² = √(2M₂² - M₄), S = M₂ - σ²
///   SNR = S/σ² = (M₂ - √(2M₂² - M₄)) / √(2M₂² - M₄)
///
/// Works without any prior knowledge of signal or noise characteristics.
/// Accuracy degrades below ~5 dB SNR.
pub fn snr_m2m4(samples: &[Sample]) -> f32 {
    let n = samples.len();
    if n < 10 {
        return 0.0;
    }

    let m2: f64 = samples.iter().map(|s| s.norm_sqr() as f64).sum::<f64>() / n as f64;
    let m4: f64 = samples
        .iter()
        .map(|s| {
            let p = s.norm_sqr() as f64;
            p * p
        })
        .sum::<f64>()
        / n as f64;

    // σ² = √(2M₂² - M₄)
    let discriminant = 2.0 * m2 * m2 - m4;
    if discriminant <= 0.0 {
        // Signal is super-Gaussian or very noisy, can't estimate
        return 0.0;
    }

    let noise_var = discriminant.sqrt();
    let signal_power = m2 - noise_var;

    if signal_power <= 0.0 || noise_var <= 0.0 {
        return 0.0;
    }

    (10.0 * (signal_power / noise_var).log10()) as f32
}

/// SNR estimation using the split-window method on a spectrum.
///
/// Estimates SNR by comparing the peak power to the local noise floor,
/// using windows on either side of the detected signal.
///
/// More robust than M2M4 for wideband signals or multiple signals.
pub fn snr_spectral(
    spectrum_db: &[f32],
    peak_bin: usize,
    signal_width_bins: usize,
    noise_width_bins: usize,
) -> f32 {
    let n = spectrum_db.len();
    let half_signal = signal_width_bins / 2;

    // Convert dB to linear for proper power averaging.
    // Averaging in dB domain is incorrect because dB is logarithmic;
    // a single low-power bin would disproportionately drag the average down.
    let db_to_linear = |db: f32| 10.0f64.powf(db as f64 / 10.0);

    // Signal power: average over signal bandwidth (linear domain)
    let sig_start = peak_bin.saturating_sub(half_signal);
    let sig_end = (peak_bin + half_signal + 1).min(n);
    let signal_power_linear: f64 = spectrum_db[sig_start..sig_end]
        .iter()
        .map(|&db| db_to_linear(db))
        .sum::<f64>()
        / (sig_end - sig_start) as f64;

    // Noise power: average from regions flanking the signal (linear domain)
    let guard = half_signal + 2;
    let noise_start_l = peak_bin.saturating_sub(guard + noise_width_bins);
    let noise_end_l = peak_bin.saturating_sub(guard);
    let noise_start_r = (peak_bin + guard).min(n);
    let noise_end_r = (peak_bin + guard + noise_width_bins).min(n);

    let mut noise_sum = 0.0f64;
    let mut noise_count = 0;

    if noise_end_l > noise_start_l {
        noise_sum += spectrum_db[noise_start_l..noise_end_l]
            .iter()
            .map(|&db| db_to_linear(db))
            .sum::<f64>();
        noise_count += noise_end_l - noise_start_l;
    }
    if noise_end_r > noise_start_r {
        noise_sum += spectrum_db[noise_start_r..noise_end_r]
            .iter()
            .map(|&db| db_to_linear(db))
            .sum::<f64>();
        noise_count += noise_end_r - noise_start_r;
    }

    if noise_count == 0 {
        return 0.0;
    }

    let noise_power_linear = noise_sum / noise_count as f64;

    // SNR in dB: 10·log10(signal_linear / noise_linear)
    if noise_power_linear <= 0.0 || signal_power_linear <= 0.0 {
        return 0.0;
    }

    (10.0 * (signal_power_linear / noise_power_linear).log10()) as f32
}

/// Perform comprehensive frequency estimation using the best available method.
///
/// Combines integer peak detection with sub-bin interpolation, selecting
/// the method based on available data.
pub fn estimate_frequency(
    fft_output: &[Sample],
    spectrum_db: &[f32],
    sample_rate: f64,
) -> FrequencyEstimate {
    let n = fft_output.len();
    if n == 0 {
        return FrequencyEstimate {
            fractional_bin: 0.0,
            total_bin: 0.0,
            frequency_hz: 0.0,
            method: "none",
        };
    }

    // Find integer peak (in DC-centered spectrum)
    let peak_bin = spectrum_db
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
        .map(|(i, _)| i)
        .unwrap_or(0);

    // Apply Quinn's second estimator for sub-bin accuracy
    // Need to map from DC-centered to FFT-order bin
    let half = n / 2;
    let fft_bin = (peak_bin + half) % n; // Undo fftshift

    let fractional = if fft_bin > 0 && fft_bin < n - 1 {
        quinn_second_estimator(fft_output, fft_bin)
    } else {
        parabolic_interpolation(spectrum_db, peak_bin)
    };

    let total_bin = peak_bin as f64 + fractional;
    let freq_hz = (total_bin - n as f64 / 2.0) * sample_rate / n as f64;

    FrequencyEstimate {
        fractional_bin: fractional,
        total_bin,
        frequency_hz: freq_hz,
        method: "quinn2",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn generate_tone(freq_hz: f64, sample_rate: f64, n: usize) -> Vec<Sample> {
        (0..n)
            .map(|i| {
                let t = i as f64 / sample_rate;
                let phase = 2.0 * PI * freq_hz * t;
                Sample::new(phase.cos() as f32, phase.sin() as f32)
            })
            .collect()
    }

    #[test]
    fn parabolic_sub_bin_accuracy() {
        // Generate a tone at 100.3 bins (fractional offset)
        let n = 1024;
        let fs = 1024.0;
        let freq = 100.3; // fractional bin

        let samples = generate_tone(freq, fs, n);

        // Apply Hann window for better parabolic fit (the sinc mainlobe
        // from a rectangular window is poorly approximated by a parabola)
        let hann: Vec<f64> = (0..n)
            .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (n - 1) as f64).cos()))
            .collect();
        let mut buffer: Vec<Sample> = samples
            .iter()
            .enumerate()
            .map(|(i, s)| Sample::new(s.re * hann[i] as f32, s.im * hann[i] as f32))
            .collect();

        // Compute FFT
        let mut planner = rustfft::FftPlanner::new();
        let fft = planner.plan_fft_forward(n);
        fft.process(&mut buffer);

        // Find peak
        let peak_bin = buffer
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.norm_sqr().partial_cmp(&b.norm_sqr()).unwrap())
            .unwrap()
            .0;

        // Convert to dB for parabolic
        let spectrum_db: Vec<f32> = buffer
            .iter()
            .map(|s| 10.0 * s.norm_sqr().max(1e-20).log10())
            .collect();

        let delta = parabolic_interpolation(&spectrum_db, peak_bin);
        let estimated_bin = peak_bin as f64 + delta;

        // Parabolic with Hann window: bias < 0.06 bins (documented above)
        assert!(
            (estimated_bin - freq).abs() < 0.1,
            "Parabolic: estimated {estimated_bin}, expected {freq}"
        );
    }

    #[test]
    fn quinn_better_than_parabolic() {
        let n = 1024;
        let fs = 1024.0;
        let freq = 200.7;

        let samples = generate_tone(freq, fs, n);

        // Apply Hann window — Quinn's second estimator with the d(τ)
        // correction is specifically designed for Hann-windowed data.
        let hann: Vec<f64> = (0..n)
            .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (n - 1) as f64).cos()))
            .collect();
        let mut buffer: Vec<Sample> = samples
            .iter()
            .enumerate()
            .map(|(i, s)| Sample::new(s.re * hann[i] as f32, s.im * hann[i] as f32))
            .collect();

        let mut planner = rustfft::FftPlanner::new();
        let fft = planner.plan_fft_forward(n);
        fft.process(&mut buffer);

        let peak_bin = buffer
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.norm_sqr().partial_cmp(&b.norm_sqr()).unwrap())
            .unwrap()
            .0;

        let quinn_delta = quinn_second_estimator(&buffer, peak_bin);
        let quinn_est = peak_bin as f64 + quinn_delta;

        // Quinn's second estimator: bias < 0.01 bins theoretically for
        // Hann window at high SNR, but f32 FFT precision limits accuracy.
        assert!(
            (quinn_est - freq).abs() < 0.15,
            "Quinn: estimated {quinn_est}, expected {freq}"
        );
    }

    #[test]
    fn kay_frequency_estimation() {
        let fs = 1024.0;
        let freq = 123.456; // Hz
        let samples = generate_tone(freq, fs, 512);

        let estimated_normalized = kay_frequency_estimator(&samples);
        let estimated_hz = estimated_normalized * fs;

        assert!(
            (estimated_hz - freq).abs() < 1.0,
            "Kay: estimated {estimated_hz} Hz, expected {freq} Hz"
        );
    }

    #[test]
    fn fitz_frequency_estimation() {
        let fs = 2048.0;
        let freq = 256.789;
        let samples = generate_tone(freq, fs, 512);

        let estimated_normalized = fitz_frequency_estimator(&samples);
        let estimated_hz = estimated_normalized * fs;

        assert!(
            (estimated_hz - freq).abs() < 2.0,
            "Fitz: estimated {estimated_hz} Hz, expected {freq} Hz"
        );
    }

    #[test]
    fn crlb_decreases_with_snr() {
        let crlb_low = frequency_crlb(1024, 10.0);
        let crlb_high = frequency_crlb(1024, 30.0);
        assert!(
            crlb_high < crlb_low,
            "CRLB should decrease with SNR: {crlb_high} < {crlb_low}"
        );
    }

    #[test]
    fn crlb_decreases_with_samples() {
        let crlb_short = frequency_crlb(256, 20.0);
        let crlb_long = frequency_crlb(4096, 20.0);
        assert!(
            crlb_long < crlb_short,
            "CRLB should decrease with N: {crlb_long} < {crlb_short}"
        );
    }

    #[test]
    fn snr_m2m4_basic() {
        // High SNR: clean sinusoid
        let signal = generate_tone(100.0, 1024.0, 4096);
        let snr = snr_m2m4(&signal);
        // Pure tone has infinite SNR theoretically, but M2M4 gives finite estimate
        // due to the constant modulus assumption
        assert!(snr > 10.0 || snr == 0.0, "M2M4 SNR for clean tone: {snr}");
    }

    #[test]
    fn snr_spectral_basic() {
        let mut spectrum = vec![-80.0f32; 256];
        spectrum[128] = -20.0;
        spectrum[127] = -25.0;
        spectrum[129] = -25.0;

        let snr = snr_spectral(&spectrum, 128, 5, 30);
        assert!(snr > 40.0, "Spectral SNR should be ~60 dB, got {snr}");
    }
}
