use std::f32::consts::PI;

use crate::types::Sample;

/// Detected signal in a spectrum.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Detection {
    /// Bin index in the spectrum.
    pub bin: usize,
    /// Power level in dBFS.
    pub power_db: f32,
    /// Estimated local noise floor in dBFS.
    pub noise_floor_db: f32,
    /// SNR above local noise floor in dB.
    pub snr_db: f32,
    /// Frequency offset from center in Hz (if sample_rate provided).
    pub freq_offset_hz: f64,
}

/// CFAR (Constant False Alarm Rate) detection method.
#[derive(Debug, Clone)]
pub enum CfarMethod {
    /// Cell-Averaging CFAR. Uses mean of reference cells.
    /// Most common, works well for homogeneous noise.
    CellAveraging,
    /// Greatest-Of CA-CFAR. Uses the greater of leading/lagging averages.
    /// Better at clutter edges, higher detection threshold.
    GreatestOf,
    /// Smallest-Of CA-CFAR. Uses the lesser of leading/lagging averages.
    /// Better sensitivity, higher false alarm rate at edges.
    SmallestOf,
    /// Ordered-Statistic CFAR. Uses the k-th ordered value.
    /// Robust against interfering targets in reference cells.
    /// `rank` is the index into sorted reference cells (0-indexed).
    /// Typical: rank = 0.75 * num_reference_cells.
    OrderedStatistic { rank: usize },
}

/// CFAR detector configuration.
#[derive(Debug, Clone)]
pub struct CfarConfig {
    /// Detection method.
    pub method: CfarMethod,
    /// Number of reference cells on each side of the cell under test (CUT).
    pub num_reference: usize,
    /// Number of guard cells on each side (protects against signal leakage).
    pub num_guard: usize,
    /// Threshold multiplier (scale factor applied to noise estimate).
    /// For a given P_fa (false alarm probability):
    ///   α = N · (P_fa^(-1/N) - 1)  where N = 2·num_reference
    /// Typical: 3.0-6.0 for CA-CFAR.
    pub threshold_factor: f32,
}

impl Default for CfarConfig {
    fn default() -> Self {
        Self {
            method: CfarMethod::CellAveraging,
            num_reference: 20,
            num_guard: 4,
            threshold_factor: 4.5,
        }
    }
}

impl CfarConfig {
    /// Design threshold factor for a desired probability of false alarm.
    ///
    /// For CA-CFAR with N reference cells and desired P_fa:
    ///   α = N · (P_fa^(-1/N) - 1)
    ///
    /// For OS-CFAR with rank k out of N reference cells:
    ///   Uses the beta distribution approximation.
    pub fn from_pfa(pfa: f64, method: &CfarMethod, num_reference: usize) -> f32 {
        let n = (2 * num_reference) as f64;
        match method {
            CfarMethod::CellAveraging => {
                // Exact: α = N(P_fa^(-1/N) - 1)
                (n * (pfa.powf(-1.0 / n) - 1.0)) as f32
            }
            CfarMethod::GreatestOf | CfarMethod::SmallestOf => {
                // Approximate: use CA-CFAR formula with adjusted N
                let n_eff = n / 2.0; // Each side independently
                (n_eff * (pfa.powf(-1.0 / n_eff) - 1.0)) as f32
            }
            CfarMethod::OrderedStatistic { rank } => {
                // Approximate using Gamma distribution
                let k = (*rank + 1) as f64;
                let n_total = n;
                // Threshold ≈ (k/N) · F_{Beta}^{-1}(1-P_fa; k, N-k+1) / P_fa^(1/k)
                // Simplified approximation:
                (k * pfa.powf(-1.0 / k) / n_total * (n_total - k + 1.0)) as f32
            }
        }
    }
}

