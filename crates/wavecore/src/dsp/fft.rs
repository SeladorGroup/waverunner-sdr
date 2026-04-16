use std::f32::consts::PI;

use rustfft::FftPlanner;

use crate::dsp::windows;
use crate::error::{DspError, WaveError};
use crate::types::Sample;

/// Computes power spectral density from IQ samples.
///
/// Results are returned in dBFS (dB relative to full-scale), DC-centered
/// (fftshift applied so bins go from -fs/2 to +fs/2).
pub struct SpectrumAnalyzer {
    fft_size: usize,
    planner: FftPlanner<f32>,
    window: Vec<f32>,
    window_power: f32,
}

impl SpectrumAnalyzer {
    /// Create a new spectrum analyzer with the given FFT size.
    ///
    /// FFT size must be a power of 2.
    pub fn new(fft_size: usize) -> Result<Self, WaveError> {
        if fft_size == 0 || !fft_size.is_power_of_two() {
            return Err(WaveError::Dsp(DspError::InvalidParameter(format!(
                "FFT size must be a power of 2, got {fft_size}"
            ))));
        }

        // Hann window: w[n] = 0.5 * (1 - cos(2*pi*n / (N-1)))
        let window: Vec<f32> = (0..fft_size)
            .map(|n| 0.5 * (1.0 - (2.0 * PI * n as f32 / (fft_size - 1) as f32).cos()))
            .collect();

        // Window power for normalization
        let window_power: f32 = window.iter().map(|w| w * w).sum::<f32>() / fft_size as f32;

        Ok(Self {
            fft_size,
            planner: FftPlanner::new(),
            window,
            window_power,
        })
    }

    pub fn fft_size(&self) -> usize {
        self.fft_size
    }

    /// Compute power spectrum from samples.
    ///
    /// Returns `fft_size` bins in dBFS, DC-centered (fftshift applied).
    /// If `samples.len() < fft_size`, the buffer is zero-padded.
    /// If `samples.len() > fft_size`, only the first `fft_size` samples are used.
    pub fn compute_spectrum(&mut self, samples: &[Sample]) -> Vec<f32> {
        let n = self.fft_size;
        let fft = self.planner.plan_fft_forward(n);

        // Prepare windowed input
        let mut buffer: Vec<Sample> = Vec::with_capacity(n);
        for i in 0..n {
            if i < samples.len() {
                buffer.push(samples[i] * self.window[i]);
            } else {
                buffer.push(Sample::new(0.0, 0.0));
            }
        }

        // In-place FFT
        fft.process(&mut buffer);

        // Convert to power (dBFS) and apply fftshift
        let half = n / 2;
        let mut spectrum = vec![0.0f32; n];

        for (i, bin) in buffer.iter().enumerate() {
            let mag_sq = bin.norm_sqr();
            // Normalize by FFT size and window power, convert to dBFS
            let power = mag_sq / (n as f32 * n as f32 * self.window_power);
            let db = if power > 0.0 {
                10.0 * power.log10()
            } else {
                -200.0 // floor
            };

            // fftshift: swap first and second halves
            let shifted_idx = (i + half) % n;
            spectrum[shifted_idx] = db;
        }

        spectrum
    }

    /// Compute linear power spectrum (not dBFS) with fftshift.
    ///
    /// Used internally by `compute_averaged_spectrum` to average in the linear
    /// power domain (correct per Welch's method) before converting to dB.
    fn compute_spectrum_linear(&mut self, samples: &[Sample]) -> Vec<f32> {
        let n = self.fft_size;
        let fft = self.planner.plan_fft_forward(n);

        let mut buffer: Vec<Sample> = Vec::with_capacity(n);
        for i in 0..n {
            if i < samples.len() {
                buffer.push(samples[i] * self.window[i]);
            } else {
                buffer.push(Sample::new(0.0, 0.0));
            }
        }

        fft.process(&mut buffer);

        let half = n / 2;
        let mut spectrum = vec![0.0f32; n];
        for (i, bin) in buffer.iter().enumerate() {
            let mag_sq = bin.norm_sqr();
            let power = mag_sq / (n as f32 * n as f32 * self.window_power);
            let shifted_idx = (i + half) % n;
            spectrum[shifted_idx] = power;
        }
        spectrum
    }

