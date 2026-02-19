//! NOAA APT (Automatic Picture Transmission) Decoder
//!
//! Decodes satellite weather images from NOAA polar-orbiting satellites
//! (NOAA-15, NOAA-18, NOAA-19). APT transmits two image channels at
//! 2 lines per second using a 2400 Hz AM subcarrier over FM.
//!
//! ## Frequencies
//!
//! - NOAA-15: 137.620 MHz
//! - NOAA-18: 137.9125 MHz
//! - NOAA-19: 137.100 MHz
//!
//! ## Signal Structure
//!
//! ```text
//! FM carrier → 2400 Hz AM subcarrier → Image data
//!
//! Line format (4160 samples @ 4160 Hz, 0.5s/line):
//! ┌─────────┬───────┬──────────┬──────────┬─────────┬───────┬──────────┬──────────┐
//! │ Sync A  │ Space │ Image A  │ Telem A  │ Sync B  │ Space │ Image B  │ Telem B  │
//! │ 39 px   │ 47 px │ 909 px   │ 45 px    │ 39 px   │ 47 px │ 909 px   │ 45 px    │
//! └─────────┴───────┴──────────┴──────────┴─────────┴───────┴──────────┴──────────┘
//! Total: 2080 samples per half-line, 4160 per full line
//! ```
//!
//! ## Signal Processing Chain
//!
//! ```text
//! IQ → FM discriminator → Bandpass 2400 Hz → AM envelope
//!   → Resample to 4160 Hz → Sync correlator → Pixel extraction
//!   → Line assembly → Image output
//! ```

use std::collections::BTreeMap;
use std::f64::consts::PI;
use std::time::Instant;

use crate::dsp::decoder::{DecoderPlugin, DecoderRequirements};
use crate::session::DecodedMessage;
use crate::types::Sample;

use super::util;

// ============================================================================
// Constants
// ============================================================================

/// NOAA-15 frequency.
const NOAA15_FREQ: f64 = 137.62e6;
/// NOAA-18 frequency.
const NOAA18_FREQ: f64 = 137.9125e6;
/// NOAA-19 frequency.
const NOAA19_FREQ: f64 = 137.1e6;

/// APT line rate (lines per second).
const LINE_RATE: f64 = 2.0;
/// Samples per APT line at the working sample rate.
const SAMPLES_PER_LINE: usize = 2080;
/// Full line (both channels A and B).
const FULL_LINE_SAMPLES: usize = 4160;
/// APT working sample rate (4160 samples/line × 2 lines/sec).
const APT_SAMPLE_RATE: f64 = (FULL_LINE_SAMPLES as f64) * LINE_RATE;
/// Subcarrier frequency.
const SUBCARRIER_FREQ: f64 = 2400.0;

/// Sync A pattern: 7 cycles of 1040 Hz at 4160 Hz sample rate
/// (4160/1040 = 4 samples/cycle × 7 cycles = 28 samples of useful sync)
const SYNC_A_FREQ: f64 = 1040.0;
/// Sync B pattern: 7 cycles of 832 Hz
const SYNC_B_FREQ: f64 = 832.0;
/// Number of sync cycles.
const SYNC_CYCLES: usize = 7;

/// Channel A image width.
const IMAGE_A_WIDTH: usize = 909;
/// Channel B image width.
const IMAGE_B_WIDTH: usize = 909;
/// Sync width in samples.
const SYNC_WIDTH: usize = 39;
/// Space width in samples.
const SPACE_WIDTH: usize = 47;
/// Telemetry width in samples.
#[cfg(test)]
const TELEMETRY_WIDTH: usize = 45;

// ============================================================================
// Sync Correlator
// ============================================================================

/// Cross-correlator for APT sync pattern detection.
struct SyncCorrelator {
    /// Reference waveform (normalized).
    reference: Vec<f32>,
    /// Correlation threshold.
    threshold: f32,
}

impl SyncCorrelator {
    /// Create a sync correlator for the given frequency.
    fn new(freq: f64, sample_rate: f64) -> Self {
        let samples_per_cycle = (sample_rate / freq) as usize;
        let total_samples = samples_per_cycle * SYNC_CYCLES;

        let mut reference = Vec::with_capacity(total_samples);
        let mut energy = 0.0f64;

        for i in 0..total_samples {
            let t = i as f64 / sample_rate;
            let val = (2.0 * PI * freq * t).sin();
            reference.push(val as f32);
            energy += val * val;
        }

        // Normalize
        let norm = energy.sqrt() as f32;
        if norm > 0.0 {
            for r in &mut reference {
                *r /= norm;
            }
        }

        Self {
            reference,
            threshold: 0.4, // Correlation threshold (0-1)
        }
    }

