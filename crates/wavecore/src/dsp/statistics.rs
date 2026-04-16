use std::f64::consts::PI;

use crate::types::Sample;

/// Comprehensive signal statistics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SignalStats {
    pub mean: Sample,
    pub variance: f32,
    pub rms: f32,
    pub peak: f32,
    pub crest_factor_db: f32,
    pub skewness: f32,
    pub kurtosis: f32,
    pub excess_kurtosis: f32,
}

/// Compute comprehensive signal statistics.
pub fn signal_statistics(samples: &[Sample]) -> SignalStats {
    let n = samples.len();
    if n == 0 {
        return SignalStats {
            mean: Sample::new(0.0, 0.0),
            variance: 0.0,
            rms: 0.0,
            peak: 0.0,
            crest_factor_db: 0.0,
            skewness: 0.0,
            kurtosis: 0.0,
            excess_kurtosis: 0.0,
        };
    }

    // Mean
    let mean_re = samples.iter().map(|s| s.re as f64).sum::<f64>() / n as f64;
    let mean_im = samples.iter().map(|s| s.im as f64).sum::<f64>() / n as f64;
    let mean = Sample::new(mean_re as f32, mean_im as f32);

    // Centered moments
    let magnitudes: Vec<f64> = samples
        .iter()
        .map(|s| {
            let re = s.re as f64 - mean_re;
            let im = s.im as f64 - mean_im;
            (re * re + im * im).sqrt()
        })
        .collect();

    let m2: f64 = magnitudes.iter().map(|&m| m * m).sum::<f64>() / n as f64;
    let m3: f64 = magnitudes.iter().map(|&m| m * m * m).sum::<f64>() / n as f64;
    let m4: f64 = magnitudes.iter().map(|&m| m * m * m * m).sum::<f64>() / n as f64;

    let variance = m2;
    let std_dev = variance.sqrt();
    let rms = std_dev;

    let peak = magnitudes.iter().cloned().fold(0.0f64, f64::max);

    let crest_factor_db = if rms > 0.0 {
        20.0 * (peak / rms).log10()
    } else {
        0.0
    };

    let skewness = if std_dev > 0.0 {
        m3 / (std_dev * std_dev * std_dev)
    } else {
        0.0
    };

    // Kurtosis for complex: E[|x|⁴] / E[|x|²]²
    let kurtosis = if variance > 0.0 {
        m4 / (variance * variance)
    } else {
        0.0
    };

    // Excess kurtosis: κ - 2 for complex Gaussian (κ=2 is Gaussian)
    let excess_kurtosis = kurtosis - 2.0;

    SignalStats {
        mean,
        variance: variance as f32,
        rms: rms as f32,
        peak: peak as f32,
        crest_factor_db: crest_factor_db as f32,
        skewness: skewness as f32,
        kurtosis: kurtosis as f32,
        excess_kurtosis: excess_kurtosis as f32,
    }
}

/// Autocorrelation function `R[τ] = E[x[n] · x*[n-τ]]`.
///
/// Computes the biased autocorrelation (normalized by N, not N-τ)
/// which is guaranteed to produce a positive semi-definite sequence.
///
/// Returns lags 0 through max_lag.
pub fn autocorrelation(samples: &[Sample], max_lag: usize) -> Vec<Sample> {
    let n = samples.len();
    let max_lag = max_lag.min(n - 1);

    (0..=max_lag)
        .map(|lag| {
            let mut sum = Sample::new(0.0, 0.0);
            for i in lag..n {
                sum += samples[i] * samples[i - lag].conj();
            }
            sum / n as f32
        })
        .collect()
}

/// Spectral entropy (Shannon entropy of normalized power spectrum).
///
/// `H = -Σ p[k] · log₂(p[k])` where `p[k] = S[k] / Σ S[k]`
///
/// Range: 0 (pure tone, all energy in 1 bin) to log₂(N) (white noise).
///
/// Normalized spectral entropy: H / log₂(N) ∈ [0, 1]
/// 0 = maximally structured, 1 = maximally random.
pub fn spectral_entropy(spectrum_linear: &[f32]) -> (f32, f32) {
    let n = spectrum_linear.len();
    if n == 0 {
        return (0.0, 0.0);
    }

    let total: f64 = spectrum_linear.iter().map(|&s| s.max(1e-20) as f64).sum();

    let mut entropy = 0.0f64;
    for &s in spectrum_linear {
        let p = s.max(1e-20) as f64 / total;
        entropy -= p * p.log2();
    }

    let max_entropy = (n as f64).log2();
    let normalized = if max_entropy > 0.0 {
        entropy / max_entropy
    } else {
        0.0
    };

    (entropy as f32, normalized as f32)
}