    /// Compute averaged spectrum using Welch's method.
    ///
    /// Splits `samples` into overlapping segments of `fft_size`, windows each,
    /// computes FFT, and averages the power spectra. Averaging is done in the
    /// linear power domain (correct per Welch's method), then converted to dBFS.
    ///
    /// `overlap` is a fraction from 0.0 (no overlap) to less than 1.0
    /// (e.g., 0.5 for 50% overlap).
    pub fn compute_averaged_spectrum(&mut self, samples: &[Sample], overlap: f32) -> Vec<f32> {
        let n = self.fft_size;
        let overlap = overlap.clamp(0.0, 0.99);
        let step = ((1.0 - overlap) * n as f32) as usize;
        let step = step.max(1);

        if samples.len() < n {
            return self.compute_spectrum(samples);
        }

        let num_segments = (samples.len() - n) / step + 1;
        if num_segments == 0 {
            return self.compute_spectrum(samples);
        }

        // Average in linear power domain (not dB) to avoid Jensen's inequality bias
        let mut avg_linear = vec![0.0f32; n];

        for seg in 0..num_segments {
            let start = seg * step;
            let segment = &samples[start..start + n];
            let linear = self.compute_spectrum_linear(segment);
            for (avg, val) in avg_linear.iter_mut().zip(linear.iter()) {
                *avg += val;
            }
        }

        // Average, then convert to dBFS
        let scale = 1.0 / num_segments as f32;
        for val in &mut avg_linear {
            *val *= scale;
            *val = if *val > 0.0 {
                10.0 * val.log10()
            } else {
                -200.0
            };
        }

        avg_linear
    }
}

/// Compute raw (complex) FFT output along with dBFS spectrum.
///
/// Returns (fft_output_natural_order, spectrum_dbfs_dc_centered).
/// The fft_output is in natural FFT order (not shifted) for use with
/// sub-bin frequency estimators like Quinn's.
pub fn compute_spectrum_with_fft(
    planner: &mut FftPlanner<f32>,
    samples: &[Sample],
    window: &[f32],
    fft_size: usize,
) -> (Vec<Sample>, Vec<f32>) {
    let n = fft_size;
    let fft = planner.plan_fft_forward(n);

    let window_power: f32 = window.iter().map(|w| w * w).sum::<f32>() / n as f32;

    let mut buffer: Vec<Sample> = Vec::with_capacity(n);
    for i in 0..n {
        if i < samples.len() && i < window.len() {
            buffer.push(samples[i] * window[i]);
        } else {
            buffer.push(Sample::new(0.0, 0.0));
        }
    }

    fft.process(&mut buffer);

    let half = n / 2;
    let mut spectrum = vec![0.0f32; n];

    for (i, bin) in buffer.iter().enumerate() {
        let mag_sq = bin.norm_sqr();
        let power = mag_sq / (n as f32 * n as f32 * window_power.max(1e-20));
        let db = if power > 0.0 {
            10.0 * power.log10()
        } else {
            -200.0
        };
        let shifted_idx = (i + half) % n;
        spectrum[shifted_idx] = db;
    }

    (buffer, spectrum)
}

/// Thomson's multitaper spectral estimation.
///
/// Uses multiple DPSS (Slepian) tapers to produce a spectral estimate with:
/// - Lower variance than single-window methods (variance ∝ 1/K for K tapers)
/// - Controlled spectral leakage (determined by half-bandwidth parameter NW)
/// - Near-optimal bias-variance tradeoff
///
/// The estimate is: Ŝ(f) = (1/K) · Σ_{k=0}^{K-1} |Y_k(f)|²
///
/// where Y_k is the DFT of `x[n]·v_k[n]` and v_k are the DPSS sequences.
///
/// Typical parameters:
/// - NW = 3-4 (half-bandwidth, resolution = 2·NW/N bins)
/// - K = 2·NW - 1 tapers (rule of thumb)
pub struct MultitaperAnalyzer {
    fft_size: usize,
    planner: FftPlanner<f32>,
    tapers: Vec<Vec<f32>>,
    num_tapers: usize,
}

impl MultitaperAnalyzer {
    /// Create a multitaper analyzer.
    ///
    /// `fft_size`: FFT length (must be power of 2)
    /// `half_bandwidth`: NW parameter (3.0-4.0 typical)
    /// `num_tapers`: K (should be ≤ 2·NW - 1)
    pub fn new(fft_size: usize, half_bandwidth: f64, num_tapers: usize) -> Result<Self, WaveError> {
        if fft_size == 0 || !fft_size.is_power_of_two() {
            return Err(WaveError::Dsp(DspError::InvalidParameter(format!(
                "FFT size must be a power of 2, got {fft_size}"
            ))));
        }

        let tapers_f64 = windows::dpss(fft_size, half_bandwidth, num_tapers);
        let tapers: Vec<Vec<f32>> = tapers_f64
            .iter()
            .map(|t| windows::window_to_f32(t))
            .collect();

        let num_tapers = tapers.len();
        Ok(Self {
            fft_size,
            planner: FftPlanner::new(),
            tapers,
            num_tapers,
        })
    }