    /// Compute normalized cross-correlation at the given offset.
    ///
    /// Returns the correlation value (0-1) if it exceeds the threshold.
    fn correlate(&self, data: &[f32], offset: usize) -> Option<f32> {
        if offset + self.reference.len() > data.len() {
            return None;
        }

        let mut cross = 0.0f32;
        let mut signal_energy = 0.0f32;

        for (i, &ref_val) in self.reference.iter().enumerate() {
            let s = data[offset + i];
            cross += s * ref_val;
            signal_energy += s * s;
        }

        if signal_energy < 1e-10 {
            return None;
        }

        // Normalized correlation
        let norm_corr = cross / signal_energy.sqrt();

        if norm_corr.abs() > self.threshold {
            Some(norm_corr.abs())
        } else {
            None
        }
    }
}

// ============================================================================
// NOAA APT Decoder
// ============================================================================

/// NOAA APT satellite image decoder.
///
/// Decodes weather satellite images from FM-modulated APT signals.
/// Emits one DecodedMessage per image line, containing pixel data
/// for both channels.
pub struct NoaaAptDecoder {
    sample_rate: f64,
    satellite: String,
    satellite_freq: f64,
    /// Previous IQ sample for FM discriminator.
    prev_iq: Sample,
    /// DC removal state.
    dc_state: f64,
    dc_alpha: f64,
    /// 2400 Hz bandpass filter state (biquad).
    bpf_state: [f64; 4],
    bpf_coeffs: BiquadCoeffs,
    /// AM envelope lowpass filter state.
    env_lpf_state: f64,
    env_lpf_alpha: f64,
    /// Resampling state (from input sample rate to 4160 Hz).
    resample_phase: f64,
    resample_step: f64,
    /// Line buffer (accumulates samples at APT rate).
    line_buffer: Vec<f32>,
    /// Sync correlators.
    sync_a: SyncCorrelator,
    sync_b: SyncCorrelator,
    /// Line counter.
    line_count: usize,
    /// Decoder name.
    decoder_name: String,
}

/// Biquad filter coefficients.
#[derive(Clone)]
struct BiquadCoeffs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl BiquadCoeffs {
    /// Design a bandpass biquad filter.
    fn bandpass(center_freq: f64, bandwidth: f64, sample_rate: f64) -> Self {
        let w0 = 2.0 * PI * center_freq / sample_rate;
        let q = center_freq / bandwidth;
        let alpha = w0.sin() / (2.0 * q);

        let b0 = alpha;
        let b1 = 0.0;
        let b2 = -alpha;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * w0.cos();
        let a2 = 1.0 - alpha;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }
}

impl NoaaAptDecoder {
    /// Create a new NOAA APT decoder.
    ///
    /// `sample_rate`: input IQ sample rate (must be >= 8320 Hz for Nyquist)
    /// `satellite`: "NOAA-15", "NOAA-18", or "NOAA-19"
    pub fn new(sample_rate: f64, satellite: &str) -> Self {
        let satellite_freq = match satellite {
            "NOAA-15" => NOAA15_FREQ,
            "NOAA-18" => NOAA18_FREQ,
            _ => NOAA19_FREQ,
        };

        let decoder_name = match satellite {
            "NOAA-15" => "noaa-apt-15".to_string(),
            "NOAA-18" => "noaa-apt-18".to_string(),
            _ => "noaa-apt-19".to_string(),
        };

        let dc_alpha = 1.0 - (-1.0 / (0.005 * sample_rate)).exp();

        // Bandpass filter centered at 2400 Hz, bandwidth ~200 Hz
        let bpf_coeffs = BiquadCoeffs::bandpass(SUBCARRIER_FREQ, 400.0, sample_rate);

        // Envelope lowpass: cutoff at ~4200 Hz (just above APT sample rate)
        let env_tau = 1.0 / (2.0 * PI * 4200.0);
        let env_dt = 1.0 / sample_rate;
        let env_lpf_alpha = env_dt / (env_tau + env_dt);

        // Resampling: convert from input sample rate to APT_SAMPLE_RATE (8320 Hz)
        let resample_step = APT_SAMPLE_RATE / sample_rate;

        Self {
            sample_rate,
            satellite: satellite.to_string(),
            satellite_freq,
            prev_iq: Sample::new(0.0, 0.0),
            dc_state: 0.0,
            dc_alpha,
            bpf_state: [0.0; 4],
            bpf_coeffs,
            env_lpf_state: 0.0,
            env_lpf_alpha,
            resample_phase: 0.0,
            resample_step,
            line_buffer: Vec::with_capacity(FULL_LINE_SAMPLES + 100),
            sync_a: SyncCorrelator::new(SYNC_A_FREQ, APT_SAMPLE_RATE),
            sync_b: SyncCorrelator::new(SYNC_B_FREQ, APT_SAMPLE_RATE),
            line_count: 0,
            decoder_name,
        }
    }

