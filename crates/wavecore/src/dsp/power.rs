use crate::types::Sample;

/// Compute RMS power of samples in dBFS.
///
/// dBFS = 10 * log10(mean(|s|^2))
/// For a full-scale sinusoid (amplitude 1.0), this returns ~-3 dBFS.
pub fn rms_power_dbfs(samples: &[Sample]) -> f32 {
    if samples.is_empty() {
        return -200.0;
    }

    let mean_power: f32 = samples.iter().map(|s| s.norm_sqr()).sum::<f32>() / samples.len() as f32;

    if mean_power > 0.0 {
        10.0 * mean_power.log10()
    } else {
        -200.0
    }
}

/// Compute peak power of samples in dBFS.
///
/// dBFS = 10 * log10(max(|s|^2))
pub fn peak_power_dbfs(samples: &[Sample]) -> f32 {
    if samples.is_empty() {
        return -200.0;
    }

    let peak_power = samples.iter().map(|s| s.norm_sqr()).fold(0.0f32, f32::max);

    if peak_power > 0.0 {
        10.0 * peak_power.log10()
    } else {
        -200.0
    }
}

/// Find the frequency offset of the strongest signal in the spectrum.
///
/// Given a power spectrum (dBFS, DC-centered) and the sample rate,
/// returns the frequency offset from center in Hz and the power in dBFS.
pub fn peak_frequency(spectrum: &[f32], sample_rate: f64) -> (f64, f32) {
    if spectrum.is_empty() {
        return (0.0, -200.0);
    }

    let n = spectrum.len();
    let (peak_idx, &peak_power) = spectrum
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap();

    // DC-centered spectrum: bin N/2 is DC (0 Hz)
    // Frequency of bin i: (i - N/2) * fs / N
    let freq_offset = (peak_idx as f64 - n as f64 / 2.0) * sample_rate / n as f64;

    (freq_offset, peak_power)
}

/// Estimate the noise floor from a spectrum by taking the median power.
pub fn noise_floor(spectrum: &[f32]) -> f32 {
    if spectrum.is_empty() {
        return -200.0;
    }

    let mut sorted: Vec<f32> = spectrum.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    sorted[sorted.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    #[test]
    fn rms_power_fullscale_sine() {
        // Full-scale complex sinusoid: amplitude 1.0
        let samples: Vec<Sample> = (0..1024)
            .map(|i| {
                let phase = 2.0 * PI * i as f32 / 1024.0;
                Sample::new(phase.cos(), phase.sin())
            })
            .collect();

        let power = rms_power_dbfs(&samples);
        // |e^(j*theta)| = 1, so mean(|s|^2) = 1.0, so dBFS = 0
        assert!((power - 0.0).abs() < 0.1, "Expected ~0 dBFS, got {power}");
    }

    #[test]
    fn rms_power_half_scale() {
        let samples: Vec<Sample> = (0..1024)
            .map(|i| {
                let phase = 2.0 * PI * i as f32 / 1024.0;
                Sample::new(0.5 * phase.cos(), 0.5 * phase.sin())
            })
            .collect();

        let power = rms_power_dbfs(&samples);
        // amplitude 0.5 -> power = 0.25 -> dBFS = 10*log10(0.25) = -6.02
        assert!(
            (power - (-6.02)).abs() < 0.1,
            "Expected ~-6 dBFS, got {power}"
        );
    }

    #[test]
    fn peak_power_empty() {
        assert_eq!(peak_power_dbfs(&[]), -200.0);
        assert_eq!(rms_power_dbfs(&[]), -200.0);
    }

    #[test]
    fn peak_frequency_detection() {
        // Create a simple spectrum with a peak at a known offset
        let n = 256;
        let mut spectrum = vec![-100.0f32; n];
        // Put peak at bin 160 (offset from DC at 128: +32 bins)
        spectrum[160] = -10.0;

        let sample_rate = 2_048_000.0;
        let (freq, power) = peak_frequency(&spectrum, sample_rate);

        // Expected: (160 - 128) * 2048000 / 256 = 32 * 8000 = 256000 Hz
        assert!((freq - 256_000.0).abs() < 1.0);
        assert!((power - (-10.0)).abs() < 0.01);
    }

    #[test]
    fn noise_floor_estimation() {
        let spectrum = vec![-80.0, -82.0, -79.0, -10.0, -81.0, -78.0, -80.0];
        let floor = noise_floor(&spectrum);
        // Median of sorted: [-82, -81, -80, -80, -79, -78, -10] -> -80
        assert!((floor - (-80.0)).abs() < 0.01);
    }
}
