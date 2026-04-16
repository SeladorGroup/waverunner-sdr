//! RF measurement tools for signal analysis.
//!
//! Provides standard RF measurements: bandwidth (−3/−6 dB), occupied bandwidth,
//! channel power, adjacent channel power ratio (ACPR), and peak-to-average
//! power ratio (PAPR).

/// Configuration for signal measurement.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MeasureConfig {
    /// Center bin of the signal of interest (DC-centered spectrum index).
    pub signal_center_bin: usize,
    /// Approximate signal bandwidth in bins (for ACPR reference channel).
    pub signal_width_bins: usize,
    /// Number of adjacent channel bins for ACPR measurement.
    pub adjacent_width_bins: usize,
    /// Threshold below peak for occupied BW measurement in dB (e.g., 26.0 for 99% power).
    pub obw_threshold_db: f32,
}

/// RF measurement results.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MeasurementReport {
    /// −3 dB bandwidth in Hz.
    pub bandwidth_3db_hz: f64,
    /// −6 dB bandwidth in Hz.
    pub bandwidth_6db_hz: f64,
    /// Occupied bandwidth (power containment) in Hz.
    pub occupied_bw_hz: f64,
    /// Occupied BW power containment percentage.
    pub obw_percent: f32,
    /// Integrated channel power in dBFS.
    pub channel_power_dbfs: f32,
    /// Adjacent Channel Power Ratio — lower channel (dBc).
    pub acpr_lower_dbc: f32,
    /// Adjacent Channel Power Ratio — upper channel (dBc).
    pub acpr_upper_dbc: f32,
    /// Peak-to-Average Power Ratio in dB.
    pub papr_db: f32,
    /// Sub-bin frequency offset from center bin (Hz).
    pub freq_offset_hz: f64,
}

/// Compute −N dB bandwidth from a DC-centered power spectrum.
///
/// Finds the peak bin, then walks outward in both directions until the
/// power drops by `n_db` below the peak. Returns the bandwidth in Hz.
pub fn bandwidth_ndb(spectrum_db: &[f32], n_db: f32, sample_rate: f64) -> f64 {
    if spectrum_db.is_empty() {
        return 0.0;
    }

    let n = spectrum_db.len();
    let (peak_bin, peak_val) = spectrum_db
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap();

    let threshold = peak_val - n_db;

    // Walk left from peak
    let mut left = peak_bin;
    while left > 0 && spectrum_db[left] >= threshold {
        left -= 1;
    }
    // Interpolate left edge
    let left_frac = if left < peak_bin && spectrum_db[left] < threshold {
        let above = spectrum_db[left + 1];
        let below = spectrum_db[left];
        let frac = (above - threshold) / (above - below);
        left as f64 + (1.0 - frac as f64)
    } else {
        left as f64
    };

    // Walk right from peak
    let mut right = peak_bin;
    while right < n - 1 && spectrum_db[right] >= threshold {
        right += 1;
    }
    // Interpolate right edge
    let right_frac = if right > peak_bin && spectrum_db[right] < threshold {
        let above = spectrum_db[right - 1];
        let below = spectrum_db[right];
        let frac = (above - threshold) / (above - below);
        right as f64 - (1.0 - frac as f64)
    } else {
        right as f64
    };

    let bin_width_hz = sample_rate / n as f64;
    (right_frac - left_frac) * bin_width_hz
}