    /// Apply biquad bandpass filter.
    #[inline]
    fn biquad_process(&mut self, x: f64) -> f64 {
        let y = self.bpf_coeffs.b0 * x
            + self.bpf_coeffs.b1 * self.bpf_state[0]
            + self.bpf_coeffs.b2 * self.bpf_state[1]
            - self.bpf_coeffs.a1 * self.bpf_state[2]
            - self.bpf_coeffs.a2 * self.bpf_state[3];

        self.bpf_state[1] = self.bpf_state[0];
        self.bpf_state[0] = x;
        self.bpf_state[3] = self.bpf_state[2];
        self.bpf_state[2] = y;

        y
    }

    /// Process a complete APT line from the line buffer.
    fn process_line(&mut self, messages: &mut Vec<DecodedMessage>) {
        if self.line_buffer.len() < FULL_LINE_SAMPLES {
            return;
        }

        let line_data = &self.line_buffer[..FULL_LINE_SAMPLES];

        // Try to find sync A in the first ~100 samples
        let mut sync_a_pos = None;
        let mut sync_a_quality = 0.0f32;
        for offset in 0..100.min(line_data.len()) {
            if let Some(corr) = self.sync_a.correlate(line_data, offset) {
                if corr > sync_a_quality {
                    sync_a_quality = corr;
                    sync_a_pos = Some(offset);
                }
            }
        }

        // Try to find sync B around the expected position (SAMPLES_PER_LINE)
        let sync_b_expected = sync_a_pos.unwrap_or(0) + SAMPLES_PER_LINE;
        let search_start = sync_b_expected.saturating_sub(50);
        let search_end = (sync_b_expected + 50).min(line_data.len());
        let mut sync_b_quality = 0.0f32;

        for offset in search_start..search_end {
            if let Some(corr) = self.sync_b.correlate(line_data, offset) {
                if corr > sync_b_quality {
                    sync_b_quality = corr;
                }
            }
        }

        // Extract pixel data
        let a_start = sync_a_pos.unwrap_or(0) + SYNC_WIDTH + SPACE_WIDTH;
        let a_end = a_start + IMAGE_A_WIDTH;
        let b_start = sync_a_pos.unwrap_or(0) + SAMPLES_PER_LINE + SYNC_WIDTH + SPACE_WIDTH;
        let b_end = b_start + IMAGE_B_WIDTH;

        // Normalize pixels to 0-255 range
        let pixels_a = if a_end <= line_data.len() {
            normalize_pixels(&line_data[a_start..a_end])
        } else {
            vec![0u8; IMAGE_A_WIDTH]
        };

        let pixels_b = if b_end <= line_data.len() {
            normalize_pixels(&line_data[b_start..b_end])
        } else {
            vec![0u8; IMAGE_B_WIDTH]
        };

        self.line_count += 1;

        let mut fields = BTreeMap::new();
        fields.insert("satellite".to_string(), self.satellite.clone());
        fields.insert("line_number".to_string(), self.line_count.to_string());
        fields.insert("sync_a_quality".to_string(), format!("{:.2}", sync_a_quality));
        fields.insert("sync_b_quality".to_string(), format!("{:.2}", sync_b_quality));
        fields.insert("image_width".to_string(), IMAGE_A_WIDTH.to_string());
        fields.insert("channel_a".to_string(), "Visible".to_string());
        fields.insert("channel_b".to_string(), "Infrared".to_string());

        // Encode pixels as comma-separated values for each channel
        let pixels_a_str: Vec<String> = pixels_a.iter().map(|p| p.to_string()).collect();
        let pixels_b_str: Vec<String> = pixels_b.iter().map(|p| p.to_string()).collect();
        fields.insert("pixels_a".to_string(), pixels_a_str.join(","));
        fields.insert("pixels_b".to_string(), pixels_b_str.join(","));

        let summary = format!(
            "{} line {} (sync: {:.0}%)",
            self.satellite,
            self.line_count,
            sync_a_quality * 100.0
        );

        messages.push(DecodedMessage {
            decoder: self.decoder_name.clone(),
            timestamp: Instant::now(),
            summary,
            fields,
            raw_bits: None,
        });

        // Keep any excess samples for the next line
        let remainder = self.line_buffer.split_off(FULL_LINE_SAMPLES);
        self.line_buffer = remainder;
    }
}