/// Run CFAR detection on a power spectrum (linear power, NOT dBFS).
///
/// Input `spectrum_linear` should be linear power values (NOT in dB).
/// Returns indices and detection info for cells exceeding the adaptive threshold.
pub fn cfar_detect(
    spectrum_linear: &[f32],
    config: &CfarConfig,
    sample_rate: f64,
) -> Vec<Detection> {
    let n = spectrum_linear.len();
    let half_window = config.num_reference + config.num_guard;

    if n < 2 * half_window + 1 {
        return Vec::new();
    }

    let mut detections = Vec::new();

    for cut in half_window..(n - half_window) {
        // Collect reference cells (excluding guard cells)
        let mut leading = Vec::with_capacity(config.num_reference);
        let mut lagging = Vec::with_capacity(config.num_reference);

        for i in 1..=config.num_reference {
            let lead_idx = cut - config.num_guard - i;
            let lag_idx = cut + config.num_guard + i;
            if lead_idx < n {
                leading.push(spectrum_linear[lead_idx]);
            }
            if lag_idx < n {
                lagging.push(spectrum_linear[lag_idx]);
            }
        }

        let noise_estimate = match &config.method {
            CfarMethod::CellAveraging => {
                let sum: f32 = leading.iter().chain(lagging.iter()).sum();
                let count = (leading.len() + lagging.len()) as f32;
                sum / count
            }
            CfarMethod::GreatestOf => {
                let lead_avg: f32 = leading.iter().sum::<f32>() / leading.len().max(1) as f32;
                let lag_avg: f32 = lagging.iter().sum::<f32>() / lagging.len().max(1) as f32;
                lead_avg.max(lag_avg)
            }
            CfarMethod::SmallestOf => {
                let lead_avg: f32 = leading.iter().sum::<f32>() / leading.len().max(1) as f32;
                let lag_avg: f32 = lagging.iter().sum::<f32>() / lagging.len().max(1) as f32;
                lead_avg.min(lag_avg)
            }
            CfarMethod::OrderedStatistic { rank } => {
                let mut all: Vec<f32> = leading.iter().chain(lagging.iter()).copied().collect();
                all.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let idx = (*rank).min(all.len().saturating_sub(1));
                all[idx]
            }
        };

        let threshold = noise_estimate * config.threshold_factor;

        if spectrum_linear[cut] > threshold {
            let power_db = 10.0 * spectrum_linear[cut].max(1e-20).log10();
            let noise_db = 10.0 * noise_estimate.max(1e-20).log10();
            let freq_offset = (cut as f64 - n as f64 / 2.0) * sample_rate / n as f64;

            detections.push(Detection {
                bin: cut,
                power_db,
                noise_floor_db: noise_db,
                snr_db: power_db - noise_db,
                freq_offset_hz: freq_offset,
            });
        }
    }

    // Merge adjacent detections (keep peak of each cluster)
    merge_detections(&mut detections, 3)
}

/// Merge adjacent detections, keeping the strongest in each cluster.
fn merge_detections(detections: &mut [Detection], min_gap: usize) -> Vec<Detection> {
    if detections.is_empty() {
        return Vec::new();
    }

    let mut merged = Vec::new();
    let mut current_best = detections[0].clone();

    for det in detections.iter().skip(1) {
        if det.bin <= current_best.bin + min_gap {
            // Same cluster — keep the stronger one
            if det.power_db > current_best.power_db {
                current_best = det.clone();
            }
        } else {
            merged.push(current_best);
            current_best = det.clone();
        }
    }
    merged.push(current_best);

    merged
}

/// Convert dBFS spectrum to linear power for CFAR processing.
pub fn db_to_linear(spectrum_db: &[f32]) -> Vec<f32> {
    spectrum_db
        .iter()
        .map(|&db| 10.0f32.powf(db / 10.0))
        .collect()
}

/// Spectral flatness (Wiener entropy).
///
/// Ratio of geometric mean to arithmetic mean of the power spectrum.
/// Range: 0 (pure tone) to 1 (white noise).
///
/// SF = exp(1/N · Σ ln(S[k])) / (1/N · Σ S[k])
///    = (∏ S[k])^(1/N) / mean(S[k])
///
/// This is a powerful signal detection statistic because white noise
/// has SF ≈ 1 regardless of noise level, while any deterministic signal
/// drives SF toward 0.
pub fn spectral_flatness(spectrum_linear: &[f32]) -> f32 {
    let n = spectrum_linear.len();
    if n == 0 {
        return 0.0;
    }

    let log_sum: f64 = spectrum_linear
        .iter()
        .map(|&s| (s.max(1e-20) as f64).ln())
        .sum::<f64>();
    let arithmetic_mean: f64 = spectrum_linear.iter().map(|&s| s as f64).sum::<f64>() / n as f64;

    if arithmetic_mean <= 0.0 {
        return 0.0;
    }

    let geometric_mean = (log_sum / n as f64).exp();
    (geometric_mean / arithmetic_mean) as f32
}

