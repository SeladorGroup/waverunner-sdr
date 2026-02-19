//! Pulse and burst analysis for pulsed/intermittent signals.
//!
//! Detects bursts in IQ data by thresholding the power envelope,
//! then computes pulse width, pulse repetition interval (PRI),
//! duty cycle, and burst power statistics.

use crate::types::Sample;

/// Configuration for burst analysis.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BurstConfig {
    /// Threshold above noise floor to detect burst edges (dB).
    pub threshold_db: f32,
    /// Minimum burst duration in samples.
    pub min_burst_samples: usize,
    /// Sample rate for time conversion.
    pub sample_rate: f64,
}

/// Description of a single detected burst.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BurstDescriptor {
    /// Start sample index.
    pub start: usize,
    /// End sample index.
    pub end: usize,
    /// Duration in microseconds.
    pub duration_us: f64,
    /// Peak power in dBFS.
    pub peak_power_db: f32,
    /// Mean power in dBFS.
    pub mean_power_db: f32,
}

/// Burst analysis results.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BurstReport {
    /// Number of bursts detected.
    pub burst_count: usize,
    /// Individual burst descriptors.
    pub bursts: Vec<BurstDescriptor>,
    /// Mean pulse width in microseconds.
    pub mean_pulse_width_us: f64,
    /// Std deviation of pulse width in microseconds.
    pub pulse_width_std_us: f64,
    /// Mean Pulse Repetition Interval in microseconds.
    pub mean_pri_us: f64,
    /// Std deviation of PRI in microseconds.
    pub pri_std_us: f64,
    /// Duty cycle (fraction of time signal is on).
    pub duty_cycle: f32,
    /// Mean burst SNR in dB (burst power vs noise floor).
    pub mean_burst_snr_db: f32,
}

/// Detect bursts in IQ samples and compute timing statistics.
pub fn analyze_bursts(samples: &[Sample], config: &BurstConfig) -> BurstReport {
    if samples.is_empty() || config.sample_rate <= 0.0 {
        return empty_report();
    }

    // Compute power envelope in dB
    let envelope_db: Vec<f32> = samples
        .iter()
        .map(|s| {
            let pwr = s.re * s.re + s.im * s.im;
            if pwr > 0.0 {
                10.0 * pwr.log10()
            } else {
                -120.0
            }
        })
        .collect();

    // Estimate noise floor as median of envelope
    let noise_floor = median_f32(&envelope_db);
    let threshold = noise_floor + config.threshold_db;

    // Detect burst edges
    let mut bursts: Vec<BurstDescriptor> = Vec::new();
    let mut in_burst = false;
    let mut burst_start = 0usize;

    for (i, &db) in envelope_db.iter().enumerate() {
        if !in_burst && db >= threshold {
            in_burst = true;
            burst_start = i;
        } else if in_burst && (db < threshold || i == envelope_db.len() - 1) {
            let end = if db < threshold { i } else { i + 1 };
            let len = end - burst_start;
            if len >= config.min_burst_samples {
                let segment = &envelope_db[burst_start..end];
                let peak = segment.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                let mean_linear: f64 = segment
                    .iter()
                    .map(|&db| 10f64.powf(db as f64 / 10.0))
                    .sum::<f64>()
                    / segment.len() as f64;
                let mean_db = if mean_linear > 0.0 {
                    (10.0 * mean_linear.log10()) as f32
                } else {
                    -120.0
                };

                bursts.push(BurstDescriptor {
                    start: burst_start,
                    end,
                    duration_us: len as f64 / config.sample_rate * 1e6,
                    peak_power_db: peak,
                    mean_power_db: mean_db,
                });
            }
            in_burst = false;
        }
    }

    if bursts.is_empty() {
        return empty_report();
    }

    // Compute timing statistics
    let widths_us: Vec<f64> = bursts.iter().map(|b| b.duration_us).collect();
    let mean_pw = widths_us.iter().sum::<f64>() / widths_us.len() as f64;
    let pw_var = widths_us
        .iter()
        .map(|w| (w - mean_pw).powi(2))
        .sum::<f64>()
        / widths_us.len().max(1) as f64;
    let pw_std = pw_var.sqrt();

    // PRI: intervals between burst starts
    let pris: Vec<f64> = bursts
        .windows(2)
        .map(|w| (w[1].start - w[0].start) as f64 / config.sample_rate * 1e6)
        .collect();
    let (mean_pri, pri_std) = if pris.is_empty() {
        (0.0, 0.0)
    } else {
        let mean = pris.iter().sum::<f64>() / pris.len() as f64;
        let var = pris.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / pris.len() as f64;
        (mean, var.sqrt())
    };

    // Duty cycle
    let total_on: usize = bursts.iter().map(|b| b.end - b.start).sum();
    let duty = total_on as f32 / samples.len() as f32;

    // Mean burst SNR
    let mean_snr = bursts
        .iter()
        .map(|b| b.mean_power_db - noise_floor)
        .sum::<f32>()
        / bursts.len() as f32;

    BurstReport {
        burst_count: bursts.len(),
        bursts,
        mean_pulse_width_us: mean_pw,
        pulse_width_std_us: pw_std,
        mean_pri_us: mean_pri,
        pri_std_us: pri_std,
        duty_cycle: duty,
        mean_burst_snr_db: mean_snr,
    }
}