/// Compute occupied bandwidth — the bandwidth containing a given fraction of
/// total signal power, measured as threshold_db below total integrated power.
///
/// Returns `(occupied_bw_hz, percentage)`.
pub fn occupied_bandwidth(
    spectrum_db: &[f32],
    center_bin: usize,
    threshold_db: f32,
    sample_rate: f64,
) -> (f64, f32) {
    if spectrum_db.is_empty() {
        return (0.0, 0.0);
    }

    let n = spectrum_db.len();

    // Convert to linear power for integration
    let powers: Vec<f64> = spectrum_db
        .iter()
        .map(|&db| 10f64.powf(db as f64 / 10.0))
        .collect();
    let total_power: f64 = powers.iter().sum();

    if total_power <= 0.0 {
        return (0.0, 0.0);
    }

    // Threshold: fraction of total power to contain
    // threshold_db below total means we want (1 - 10^(-threshold_db/10)) of power
    let containment = 1.0 - 10f64.powf(-threshold_db as f64 / 10.0);
    let target_power = total_power * containment;

    // Expand symmetrically from center_bin until we contain target_power
    let center = center_bin.min(n - 1);
    let mut accumulated = powers[center];
    let mut left = center;
    let mut right = center;

    while accumulated < target_power && (left > 0 || right < n - 1) {
        // Expand whichever side has more power
        let left_power = if left > 0 { powers[left - 1] } else { 0.0 };
        let right_power = if right < n - 1 {
            powers[right + 1]
        } else {
            0.0
        };

        if left_power >= right_power && left > 0 {
            left -= 1;
            accumulated += powers[left];
        } else if right < n - 1 {
            right += 1;
            accumulated += powers[right];
        } else if left > 0 {
            left -= 1;
            accumulated += powers[left];
        } else {
            break;
        }
    }

    let bin_width_hz = sample_rate / n as f64;
    let bw_hz = (right - left + 1) as f64 * bin_width_hz;
    let percent = (accumulated / total_power * 100.0) as f32;

    (bw_hz, percent)
}

/// Compute integrated channel power over a range of bins.
///
/// Returns power in dBFS (sum of linear powers, converted back to dB).
pub fn channel_power(spectrum_db: &[f32], center_bin: usize, width_bins: usize) -> f32 {
    if spectrum_db.is_empty() || width_bins == 0 {
        return f32::NEG_INFINITY;
    }

    let n = spectrum_db.len();
    let half = width_bins / 2;
    let start = center_bin.saturating_sub(half);
    let end = (center_bin + half + 1).min(n);

    let linear_sum: f64 = spectrum_db[start..end]
        .iter()
        .map(|&db| 10f64.powf(db as f64 / 10.0))
        .sum();

    if linear_sum <= 0.0 {
        return f32::NEG_INFINITY;
    }

    (10.0 * linear_sum.log10()) as f32
}

/// Compute Adjacent Channel Power Ratio (ACPR).
///
/// Returns `(lower_acpr_dbc, upper_acpr_dbc)` — both values are in dBc
/// (negative means adjacent channel is below the main channel).
pub fn acpr(
    spectrum_db: &[f32],
    center_bin: usize,
    signal_width: usize,
    adjacent_width: usize,
) -> (f32, f32) {
    if spectrum_db.is_empty() || signal_width == 0 || adjacent_width == 0 {
        return (f32::NEG_INFINITY, f32::NEG_INFINITY);
    }

    let main_power = channel_power(spectrum_db, center_bin, signal_width);

    let n = spectrum_db.len();
    let half_sig = signal_width / 2;

    // Lower adjacent channel — center it just outside signal band (no overlap)
    let half_adj = adjacent_width / 2;
    let lower_center = center_bin.saturating_sub(half_sig + half_adj + 1);
    let lower_power = if lower_center >= half_adj {
        channel_power(spectrum_db, lower_center, adjacent_width)
    } else {
        channel_power(spectrum_db, half_adj, adjacent_width)
    };

    // Upper adjacent channel — center it just outside signal band (no overlap)
    let upper_center = (center_bin + half_sig + half_adj + 1).min(n - 1);
    let upper_power = if upper_center + half_adj < n {
        channel_power(spectrum_db, upper_center, adjacent_width)
    } else {
        channel_power(spectrum_db, n - 1 - half_adj, adjacent_width)
    };

    (lower_power - main_power, upper_power - main_power)
}

/// Compute Peak-to-Average Power Ratio (PAPR) from spectrum in dB.
pub fn papr(spectrum_db: &[f32]) -> f32 {
    if spectrum_db.is_empty() {
        return 0.0;
    }

    let powers: Vec<f64> = spectrum_db
        .iter()
        .map(|&db| 10f64.powf(db as f64 / 10.0))
        .collect();
    let avg = powers.iter().sum::<f64>() / powers.len() as f64;
    let peak = powers.iter().cloned().fold(0.0f64, f64::max);

    if avg <= 0.0 {
        return 0.0;
    }

    (10.0 * (peak / avg).log10()) as f32
}