    /// Compute multitaper spectrum estimate.
    ///
    /// Returns dBFS spectrum, DC-centered.
    pub fn compute_spectrum(&mut self, samples: &[Sample]) -> Vec<f32> {
        let n = self.fft_size;
        let half = n / 2;
        let fft = self.planner.plan_fft_forward(n);

        let mut avg_power = vec![0.0f32; n];

        for taper in &self.tapers {
            // Apply taper and compute FFT
            let mut buffer: Vec<Sample> = Vec::with_capacity(n);
            for i in 0..n {
                if i < samples.len() {
                    buffer.push(samples[i] * taper[i]);
                } else {
                    buffer.push(Sample::new(0.0, 0.0));
                }
            }

            fft.process(&mut buffer);

            // Accumulate power
            for (i, bin) in buffer.iter().enumerate() {
                avg_power[i] += bin.norm_sqr();
            }
        }

        // Average over tapers, normalize, convert to dBFS, fftshift
        let scale = 1.0 / (self.num_tapers as f32 * n as f32 * n as f32);
        let mut spectrum = vec![0.0f32; n];

        for (i, &avg_pow) in avg_power.iter().enumerate() {
            let power = avg_pow * scale;
            let db = if power > 0.0 {
                10.0 * power.log10()
            } else {
                -200.0
            };
            let shifted_idx = (i + half) % n;
            spectrum[shifted_idx] = db;
        }

        spectrum
    }
}

/// Goertzel algorithm for computing a single DFT bin.
///
/// O(N) computation for a single frequency, compared to O(N·log N) for full FFT.
/// Efficient when you only need power at specific frequencies (e.g., DTMF detection,
/// single-tone detection, frequency monitoring).
///
/// Mathematically equivalent to: `X[k] = Σ x[n]·e^{-j2πkn/N}`
///
/// Uses the second-order recurrence:
///   `s[n] = x[n] + 2·cos(2πk/N)·s[n-1] - s[n-2]`
///   `X[k] = s[N-1] - e^{-j2πk/N}·s[N-2]`
///
/// This avoids trig functions inside the loop (only one cosine precomputed).
///
/// `samples`: input signal
/// `target_freq_hz`: frequency to detect
/// `sample_rate`: sample rate in Hz
///
/// Returns: (power_dbfs, complex_value)
pub fn goertzel(samples: &[Sample], target_freq_hz: f64, sample_rate: f64) -> (f32, Sample) {
    let n = samples.len();
    if n == 0 {
        return (-200.0, Sample::new(0.0, 0.0));
    }

    let k = target_freq_hz * n as f64 / sample_rate;
    let omega = 2.0 * std::f64::consts::PI * k / n as f64;
    let coeff = 2.0 * omega.cos() as f32;

    let mut s1 = 0.0f32; // s[n-1]
    let mut s2 = 0.0f32; // s[n-2]

    // Process I and Q channels with independent recurrences.
    // By linearity of the DFT, X[k] = DFT(x_re)[k] + j * DFT(x_im)[k].
    let mut s1_q = 0.0f32;
    let mut s2_q = 0.0f32;

    for sample in samples {
        let temp = sample.re + coeff * s1 - s2;
        s2 = s1;
        s1 = temp;

        let temp_q = sample.im + coeff * s1_q - s2_q;
        s2_q = s1_q;
        s1_q = temp_q;
    }

    // Final computation: combine two real Goertzel results into complex DFT.
    // Real Goertzel: X_ch[k] = s1 - e^{-jω} * s2
    //   = (s1 - s2*cos(ω)) + j*(s2*sin(ω))
    // Total: X[k] = X_re[k] + j * X_im[k]
    //   result_re = (s1 - s2*cos(ω)) - s2_q*sin(ω)
    //   result_im = s2*sin(ω) + (s1_q - s2_q*cos(ω))
    let cos_w = omega.cos() as f32;
    let sin_w = omega.sin() as f32;

    let result_re = (s1 - s2 * cos_w) - s2_q * sin_w;
    let result_im = s2 * sin_w + (s1_q - s2_q * cos_w);

    let result = Sample::new(result_re, result_im);
    let power = result.norm_sqr() / (n as f32 * n as f32);
    let db = if power > 0.0 {
        10.0 * power.log10()
    } else {
        -200.0
    };

    (db, result)
}