fn empty_report() -> BurstReport {
    BurstReport {
        burst_count: 0,
        bursts: Vec::new(),
        mean_pulse_width_us: 0.0,
        pulse_width_std_us: 0.0,
        mean_pri_us: 0.0,
        pri_std_us: 0.0,
        duty_cycle: 0.0,
        mean_burst_snr_db: 0.0,
    }
}

fn median_f32(data: &[f32]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f32> = data.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sorted[sorted.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_burst_signal(
        total_samples: usize,
        burst_ranges: &[(usize, usize)],
        burst_amplitude: f32,
        noise_amplitude: f32,
        sample_rate: f64,
    ) -> (Vec<Sample>, BurstConfig) {
        let mut samples = vec![Sample::new(noise_amplitude * 0.01, noise_amplitude * 0.01); total_samples];
        for &(start, end) in burst_ranges {
            for val in &mut samples[start..end.min(total_samples)] {
                *val = Sample::new(burst_amplitude, 0.0);
            }
        }
        let config = BurstConfig {
            threshold_db: 10.0,
            min_burst_samples: 5,
            sample_rate,
        };
        (samples, config)
    }

    #[test]
    fn detect_single_burst() {
        let (samples, config) = make_burst_signal(10000, &[(2000, 3000)], 1.0, 0.001, 1e6);
        let report = analyze_bursts(&samples, &config);
        assert_eq!(report.burst_count, 1);
        assert!((report.bursts[0].duration_us - 1000.0).abs() < 100.0);
    }

    #[test]
    fn detect_multiple_bursts() {
        let (samples, config) = make_burst_signal(
            20000,
            &[(2000, 3000), (5000, 6000), (8000, 9000)],
            1.0,
            0.001,
            1e6,
        );
        let report = analyze_bursts(&samples, &config);
        assert_eq!(report.burst_count, 3);
    }

    #[test]
    fn burst_timing_accuracy() {
        // Regular pulse train: 1000-sample bursts every 3000 samples
        let ranges: Vec<(usize, usize)> = (0..5).map(|i| (i * 3000, i * 3000 + 1000)).collect();
        let (samples, config) = make_burst_signal(20000, &ranges, 1.0, 0.001, 1e6);
        let report = analyze_bursts(&samples, &config);
        assert_eq!(report.burst_count, 5);
        // PRI should be ~3000 μs
        assert!(
            (report.mean_pri_us - 3000.0).abs() < 200.0,
            "Expected PRI ~3000 μs, got {}",
            report.mean_pri_us
        );
    }

    #[test]
    fn duty_cycle_calculation() {
        // ~33% duty cycle: 500 on, 1000 off, repeated — noise dominates median
        let ranges: Vec<(usize, usize)> = (0..10).map(|i| (i * 1500, i * 1500 + 500)).collect();
        let (samples, config) = make_burst_signal(15000, &ranges, 1.0, 0.001, 1e6);
        let report = analyze_bursts(&samples, &config);
        assert!(
            (report.duty_cycle - 0.333).abs() < 0.1,
            "Expected ~33% duty cycle, got {}",
            report.duty_cycle
        );
    }

    #[test]
    fn no_bursts_in_noise() {
        let samples = vec![Sample::new(0.001, 0.001); 10000];
        let config = BurstConfig {
            threshold_db: 10.0,
            min_burst_samples: 5,
            sample_rate: 1e6,
        };
        let report = analyze_bursts(&samples, &config);
        assert_eq!(report.burst_count, 0);
    }

    #[test]
    fn burst_at_boundary() {
        // Burst at the very end of the buffer
        let (samples, config) = make_burst_signal(10000, &[(9500, 10000)], 1.0, 0.001, 1e6);
        let report = analyze_bursts(&samples, &config);
        assert_eq!(report.burst_count, 1);
    }

    #[test]
    fn irregular_bursts() {
        // Varying PRI
        let (samples, config) = make_burst_signal(
            30000,
            &[(1000, 2000), (5000, 6000), (7000, 8000), (15000, 16000)],
            1.0,
            0.001,
            1e6,
        );
        let report = analyze_bursts(&samples, &config);
        assert_eq!(report.burst_count, 4);
        assert!(report.pri_std_us > 100.0, "Irregular bursts should have high PRI std");
    }

    #[test]
    fn burst_snr_measurement() {
        let (samples, config) = make_burst_signal(10000, &[(2000, 3000)], 1.0, 0.001, 1e6);
        let report = analyze_bursts(&samples, &config);
        assert!(report.mean_burst_snr_db > 20.0, "Burst SNR should be high, got {}", report.mean_burst_snr_db);
    }
}