/// Kurtosis-based signal detection.
///
/// The kurtosis of a Gaussian process is 2 (for complex samples).
/// Signals with structure (deterministic components) have kurtosis ≠ 2.
///
/// Excess kurtosis κ = E[|x|⁴] / E[|x|²]² - 2
///
/// κ = 0: Gaussian (noise only)
/// κ < 0: Sub-Gaussian (constant-envelope signals like FM, PSK)
/// κ > 0: Super-Gaussian (impulsive signals, AM, sparse signals)
///
/// The test statistic κ̂ is compared against thresholds derived from
/// the asymptotic distribution: κ̂ ~ N(0, 24/N) under H₀ (Gaussian).
pub fn complex_kurtosis(samples: &[Sample]) -> f32 {
    let n = samples.len();
    if n < 4 {
        return 0.0;
    }

    let moment2: f64 = samples.iter().map(|s| s.norm_sqr() as f64).sum::<f64>() / n as f64;
    let moment4: f64 = samples
        .iter()
        .map(|s| {
            let p = s.norm_sqr() as f64;
            p * p
        })
        .sum::<f64>()
        / n as f64;

    if moment2 <= 0.0 {
        return 0.0;
    }

    // Excess kurtosis (relative to Gaussian)
    (moment4 / (moment2 * moment2) - 2.0) as f32
}

/// Energy detection (Neyman-Pearson).
///
/// Tests H₀: x[n] = w[n] (noise only) vs H₁: x[n] = s[n] + w[n] (signal present)
///
/// Test statistic: T = (1/N) · Σ|x[n]|²
///
/// Under H₀ with known noise variance σ²:
///   2N·T/σ² ~ χ²(2N) ≈ N(2N, 4N) for large N
///
/// Threshold for P_fa:
///   γ = σ² · (1 + Q^{-1}(P_fa) · √(2/N))
///
/// Returns: (test_statistic, threshold, detected)
pub fn energy_detect(samples: &[Sample], noise_variance: f32, pfa: f64) -> (f32, f32, bool) {
    let n = samples.len();
    if n == 0 {
        return (0.0, 0.0, false);
    }

    // Test statistic: average power
    let test_stat: f32 = samples.iter().map(|s| s.norm_sqr()).sum::<f32>() / n as f32;

    // Threshold using Gaussian approximation to chi-squared
    // Q^{-1}(P_fa) ≈ -Φ^{-1}(P_fa) where Φ is the standard normal CDF
    let z = qinv(pfa);
    let threshold = noise_variance * (1.0 + z as f32 * (2.0 / n as f32).sqrt());

    (test_stat, threshold, test_stat > threshold)
}

/// Cyclostationary feature detection.
///
/// Computes the spectral correlation density (SCD) at cycle frequency α.
/// Modulated signals exhibit cyclostationary features at α related to their
/// symbol rate, carrier frequency, etc.
///
/// The SCD is estimated using the time-smoothed cross-periodogram:
///   S_x^α(f) = <X(f+α/2) · X*(f-α/2)>
///
/// where X is the STFT of x and <·> denotes time averaging.
///
/// Returns the normalized cyclic power at the given cycle frequency.
/// Values near 0 = noise, values >> 0 = cyclostationary signal present.
pub fn cyclostationary_detect(
    samples: &[Sample],
    cycle_freq_normalized: f32,
    fft_size: usize,
) -> f32 {
    let n = samples.len();
    if n < 2 * fft_size {
        return 0.0;
    }

    let num_segments = n / fft_size;
    let half_alpha_bins = (cycle_freq_normalized * fft_size as f32 / 2.0) as i32;

    let mut cyclic_power = 0.0f64;
    let mut total_power = 0.0f64;

    // Compute cross-correlations in frequency domain
    for seg in 0..num_segments {
        let start = seg * fft_size;
        let segment = &samples[start..start + fft_size];

        // DFT of segment (direct computation for flexibility)
        let mut spectrum: Vec<Sample> = vec![Sample::new(0.0, 0.0); fft_size];
        #[allow(clippy::needless_range_loop)]
        for k in 0..fft_size {
            let mut sum = Sample::new(0.0, 0.0);
            for (nn, &s) in segment.iter().enumerate() {
                let phase = -2.0 * PI * k as f32 * nn as f32 / fft_size as f32;
                let twiddle = Sample::new(phase.cos(), phase.sin());
                sum += s * twiddle;
            }
            spectrum[k] = sum;
        }

        // Cross-periodogram at cycle frequency α
        for k in 0..fft_size {
            let k_plus = ((k as i32 + half_alpha_bins).rem_euclid(fft_size as i32)) as usize;
            let k_minus = ((k as i32 - half_alpha_bins).rem_euclid(fft_size as i32)) as usize;

            let cross = spectrum[k_plus] * spectrum[k_minus].conj();
            cyclic_power += cross.norm_sqr() as f64;
            total_power += spectrum[k].norm_sqr() as f64;
        }
    }

    if total_power <= 0.0 {
        return 0.0;
    }

    // Normalized cyclic power
    (cyclic_power / (total_power * num_segments as f64)) as f32
}