/// Allan deviation (ADEV) for frequency stability analysis.
///
/// σ_y²(τ) = (1 / (2(M-1))) · Σ (ȳ_{i+1} - ȳ_i)²
///
/// where ȳ_i are the fractional frequency averages over interval τ.
///
/// The Allan deviation characterizes oscillator stability and noise type:
/// - White PM noise: σ_y ∝ τ^(-1)
/// - White FM noise: σ_y ∝ τ^(-1/2)
/// - Flicker FM noise: σ_y = constant
/// - Random walk FM: σ_y ∝ τ^(1/2)
///
/// `phase_samples`: instantaneous phase measurements
/// `sample_rate`: measurement rate in Hz
/// `tau_values`: averaging times in seconds
///
/// Returns (tau, adev) pairs.
pub fn allan_deviation(
    phase_samples: &[f64],
    sample_rate: f64,
    tau_values: &[f64],
) -> Vec<(f64, f64)> {
    let n = phase_samples.len();
    let mut results = Vec::with_capacity(tau_values.len());

    for &tau in tau_values {
        let m = (tau * sample_rate).round() as usize;
        if m == 0 || 2 * m >= n {
            continue;
        }

        // Compute fractional frequency averages
        let num_avgs = n - m;
        let freq_avgs: Vec<f64> = (0..num_avgs)
            .map(|i| (phase_samples[i + m] - phase_samples[i]) / (m as f64 / sample_rate))
            .collect();

        // Allan variance
        let num_diffs = freq_avgs.len() - 1;
        if num_diffs == 0 {
            continue;
        }

        let allan_var: f64 = freq_avgs
            .windows(2)
            .map(|w| {
                let diff = w[1] - w[0];
                diff * diff
            })
            .sum::<f64>()
            / (2.0 * num_diffs as f64);

        results.push((tau, allan_var.sqrt()));
    }

    results
}

/// Modified Allan deviation (MDEV).
///
/// Better discrimination between white PM and flicker PM noise than ADEV.
///
/// Uses second differences of phase with n-sample averaging:
/// Mod σ_y²(τ) = (1/(2n⁴τ₀²(N-3n+1))) · Σ_j Σ_i (Φ_{i+2n} - 2Φ_{i+n} + Φ_i)²
pub fn modified_allan_deviation(
    phase_samples: &[f64],
    sample_rate: f64,
    tau_values: &[f64],
) -> Vec<(f64, f64)> {
    let nn = phase_samples.len();
    let mut results = Vec::with_capacity(tau_values.len());

    for &tau in tau_values {
        let n = (tau * sample_rate).round() as usize;
        if n == 0 || 3 * n >= nn {
            continue;
        }

        let tau0 = 1.0 / sample_rate;
        let num_terms = nn - 3 * n + 1;
        if num_terms == 0 {
            continue;
        }

        let mut sum = 0.0f64;
        for j in 0..num_terms {
            let mut inner_sum = 0.0f64;
            for i in j..j + n {
                if i + 2 * n < nn {
                    inner_sum +=
                        phase_samples[i + 2 * n] - 2.0 * phase_samples[i + n] + phase_samples[i];
                }
            }
            sum += inner_sum * inner_sum;
        }

        let n4 = (n as f64).powi(4);
        let tau_sq = (n as f64 * tau0).powi(2);
        let mvar = sum / (2.0 * n4 * tau_sq * num_terms as f64);

        results.push((tau, mvar.sqrt()));
    }

    results
}

/// Phase noise measurement L(f).
///
/// Estimates single-sideband phase noise spectral density in dBc/Hz.
///
/// L(f) = S_φ(f) / 2 where S_φ is the power spectral density of
/// the phase fluctuations.
///
/// Input: instantaneous phase time series
/// Output: (offset_frequency_hz, phase_noise_dbc_hz) pairs
pub fn phase_noise(phase_samples: &[f64], sample_rate: f64, fft_size: usize) -> Vec<(f64, f64)> {
    let n = phase_samples.len();
    if n < fft_size {
        return Vec::new();
    }

    // Remove linear trend (carrier frequency)
    let mut detrended = phase_samples.to_vec();
    let slope = if n > 1 {
        (phase_samples[n - 1] - phase_samples[0]) / (n - 1) as f64
    } else {
        0.0
    };
    let intercept = phase_samples[0];
    for (i, v) in detrended.iter_mut().enumerate() {
        *v -= intercept + slope * i as f64;
    }

    // Welch's method on the phase residuals
    let overlap = fft_size / 2;
    let step = fft_size - overlap;
    let num_segments = (n.saturating_sub(fft_size)) / step + 1;

    if num_segments == 0 {
        return Vec::new();
    }

    let mut psd = vec![0.0f64; fft_size / 2 + 1];

    // Hann window
    let window: Vec<f64> = (0..fft_size)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (fft_size - 1) as f64).cos()))
        .collect();
    let window_power: f64 = window.iter().map(|w| w * w).sum::<f64>() / fft_size as f64;

    for seg in 0..num_segments {
        let start = seg * step;
        let segment = &detrended[start..start + fft_size];

        // Windowed DFT (real-valued input)
        #[allow(clippy::needless_range_loop)]
        for k in 0..=fft_size / 2 {
            let mut sum_re = 0.0f64;
            let mut sum_im = 0.0f64;
            for (i, &val) in segment.iter().enumerate() {
                let phase = -2.0 * PI * k as f64 * i as f64 / fft_size as f64;
                sum_re += val * window[i] * phase.cos();
                sum_im += val * window[i] * phase.sin();
            }
            let power = (sum_re * sum_re + sum_im * sum_im) / (fft_size as f64 * window_power);
            psd[k] += power;
        }
    }

    // Average and convert to L(f) in dBc/Hz
    let scale = 1.0 / (num_segments as f64 * sample_rate);
    let mut result = Vec::with_capacity(fft_size / 2);

    for (k, &psd_val) in psd.iter().enumerate().take(fft_size / 2 + 1).skip(1) {
        let freq = k as f64 * sample_rate / fft_size as f64;
        let l_f = 10.0 * (psd_val * scale / 2.0).max(1e-200).log10();
        result.push((freq, l_f));
    }

    result
}