/// Normalize floating-point samples to 0-255 grayscale.
fn normalize_pixels(samples: &[f32]) -> Vec<u8> {
    if samples.is_empty() {
        return Vec::new();
    }

    let mut min = f32::MAX;
    let mut max = f32::MIN;
    for &s in samples {
        if s < min {
            min = s;
        }
        if s > max {
            max = s;
        }
    }

    let range = max - min;
    if range < 1e-10 {
        return vec![128u8; samples.len()];
    }

    samples
        .iter()
        .map(|&s| ((s - min) / range * 255.0).round().clamp(0.0, 255.0) as u8)
        .collect()
}

impl DecoderPlugin for NoaaAptDecoder {
    fn name(&self) -> &str {
        &self.decoder_name
    }

    fn requirements(&self) -> DecoderRequirements {
        DecoderRequirements {
            center_frequency: self.satellite_freq,
            sample_rate: self.sample_rate,
            bandwidth: 40000.0,
            wants_iq: true,
        }
    }

    fn process(&mut self, samples: &[Sample]) -> Vec<DecodedMessage> {
        let mut messages = Vec::new();

        for &sample in samples {
            // 1. FM quadrature discriminator
            let demod = util::fm_discriminate(sample, self.prev_iq);
            self.prev_iq = sample;

            // 2. DC removal
            let x = demod as f64;
            self.dc_state += self.dc_alpha * (x - self.dc_state);
            let dc_removed = x - self.dc_state;

            // 3. Bandpass filter centered at 2400 Hz
            let filtered = self.biquad_process(dc_removed);

            // 4. AM envelope detection (rectify + lowpass)
            let rectified = filtered.abs();
            self.env_lpf_state += self.env_lpf_alpha * (rectified - self.env_lpf_state);
            let envelope = self.env_lpf_state;

            // 5. Resample to APT rate (4160 Hz × 2 = 8320 Hz)
            self.resample_phase += self.resample_step;
            if self.resample_phase >= 1.0 {
                self.resample_phase -= 1.0;
                self.line_buffer.push(envelope as f32);

                // 6. Check for complete line
                if self.line_buffer.len() >= FULL_LINE_SAMPLES {
                    self.process_line(&mut messages);
                }
            }
        }

        messages
    }

