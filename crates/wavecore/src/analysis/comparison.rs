//! Spectrum comparison tools.
//!
//! Compare a captured reference spectrum against the current spectrum to
//! identify changes — new signals appearing, signals disappearing, or
//! overall spectral shape differences.

/// Configuration for spectrum comparison.
#[derive(Debug, Clone)]
pub struct CompareConfig {
    /// Reference spectrum (dBFS, DC-centered).
    pub reference: Vec<f32>,
    /// Current spectrum (dBFS, DC-centered).
    pub current: Vec<f32>,
    /// Sample rate for frequency axis conversion.
    pub sample_rate: f64,
    /// Threshold in dB for detecting new/lost signals.
    pub threshold_db: f32,
}

/// Spectrum comparison results.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ComparisonReport {
    /// Per-bin difference (current − reference) in dB.
    pub diff_db: Vec<f32>,
    /// RMS of the per-bin difference in dB.
    pub rms_diff_db: f32,
    /// Peak absolute difference in dB.
    pub peak_diff_db: f32,
    /// Bin index of peak difference.
    pub peak_diff_bin: usize,
    /// Pearson correlation coefficient (1.0 = identical shape).
    pub correlation: f32,
    /// Bins where current exceeds reference by >threshold (bin, excess_db).
    pub new_signals: Vec<(usize, f32)>,
    /// Bins where reference exceeded current by >threshold (bin, deficit_db).
    pub lost_signals: Vec<(usize, f32)>,
}

/// Compare two spectra and identify differences.
///
/// Both spectra must have the same length (same FFT size).
/// Returns a detailed comparison report.
pub fn compare_spectra(config: &CompareConfig) -> ComparisonReport {
    let ref_spec = &config.reference;
    let cur_spec = &config.current;

    if ref_spec.is_empty() || cur_spec.is_empty() {
        return ComparisonReport {
            diff_db: Vec::new(),
            rms_diff_db: 0.0,
            peak_diff_db: 0.0,
            peak_diff_bin: 0,
            correlation: 0.0,
            new_signals: Vec::new(),
            lost_signals: Vec::new(),
        };
    }

    let n = ref_spec.len().min(cur_spec.len());

    // Per-bin difference
    let diff_db: Vec<f32> = (0..n).map(|i| cur_spec[i] - ref_spec[i]).collect();

    // RMS difference
    let rms = if n > 0 {
        let sum_sq: f64 = diff_db.iter().map(|&d| (d as f64).powi(2)).sum();
        (sum_sq / n as f64).sqrt() as f32
    } else {
        0.0
    };

    // Peak difference
    let (peak_bin, peak_diff) = diff_db
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.abs().partial_cmp(&b.abs()).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, &v)| (i, v.abs()))
        .unwrap_or((0, 0.0));

    // Pearson correlation
    let corr = pearson_correlation(&ref_spec[..n], &cur_spec[..n]);

    // Detect new signals (current >> reference)
    let new_signals: Vec<(usize, f32)> = diff_db
        .iter()
        .enumerate()
        .filter(|(_, d)| **d > config.threshold_db)
        .map(|(i, d)| (i, *d))
        .collect();

    // Detect lost signals (reference >> current)
    let lost_signals: Vec<(usize, f32)> = diff_db
        .iter()
        .enumerate()
        .filter(|(_, d)| **d < -config.threshold_db)
        .map(|(i, d)| (i, -*d))
        .collect();

    ComparisonReport {
        diff_db,
        rms_diff_db: rms,
        peak_diff_db: peak_diff,
        peak_diff_bin: peak_bin,
        correlation: corr,
        new_signals,
        lost_signals,
    }
}

/// Clone a spectrum as a reference for later comparison.
pub fn capture_reference(spectrum_db: &[f32]) -> Vec<f32> {
    spectrum_db.to_vec()
}

/// Pearson correlation coefficient between two slices.
fn pearson_correlation(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }

    let mean_a: f64 = a[..n].iter().map(|&v| v as f64).sum::<f64>() / n as f64;
    let mean_b: f64 = b[..n].iter().map(|&v| v as f64).sum::<f64>() / n as f64;

    let mut cov = 0.0f64;
    let mut var_a = 0.0f64;
    let mut var_b = 0.0f64;

    for i in 0..n {
        let da = a[i] as f64 - mean_a;
        let db = b[i] as f64 - mean_b;
        cov += da * db;
        var_a += da * da;
        var_b += db * db;
    }

    let denom = (var_a * var_b).sqrt();
    if denom < 1e-12 {
        // Both slices have zero variance (constant) — if they are identical constants,
        // they are perfectly correlated.
        if var_a < 1e-12 && var_b < 1e-12 {
            return 1.0;
        }
        return 0.0;
    }

    (cov / denom) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_spectra() {
        let spec = vec![-40.0f32; 1024];
        let report = compare_spectra(&CompareConfig {
            reference: spec.clone(),
            current: spec,
            sample_rate: 2.048e6,
            threshold_db: 6.0,
        });
        assert!(report.rms_diff_db < 0.001);
        assert!((report.correlation - 1.0).abs() < 0.001);
        assert!(report.new_signals.is_empty());
        assert!(report.lost_signals.is_empty());
    }

    #[test]
    fn new_signal_detection() {
        let reference = vec![-80.0f32; 1024];
        let mut current = vec![-80.0f32; 1024];
        // New signal appears at bin 500
        for val in &mut current[495..505] {
            *val = -20.0;
        }
        let report = compare_spectra(&CompareConfig {
            reference,
            current,
            sample_rate: 2.048e6,
            threshold_db: 6.0,
        });
        assert!(!report.new_signals.is_empty(), "Should detect new signal");
        assert!(report.new_signals.iter().any(|(bin, _)| *bin >= 495 && *bin < 505));
    }

    #[test]
    fn lost_signal_detection() {
        let mut reference = vec![-80.0f32; 1024];
        for val in &mut reference[495..505] {
            *val = -20.0;
        }
        let current = vec![-80.0f32; 1024];
        let report = compare_spectra(&CompareConfig {
            reference,
            current,
            sample_rate: 2.048e6,
            threshold_db: 6.0,
        });
        assert!(!report.lost_signals.is_empty(), "Should detect lost signal");
    }

    #[test]
    fn rms_diff_known_value() {
        let reference = vec![-40.0f32; 100];
        let current = vec![-30.0f32; 100]; // 10 dB higher everywhere
        let report = compare_spectra(&CompareConfig {
            reference,
            current,
            sample_rate: 2.048e6,
            threshold_db: 6.0,
        });
        assert!((report.rms_diff_db - 10.0).abs() < 0.01, "RMS diff should be 10 dB, got {}", report.rms_diff_db);
    }

    #[test]
    fn correlation_anticorrelated() {
        let a: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let b: Vec<f32> = (0..100).map(|i| (99 - i) as f32).collect();
        let report = compare_spectra(&CompareConfig {
            reference: a,
            current: b,
            sample_rate: 2.048e6,
            threshold_db: 100.0, // high threshold so no new/lost signals
        });
        assert!(report.correlation < -0.9, "Anti-correlated should be negative, got {}", report.correlation);
    }
}
