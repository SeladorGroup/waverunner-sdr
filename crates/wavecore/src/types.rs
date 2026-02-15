use num_complex::Complex;
use serde::{Deserialize, Serialize};

/// Canonical IQ sample type. f32 gives sufficient precision for SDR
/// work while being cache-friendly and FFT-optimal.
pub type Sample = Complex<f32>;

/// Frequency in Hz, stored as f64 for sub-Hz precision across
/// the full SDR range (kHz to GHz).
pub type Frequency = f64;

/// Sample rate in samples per second.
pub type SampleRate = f64;

/// A timestamped block of IQ samples flowing through the pipeline.
#[derive(Debug, Clone)]
pub struct SampleBlock {
    pub samples: Vec<Sample>,
    /// Center frequency at time of capture (Hz).
    pub center_freq: Frequency,
    /// Sample rate at time of capture (S/s).
    pub sample_rate: SampleRate,
    /// Monotonic sequence number for ordering.
    pub sequence: u64,
    /// Timestamp in nanoseconds (monotonic clock or device timestamp).
    pub timestamp_ns: u64,
}

/// Device capability description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub driver: String,
    pub serial: Option<String>,
    pub index: u32,
    pub frequency_range: (Frequency, Frequency),
    pub sample_rate_range: (SampleRate, SampleRate),
    pub gain_range: (f64, f64),
    pub available_gains: Vec<f64>,
}

/// Convert raw unsigned 8-bit RTL-SDR I/Q bytes to normalized Complex<f32>.
///
/// RTL-SDR produces interleaved `[I0, Q0, I1, Q1, ...]` where each byte
/// is unsigned 0-255 with 127.5 as the center point.
#[inline]
pub fn u8_iq_to_samples(raw: &[u8]) -> Vec<Sample> {
    debug_assert!(raw.len() % 2 == 0, "Raw IQ data must have even length");
    raw.chunks_exact(2)
        .map(|pair| {
            Sample::new(
                (pair[0] as f32 - 127.5) / 127.5,
                (pair[1] as f32 - 127.5) / 127.5,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u8_iq_conversion_center() {
        // Values near 127-128 should produce near-zero samples
        let raw = [127u8, 128u8];
        let samples = u8_iq_to_samples(&raw);
        assert_eq!(samples.len(), 1);
        assert!(samples[0].re.abs() < 0.01);
        assert!(samples[0].im.abs() < 0.01);
    }

    #[test]
    fn u8_iq_conversion_extremes() {
        let raw = [0u8, 255u8];
        let samples = u8_iq_to_samples(&raw);
        assert!((samples[0].re - (-1.0)).abs() < 0.01);
        assert!((samples[0].im - 1.0).abs() < 0.01);
    }

    #[test]
    fn u8_iq_batch_conversion() {
        let raw = vec![128u8; 2048];
        let samples = u8_iq_to_samples(&raw);
        assert_eq!(samples.len(), 1024);
    }

    #[test]
    fn u8_iq_empty() {
        let raw: &[u8] = &[];
        let samples = u8_iq_to_samples(raw);
        assert!(samples.is_empty());
    }
}