    fn reset(&mut self) {
        self.prev_iq = Sample::new(0.0, 0.0);
        self.dc_state = 0.0;
        self.bpf_state = [0.0; 4];
        self.env_lpf_state = 0.0;
        self.resample_phase = 0.0;
        self.line_buffer.clear();
        self.line_count = 0;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Normalize pixels tests
    // ------------------------------------------------------------------

    #[test]
    fn normalize_pixels_basic() {
        let samples = vec![0.0, 0.5, 1.0];
        let pixels = normalize_pixels(&samples);
        assert_eq!(pixels[0], 0);
        assert_eq!(pixels[1], 128); // midpoint
        assert_eq!(pixels[2], 255);
    }

    #[test]
    fn normalize_pixels_constant() {
        let samples = vec![0.5, 0.5, 0.5];
        let pixels = normalize_pixels(&samples);
        // All same → all 128
        assert!(pixels.iter().all(|&p| p == 128));
    }

    #[test]
    fn normalize_pixels_empty() {
        let pixels = normalize_pixels(&[]);
        assert!(pixels.is_empty());
    }

    #[test]
    fn normalize_pixels_negative() {
        let samples = vec![-1.0, 0.0, 1.0];
        let pixels = normalize_pixels(&samples);
        assert_eq!(pixels[0], 0);
        assert_eq!(pixels[1], 128);
        assert_eq!(pixels[2], 255);
    }

    // ------------------------------------------------------------------
    // Sync correlator tests
    // ------------------------------------------------------------------

    #[test]
    fn sync_correlator_detects_matching_tone() {
        let sample_rate = APT_SAMPLE_RATE;
        let correlator = SyncCorrelator::new(SYNC_A_FREQ, sample_rate);

        // Generate matching 1040 Hz tone
        let n = correlator.reference.len() + 100;
        let mut data = vec![0.0f32; n];
        for (i, val) in data.iter_mut().enumerate() {
            let t = i as f64 / sample_rate;
            *val = (2.0 * PI * SYNC_A_FREQ * t).sin() as f32;
        }

        // Should find correlation near the start
        let mut found = false;
        for offset in 0..50 {
            if correlator.correlate(&data, offset).is_some() {
                found = true;
                break;
            }
        }
        assert!(found, "Should detect matching sync tone");
    }

    #[test]
    fn sync_correlator_rejects_wrong_tone() {
        let sample_rate = APT_SAMPLE_RATE;
        let correlator = SyncCorrelator::new(SYNC_A_FREQ, sample_rate);

        // Generate mismatched tone (500 Hz instead of 1040 Hz)
        let n = correlator.reference.len() + 10;
        let mut data = vec![0.0f32; n];
        for (i, val) in data.iter_mut().enumerate() {
            let t = i as f64 / sample_rate;
            *val = (2.0 * PI * 500.0 * t).sin() as f32;
        }

        // Should NOT find strong correlation
        let result = correlator.correlate(&data, 0);
        // May or may not be None depending on spectral similarity,
        // but should be much weaker than a match
        if let Some(corr) = result {
            assert!(corr < 0.8, "Wrong tone should have low correlation: {}", corr);
        }
    }

    #[test]
    fn sync_correlator_handles_noise() {
        let sample_rate = APT_SAMPLE_RATE;
        let correlator = SyncCorrelator::new(SYNC_A_FREQ, sample_rate);

        // Random noise
        let n = correlator.reference.len() + 10;
        let data: Vec<f32> = (0..n)
            .map(|i| ((i as f64 * 7.3).sin() * 0.1) as f32)
            .collect();

        let result = correlator.correlate(&data, 0);
        if let Some(corr) = result {
            assert!(corr < 0.8, "Noise should have low correlation");
        }
    }

    // ------------------------------------------------------------------
    // Biquad filter tests
    // ------------------------------------------------------------------

    #[test]
    fn biquad_bandpass_passes_center() {
        let sample_rate = 48000.0;
        let mut decoder = NoaaAptDecoder::new(sample_rate, "NOAA-19");

        // Generate 2400 Hz tone
        let mut power = 0.0f64;
        let n = (sample_rate * 0.1) as usize; // 100ms

        // Let filter settle
        for i in 0..n {
            let t = i as f64 / sample_rate;
            let x = (2.0 * PI * SUBCARRIER_FREQ * t).sin();
            let y = decoder.biquad_process(x);
            if i > n / 2 {
                power += y * y;
            }
        }

        assert!(power > 0.1, "Bandpass should pass 2400 Hz tone: power={}", power);
    }

    #[test]
    fn biquad_bandpass_rejects_offband() {
        let sample_rate = 48000.0;
        let mut decoder = NoaaAptDecoder::new(sample_rate, "NOAA-19");

        // Generate 100 Hz tone (well below passband)
        let mut power = 0.0f64;
        let n = (sample_rate * 0.1) as usize;

        for i in 0..n {
            let t = i as f64 / sample_rate;
            let x = (2.0 * PI * 100.0 * t).sin();
            let y = decoder.biquad_process(x);
            if i > n / 2 {
                power += y * y;
            }
        }

        // Should have much less power than passband (wide BPF allows some leakage)
        assert!(power < 0.1, "Bandpass should attenuate 100 Hz: power={}", power);
    }

    // ------------------------------------------------------------------
    // Line processing tests
    // ------------------------------------------------------------------

    #[test]
    fn process_line_generates_message() {
        let mut decoder = NoaaAptDecoder::new(48000.0, "NOAA-19");

        // Generate a fake APT line with sync tones
        let mut line = vec![0.0f32; FULL_LINE_SAMPLES];

        // Add sync A tone (1040 Hz) at the start
        let samples_per_cycle_a = (APT_SAMPLE_RATE / SYNC_A_FREQ) as usize;
        for (i, val) in line.iter_mut().take(samples_per_cycle_a * SYNC_CYCLES).enumerate() {
            let t = i as f64 / APT_SAMPLE_RATE;
            if i < SYNC_WIDTH {
                *val = (2.0 * PI * SYNC_A_FREQ * t).sin() as f32;
            }
        }

        // Add some image data (ramp)
        let img_start = SYNC_WIDTH + SPACE_WIDTH;
        for i in 0..IMAGE_A_WIDTH {
            if img_start + i < SAMPLES_PER_LINE {
                line[img_start + i] = i as f32 / IMAGE_A_WIDTH as f32;
            }
        }

        // Add sync B tone at half-line point
        let b_offset = SAMPLES_PER_LINE;
        for i in 0..(SYNC_WIDTH.min(FULL_LINE_SAMPLES - b_offset)) {
            let t = i as f64 / APT_SAMPLE_RATE;
            line[b_offset + i] = (2.0 * PI * SYNC_B_FREQ * t).sin() as f32;
        }

        decoder.line_buffer = line;
        let mut messages = Vec::new();
        decoder.process_line(&mut messages);

        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.decoder, "noaa-apt-19");
        assert_eq!(msg.fields["satellite"], "NOAA-19");
        assert_eq!(msg.fields["line_number"], "1");
        assert!(msg.fields.contains_key("pixels_a"));
        assert!(msg.fields.contains_key("pixels_b"));
    }