/// Classify noise type from Allan deviation slope.
///
/// The slope of log(ADEV) vs log(τ) indicates the noise type:
/// - μ ≈ -1.0: White phase modulation (WPM)
/// - μ ≈ -0.5: White frequency modulation (WFM)
/// - μ ≈  0.0: Flicker frequency modulation (FFM)
/// - μ ≈ +0.5: Random walk frequency modulation (RWFM)
/// - μ ≈ +1.0: Frequency drift
#[derive(Debug, Clone)]
pub enum NoiseType {
    WhitePhase,
    WhiteFrequency,
    FlickerFrequency,
    RandomWalkFrequency,
    FrequencyDrift,
    Unknown,
}

pub fn classify_noise(adev_data: &[(f64, f64)]) -> (NoiseType, f64) {
    if adev_data.len() < 3 {
        return (NoiseType::Unknown, 0.0);
    }

    // Linear regression on log-log data
    let log_data: Vec<(f64, f64)> = adev_data
        .iter()
        .filter(|(tau, adev)| *tau > 0.0 && *adev > 0.0)
        .map(|(tau, adev)| (tau.log10(), adev.log10()))
        .collect();

    if log_data.len() < 2 {
        return (NoiseType::Unknown, 0.0);
    }

    let n = log_data.len() as f64;
    let sum_x: f64 = log_data.iter().map(|(x, _)| x).sum();
    let sum_y: f64 = log_data.iter().map(|(_, y)| y).sum();
    let sum_xy: f64 = log_data.iter().map(|(x, y)| x * y).sum();
    let sum_xx: f64 = log_data.iter().map(|(x, _)| x * x).sum();

    let slope = (n * sum_xy - sum_x * sum_y) / (n * sum_xx - sum_x * sum_x);

    let noise_type = if slope < -0.75 {
        NoiseType::WhitePhase
    } else if slope < -0.25 {
        NoiseType::WhiteFrequency
    } else if slope < 0.25 {
        NoiseType::FlickerFrequency
    } else if slope < 0.75 {
        NoiseType::RandomWalkFrequency
    } else {
        NoiseType::FrequencyDrift
    };

    (noise_type, slope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_stats_dc_signal() {
        let samples = vec![Sample::new(1.0, 0.0); 1000];
        let stats = signal_statistics(&samples);
        assert!((stats.mean.re - 1.0).abs() < 0.01);
        assert!(stats.variance < 0.01); // Should be near zero
    }

    #[test]
    fn autocorrelation_sinusoid() {
        let n = 1024;
        let samples: Vec<Sample> = (0..n)
            .map(|i| {
                let t = i as f32 / 1024.0;
                let phase = 2.0 * std::f32::consts::PI * 10.0 * t;
                Sample::new(phase.cos(), phase.sin())
            })
            .collect();

        let acf = autocorrelation(&samples, 100);

        // R[0] should be the signal power (~1.0)
        assert!((acf[0].re - 1.0).abs() < 0.05);

        // For a complex sinusoid, |R[τ]| should be constant
        for r in &acf {
            assert!(
                (r.norm_sqr().sqrt() - 1.0).abs() < 0.1,
                "ACF magnitude should be ~1.0: {}",
                r.norm_sqr().sqrt()
            );
        }
    }

    #[test]
    fn spectral_entropy_tone_vs_noise() {
        // Pure tone: low entropy
        let mut tone = vec![0.001f32; 256];
        tone[128] = 100.0;
        let (_, norm_tone) = spectral_entropy(&tone);

        // Flat noise: high entropy
        let noise = vec![1.0f32; 256];
        let (_, norm_noise) = spectral_entropy(&noise);

        assert!(
            norm_noise > norm_tone,
            "Noise entropy ({norm_noise}) should exceed tone ({norm_tone})"
        );
        assert!(
            norm_noise > 0.99,
            "Flat spectrum should be ~1.0: {norm_noise}"
        );
    }

    #[test]
    fn noise_classification_basic() {
        // Synthetic ADEV data for white frequency noise (slope = -0.5)
        let adev_data: Vec<(f64, f64)> = vec![
            (0.001, 1.0),
            (0.01, 0.316), // 10^(-0.5)
            (0.1, 0.1),
            (1.0, 0.0316),
        ];

        let (noise_type, slope) = classify_noise(&adev_data);
        assert!(
            matches!(noise_type, NoiseType::WhiteFrequency),
            "Expected WFM, got {noise_type:?} (slope={slope})"
        );
    }
}