/// Find the peak bin and estimate sub-bin frequency offset using parabolic interpolation.
pub fn peak_frequency_offset(spectrum_db: &[f32], sample_rate: f64) -> (usize, f64) {
    if spectrum_db.len() < 3 {
        return (0, 0.0);
    }

    let n = spectrum_db.len();
    let (peak_bin, _) = spectrum_db
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap();

    // Parabolic interpolation for sub-bin accuracy
    let offset_bins = if peak_bin > 0 && peak_bin < n - 1 {
        let alpha = spectrum_db[peak_bin - 1] as f64;
        let beta = spectrum_db[peak_bin] as f64;
        let gamma = spectrum_db[peak_bin + 1] as f64;
        let denom = 2.0 * (2.0 * beta - alpha - gamma);
        if denom.abs() > 1e-12 {
            (alpha - gamma) / denom
        } else {
            0.0
        }
    } else {
        0.0
    };

    let bin_width_hz = sample_rate / n as f64;
    (peak_bin, offset_bins * bin_width_hz)
}

/// Run full measurement suite on a power spectrum.
pub fn measure_signal(
    spectrum_db: &[f32],
    config: &MeasureConfig,
    sample_rate: f64,
) -> MeasurementReport {
    let bw_3db = bandwidth_ndb(spectrum_db, 3.0, sample_rate);
    let bw_6db = bandwidth_ndb(spectrum_db, 6.0, sample_rate);
    let (obw, obw_pct) = occupied_bandwidth(
        spectrum_db,
        config.signal_center_bin,
        config.obw_threshold_db,
        sample_rate,
    );
    let ch_power = channel_power(
        spectrum_db,
        config.signal_center_bin,
        config.signal_width_bins,
    );
    let (acpr_lo, acpr_hi) = acpr(
        spectrum_db,
        config.signal_center_bin,
        config.signal_width_bins,
        config.adjacent_width_bins,
    );
    let papr_val = papr(spectrum_db);
    let (_, freq_off) = peak_frequency_offset(spectrum_db, sample_rate);

    MeasurementReport {
        bandwidth_3db_hz: bw_3db,
        bandwidth_6db_hz: bw_6db,
        occupied_bw_hz: obw,
        obw_percent: obw_pct,
        channel_power_dbfs: ch_power,
        acpr_lower_dbc: acpr_lo,
        acpr_upper_dbc: acpr_hi,
        papr_db: papr_val,
        freq_offset_hz: freq_off,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a synthetic spectrum with a Gaussian-shaped signal at center.
    fn make_tone_spectrum(
        n: usize,
        center: usize,
        width_bins: usize,
        peak_db: f32,
        floor_db: f32,
    ) -> Vec<f32> {
        let mut spectrum = vec![floor_db; n];
        let sigma = width_bins as f32 / 4.0; // ~95% power within width_bins
        for (i, val) in spectrum.iter_mut().enumerate() {
            let dist = (i as f32 - center as f32).abs();
            // Gaussian shape
            *val = floor_db + (peak_db - floor_db) * (-0.5 * (dist / sigma).powi(2)).exp();
        }
        spectrum
    }

    #[test]
    fn bandwidth_3db_synthetic_tone() {
        let spectrum = make_tone_spectrum(1024, 512, 40, -10.0, -80.0);
        let bw = bandwidth_ndb(&spectrum, 3.0, 2.048e6);
        // Gaussian with sigma=10 bins, −3 dB BW ≈ 5.9 bins ≈ 11.8 kHz
        let bin_hz = 2.048e6 / 1024.0;
        assert!(bw > 2.0 * bin_hz, "BW {bw} should be > ~4 kHz");
        assert!(bw < 15.0 * bin_hz, "BW {bw} should be < ~30 kHz");
    }

    #[test]
    fn bandwidth_6db_wider_than_3db() {
        let spectrum = make_tone_spectrum(1024, 512, 40, -10.0, -80.0);
        let bw_3 = bandwidth_ndb(&spectrum, 3.0, 2.048e6);
        let bw_6 = bandwidth_ndb(&spectrum, 6.0, 2.048e6);
        assert!(
            bw_6 > bw_3,
            "6 dB BW ({bw_6}) must be wider than 3 dB BW ({bw_3})"
        );
    }

    #[test]
    fn occupied_bw_contains_most_power() {
        let spectrum = make_tone_spectrum(1024, 512, 40, -10.0, -80.0);
        let (obw, pct) = occupied_bandwidth(&spectrum, 512, 26.0, 2.048e6);
        assert!(obw > 0.0, "OBW should be positive");
        assert!(pct > 90.0, "Should contain >90% power, got {pct}%");
    }

    #[test]
    fn channel_power_known_level() {
        // Flat spectrum at −30 dBFS, 100 bins → power = −30 + 10*log10(100) = −10 dBFS
        let spectrum = vec![-30.0f32; 1024];
        let power = channel_power(&spectrum, 512, 100);
        let expected = -30.0 + 10.0 * (100.0f32).log10();
        assert!(
            (power - expected).abs() < 0.1,
            "Expected ~{expected}, got {power}"
        );
    }

    #[test]
    fn acpr_clean_signal() {
        // Strong center, weak adjacent
        let mut spectrum = vec![-80.0f32; 1024];
        for val in &mut spectrum[490..534] {
            *val = -20.0;
        }
        let (lo, hi) = acpr(&spectrum, 512, 44, 44);
        // Adjacent should be well below main channel
        assert!(lo < -30.0, "Lower ACPR {lo} should be < -30 dBc");
        assert!(hi < -30.0, "Upper ACPR {hi} should be < -30 dBc");
    }

    #[test]
    fn acpr_with_adjacent_energy() {
        let mut spectrum = vec![-80.0f32; 1024];
        // Main channel
        for val in &mut spectrum[490..534] {
            *val = -20.0;
        }
        // Adjacent energy (lower)
        for val in &mut spectrum[446..490] {
            *val = -40.0;
        }
        let (lo, _hi) = acpr(&spectrum, 512, 44, 44);
        // Lower adjacent is 20 dB below main
        assert!(
            lo > -25.0 && lo < -15.0,
            "Lower ACPR should be ~-20 dBc, got {lo}"
        );
    }

    #[test]
    fn papr_flat_spectrum() {
        // Flat spectrum → PAPR = 0 dB
        let spectrum = vec![-30.0f32; 1024];
        let p = papr(&spectrum);
        assert!(p.abs() < 0.1, "Flat spectrum PAPR should be ~0, got {p}");
    }

    #[test]
    fn papr_tone_in_noise() {
        let mut spectrum = vec![-80.0f32; 1024];
        spectrum[512] = -10.0;
        let p = papr(&spectrum);
        assert!(
            p > 20.0,
            "Single tone in noise should have high PAPR, got {p}"
        );
    }

    #[test]
    fn measure_empty_spectrum() {
        let report = measure_signal(
            &[],
            &MeasureConfig {
                signal_center_bin: 0,
                signal_width_bins: 10,
                adjacent_width_bins: 10,
                obw_threshold_db: 26.0,
            },
            2.048e6,
        );
        assert_eq!(report.bandwidth_3db_hz, 0.0);
    }

    #[test]
    fn peak_frequency_offset_centered() {
        // Symmetric peak → offset should be ~0
        let mut spectrum = vec![-80.0f32; 1024];
        spectrum[511] = -20.0;
        spectrum[512] = -10.0;
        spectrum[513] = -20.0;
        let (bin, offset) = peak_frequency_offset(&spectrum, 2.048e6);
        assert_eq!(bin, 512);
        assert!(
            offset.abs() < 100.0,
            "Centered peak should have ~0 offset, got {offset}"
        );
    }
}