    // ------------------------------------------------------------------
    // Decoder plugin interface tests
    // ------------------------------------------------------------------

    #[test]
    fn decoder_plugin_noaa19() {
        let decoder = NoaaAptDecoder::new(48000.0, "NOAA-19");
        assert_eq!(decoder.name(), "noaa-apt-19");
        assert!(decoder.requirements().wants_iq);
        assert!((decoder.requirements().center_frequency - NOAA19_FREQ).abs() < 1.0);
        assert!((decoder.requirements().sample_rate - 48000.0).abs() < 1.0);
    }

    #[test]
    fn decoder_plugin_noaa15() {
        let decoder = NoaaAptDecoder::new(48000.0, "NOAA-15");
        assert_eq!(decoder.name(), "noaa-apt-15");
        assert!((decoder.requirements().center_frequency - NOAA15_FREQ).abs() < 1.0);
    }

    #[test]
    fn decoder_plugin_noaa18() {
        let decoder = NoaaAptDecoder::new(48000.0, "NOAA-18");
        assert_eq!(decoder.name(), "noaa-apt-18");
        assert!((decoder.requirements().center_frequency - NOAA18_FREQ).abs() < 1.0);
    }

    #[test]
    fn decoder_handles_empty_input() {
        let mut decoder = NoaaAptDecoder::new(48000.0, "NOAA-19");
        let msgs = decoder.process(&[]);
        assert!(msgs.is_empty());
    }

    #[test]
    fn decoder_handles_noise() {
        let mut decoder = NoaaAptDecoder::new(48000.0, "NOAA-19");
        let noise: Vec<Sample> = (0..10000)
            .map(|i| {
                let phase = (i as f64 * 0.37).sin();
                Sample::new(phase as f32, (phase + 1.0) as f32 * 0.5)
            })
            .collect();
        // May or may not produce lines from noise, but should not crash
        let _msgs = decoder.process(&noise);
    }

    #[test]
    fn decoder_reset_clears_line_buffer() {
        let mut decoder = NoaaAptDecoder::new(48000.0, "NOAA-19");
        decoder.line_buffer.push(1.0);
        decoder.line_count = 42;
        decoder.reset();
        assert!(decoder.line_buffer.is_empty());
        assert_eq!(decoder.line_count, 0);
    }

    // ------------------------------------------------------------------
    // Constants validation
    // ------------------------------------------------------------------

    #[test]
    fn line_format_sizes_add_up() {
        // Each half-line: sync(39) + space(47) + image(909) + telemetry(45) = 1040
        let half_line = SYNC_WIDTH + SPACE_WIDTH + IMAGE_A_WIDTH + TELEMETRY_WIDTH;
        assert_eq!(half_line, SAMPLES_PER_LINE / 2);
        assert_eq!(SAMPLES_PER_LINE, FULL_LINE_SAMPLES / 2);
        assert_eq!(FULL_LINE_SAMPLES, 4160);
    }

    #[test]
    fn apt_sample_rate_correct() {
        // 4160 samples/line × 2 lines/sec = 8320 Hz
        assert_eq!(APT_SAMPLE_RATE as u32, 8320);
    }
}