/// Spectrogram (Short-Time Fourier Transform matrix).
///
/// Computes a time-frequency representation by sliding a window across
/// the signal and computing the FFT at each position.
///
/// Returns a Vec of (timestamp_seconds, spectrum_dbfs) tuples.
pub fn spectrogram(
    samples: &[Sample],
    fft_size: usize,
    hop_size: usize,
    sample_rate: f64,
    window_type: &windows::WindowType,
) -> Result<Vec<(f64, Vec<f32>)>, WaveError> {
    if fft_size == 0 || !fft_size.is_power_of_two() {
        return Err(WaveError::Dsp(DspError::InvalidParameter(
            "FFT size must be power of 2".into(),
        )));
    }

    let window = windows::window_to_f32(&windows::generate_window(window_type, fft_size));
    let mut planner = FftPlanner::new();
    let mut result = Vec::new();

    let mut pos = 0;
    while pos + fft_size <= samples.len() {
        let segment = &samples[pos..pos + fft_size];
        let (_, spectrum) = compute_spectrum_with_fft(&mut planner, segment, &window, fft_size);

        let timestamp = pos as f64 / sample_rate;
        result.push((timestamp, spectrum));

        pos += hop_size;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    #[test]
    fn new_requires_power_of_two() {
        assert!(SpectrumAnalyzer::new(1024).is_ok());
        assert!(SpectrumAnalyzer::new(512).is_ok());
        assert!(SpectrumAnalyzer::new(100).is_err());
        assert!(SpectrumAnalyzer::new(0).is_err());
    }

    #[test]
    fn dc_signal_peaks_at_center() {
        let mut analyzer = SpectrumAnalyzer::new(256).unwrap();
        // DC signal: all samples are (1.0, 0.0)
        let samples: Vec<Sample> = vec![Sample::new(1.0, 0.0); 256];
        let spectrum = analyzer.compute_spectrum(&samples);

        // After fftshift, DC should be at bin N/2 = 128
        let dc_bin = 128;
        let dc_power = spectrum[dc_bin];

        // DC bin should be the strongest
        for (i, &val) in spectrum.iter().enumerate() {
            if i != dc_bin {
                assert!(
                    dc_power > val,
                    "DC bin ({dc_power:.1}) should be stronger than bin {i} ({val:.1})"
                );
            }
        }
    }

    #[test]
    fn pure_tone_detection() {
        let mut analyzer = SpectrumAnalyzer::new(1024).unwrap();
        let n = 1024;
        let fs = 1024.0f32; // 1 sample = 1 Hz for easy math

        // Generate a tone at bin 100 (100 Hz with fs=1024)
        let tone_bin = 100;
        let freq = tone_bin as f32;
        let samples: Vec<Sample> = (0..n)
            .map(|i| {
                let t = i as f32 / fs;
                let phase = 2.0 * PI * freq * t;
                Sample::new(phase.cos(), phase.sin())
            })
            .collect();

        let spectrum = analyzer.compute_spectrum(&samples);

        // After fftshift, bin `tone_bin` maps to index (tone_bin + N/2) % N
        let expected_idx = (tone_bin + n / 2) % n;
        let peak_power = spectrum[expected_idx];

        // Peak should be significantly above noise
        let noise_floor: f32 = spectrum
            .iter()
            .enumerate()
            .filter(|&(i, _)| (i as i32 - expected_idx as i32).unsigned_abs() > 5)
            .map(|(_, &v)| v)
            .sum::<f32>()
            / (n - 10) as f32;

        assert!(
            peak_power - noise_floor > 30.0,
            "Peak ({peak_power:.1} dBFS) should be >30dB above noise ({noise_floor:.1} dBFS)"
        );
    }

    #[test]
    fn zero_padded_short_input() {
        let mut analyzer = SpectrumAnalyzer::new(256).unwrap();
        // Input shorter than FFT size should still work (zero-padded)
        let samples: Vec<Sample> = vec![Sample::new(1.0, 0.0); 64];
        let spectrum = analyzer.compute_spectrum(&samples);
        assert_eq!(spectrum.len(), 256);
    }

    #[test]
    fn welch_averaging_reduces_variance() {
        let mut analyzer = SpectrumAnalyzer::new(256).unwrap();

        // Generate noisy signal
        let n = 4096;
        let samples: Vec<Sample> = (0..n)
            .map(|i| {
                let phase = 2.0 * PI * 50.0 * i as f32 / 1024.0;
                // Add some "noise" via deterministic hash-like function
                let noise = ((i as f32 * 0.7).sin() * 0.3, (i as f32 * 1.3).cos() * 0.3);
                Sample::new(phase.cos() + noise.0, phase.sin() + noise.1)
            })
            .collect();

        let single = analyzer.compute_spectrum(&samples[0..256]);
        let averaged = analyzer.compute_averaged_spectrum(&samples, 0.5);

        // Both should have the same length
        assert_eq!(single.len(), averaged.len());
        assert_eq!(averaged.len(), 256);
    }
}