/// Inverse Q-function (complementary Gaussian CDF).
///
/// Q(x) = 0.5 · erfc(x/√2)
/// Q⁻¹(p) ≈ rational approximation (Abramowitz & Stegun, improved).
///
/// Accuracy: <1.5×10⁻⁹ for 0 < p < 1.
fn qinv(p: f64) -> f64 {
    if p <= 0.0 {
        return f64::MAX;
    }
    if p >= 1.0 {
        return f64::MIN;
    }
    if p == 0.5 {
        return 0.0;
    }

    // Use the rational approximation for the normal quantile function
    // Based on Peter Acklam's algorithm
    let p_low = 0.02425;
    let p_high = 1.0 - p_low;

    if p < p_low {
        // Rational approximation for lower region
        let q = (-2.0 * p.ln()).sqrt();
        let num = ((((-7.784894002430293e-03 * q + -3.223964580411365e-01) * q
            + -2.400758277161838e+00)
            * q
            + -2.549732539343734e+00)
            * q
            + 4.374664141464968e+00)
            * q
            + 2.938163982698783e+00;
        let den =
            (((7.784695709041462e-03 * q + 3.224671290700398e-01) * q + 2.445134137142996e+00) * q
                + 3.754408661907416e+00)
                * q
                + 1.0;
        num / den
    } else if p <= p_high {
        // Rational approximation for central region
        let q = p - 0.5;
        let r = q * q;
        let num = (((((-3.969683028665376e+01 * r + 2.209460984245205e+02) * r
            + -2.759285104469687e+02)
            * r
            + 1.383_577_518_672_69e2)
            * r
            + -3.066479806614716e+01)
            * r
            + 2.506628277459239e+00)
            * q;
        let den = ((((-5.447609879822406e+01 * r + 1.615858368580409e+02) * r
            + -1.556989798598866e+02)
            * r
            + 6.680131188771972e+01)
            * r
            + -1.328068155288572e+01)
            * r
            + 1.0;
        num / den
    } else {
        // Rational approximation for upper region
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        let num = ((((-7.784894002430293e-03 * q + -3.223964580411365e-01) * q
            + -2.400758277161838e+00)
            * q
            + -2.549732539343734e+00)
            * q
            + 4.374664141464968e+00)
            * q
            + 2.938163982698783e+00;
        let den =
            (((7.784695709041462e-03 * q + 3.224671290700398e-01) * q + 2.445134137142996e+00) * q
                + 3.754408661907416e+00)
                * q
                + 1.0;
        -(num / den)
    }
}

/// Noise floor estimation using iterative sigma-clipping.
///
/// Iteratively removes samples above μ + k·σ, then re-estimates μ and σ
/// from the remaining samples. Converges to the noise-only statistics
/// even when signals are present.
///
/// More accurate than median-based estimation for multi-signal environments.
pub fn noise_floor_sigma_clip(spectrum_db: &[f32], num_iterations: usize, kappa: f32) -> f32 {
    if spectrum_db.is_empty() {
        return -200.0;
    }

    let mut mask = vec![true; spectrum_db.len()];

    for _ in 0..num_iterations {
        let (mean, std) = masked_mean_std(spectrum_db, &mask);
        let threshold = mean + kappa * std;

        let mut changed = false;
        for (i, &val) in spectrum_db.iter().enumerate() {
            if mask[i] && val > threshold {
                mask[i] = false;
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }

    let (mean, _) = masked_mean_std(spectrum_db, &mask);
    mean
}

fn masked_mean_std(values: &[f32], mask: &[bool]) -> (f32, f32) {
    let mut sum = 0.0f64;
    let mut sum_sq = 0.0f64;
    let mut count = 0u64;

    for (i, &v) in values.iter().enumerate() {
        if mask[i] {
            sum += v as f64;
            sum_sq += (v as f64) * (v as f64);
            count += 1;
        }
    }

    if count == 0 {
        return (-200.0, 0.0);
    }

    let mean = sum / count as f64;
    let variance = (sum_sq / count as f64 - mean * mean).max(0.0);
    (mean as f32, variance.sqrt() as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cfar_detects_tone_in_noise() {
        // Create spectrum with noise floor at ~0.001 and a tone at bin 100 at ~1.0
        let mut spectrum = vec![0.001f32; 256];
        spectrum[100] = 1.0;

        let config = CfarConfig::default();
        let detections = cfar_detect(&spectrum, &config, 2_048_000.0);

        assert!(!detections.is_empty(), "CFAR should detect the tone");
        assert_eq!(detections[0].bin, 100);
        assert!(detections[0].snr_db > 20.0);
    }

    #[test]
    fn cfar_multiple_signals() {
        let mut spectrum = vec![0.001f32; 512];
        spectrum[100] = 0.5;
        spectrum[300] = 1.0;
        spectrum[400] = 0.2;

        let config = CfarConfig::default();
        let detections = cfar_detect(&spectrum, &config, 2_048_000.0);

        assert!(
            detections.len() >= 3,
            "Should detect 3 signals, found {}",
            detections.len()
        );
    }

    #[test]
    fn cfar_no_false_alarms_in_noise() {
        // Flat noise floor
        let spectrum = vec![0.001f32; 256];
        let config = CfarConfig {
            threshold_factor: 6.0,
            ..Default::default()
        };
        let detections = cfar_detect(&spectrum, &config, 2_048_000.0);
        assert!(
            detections.is_empty(),
            "Should have no detections in flat noise"
        );
    }

    #[test]
    fn spectral_flatness_noise_vs_tone() {
        // White noise has flatness near 1
        let noise: Vec<f32> = (0..256)
            .map(|i| 0.001 + 0.0001 * (i as f32 * 0.37).sin())
            .collect();
        let sf_noise = spectral_flatness(&noise);

        // Tone in noise has low flatness
        let mut tone = noise.clone();
        tone[128] = 10.0;
        let sf_tone = spectral_flatness(&tone);

        assert!(
            sf_noise > sf_tone,
            "Noise flatness ({sf_noise}) should exceed tone flatness ({sf_tone})"
        );
    }

    #[test]
    fn kurtosis_gaussian_vs_constant() {
        // Gaussian-like samples should have kurtosis near 0 (excess)
        let gaussian: Vec<Sample> = (0..10000)
            .map(|i| {
                let x = (i as f32 * 0.7).sin() + (i as f32 * 1.3).cos() * 0.5;
                let y = (i as f32 * 0.9).cos() + (i as f32 * 1.7).sin() * 0.5;
                Sample::new(x * 0.1, y * 0.1)
            })
            .collect();

        let kurt = complex_kurtosis(&gaussian);
        // Not exactly 0 since our "gaussian" is deterministic, but should be finite
        assert!(kurt.abs() < 5.0, "Kurtosis should be reasonable: {kurt}");
    }

    #[test]
    fn energy_detection_basic() {
        // Signal present (power = 2.0, noise variance = 0.5)
        let signal: Vec<Sample> = (0..1000).map(|_| Sample::new(1.0, 1.0)).collect();
        let (stat, threshold, detected) = energy_detect(&signal, 0.5, 0.01);
        assert!(
            detected,
            "Should detect signal: stat={stat}, threshold={threshold}"
        );

        // Noise only (power ≈ noise variance)
        let noise: Vec<Sample> = (0..1000)
            .map(|i| Sample::new((i as f32 * 0.37).sin() * 0.5, (i as f32 * 0.73).cos() * 0.5))
            .collect();
        let (stat2, _threshold2, _detected2) = energy_detect(&noise, 0.5, 0.01);
        // Noise power should be close to noise variance
        assert!(
            (stat2 - 0.25).abs() < 0.2,
            "Noise stat should be ~0.25: {stat2}"
        );
    }

    #[test]
    fn sigma_clip_noise_floor() {
        // Spectrum with noise floor at -80 dBFS and signals at -20, -30
        let mut spectrum = vec![-80.0f32; 512];
        spectrum[100] = -20.0;
        spectrum[101] = -25.0;
        spectrum[200] = -30.0;
        spectrum[300] = -15.0;

        let floor = noise_floor_sigma_clip(&spectrum, 5, 2.5);
        assert!(
            (floor - (-80.0)).abs() < 2.0,
            "Noise floor should be ~-80, got {floor}"
        );
    }

    #[test]
    fn pfa_threshold_design() {
        // For P_fa = 1e-6 with 40 reference cells
        let alpha = CfarConfig::from_pfa(1e-6, &CfarMethod::CellAveraging, 20);
        assert!(alpha > 1.0, "Threshold should be > 1.0: {alpha}");
        assert!(
            alpha < 20.0,
            "Threshold shouldn't be absurdly high: {alpha}"
        );
    }
}
