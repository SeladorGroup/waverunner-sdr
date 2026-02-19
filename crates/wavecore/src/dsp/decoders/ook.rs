//! OOK/FSK Pulse Decoder for ISM Band Devices
//!
//! Decodes On-Off Keying and simple FSK signals commonly used by IoT
//! devices, weather stations, tire pressure monitors, and other ISM
//! band devices operating at 315/433.92/868/915 MHz.
//!
//! ## Signal Processing Chain
//!
//! ```text
//! IQ samples → Magnitude (envelope) → Adaptive threshold
//!   → Pulse/gap timing → Protocol pattern matching
//!   → Data extraction → Checksum validation
//! ```
//!
//! ## Supported Protocols
//!
//! - **Oregon Scientific v2.1**: Manchester-encoded weather sensor data
//! - **Acurite**: PWM-encoded weather station data
//! - **Generic TPMS**: Tire pressure monitoring (Manchester)

use std::collections::BTreeMap;
use std::time::Instant;

use crate::dsp::decoder::{DecoderPlugin, DecoderRequirements};
use crate::session::DecodedMessage;
use crate::types::Sample;

// ============================================================================
// Constants
// ============================================================================


/// Minimum pulse duration in microseconds (to filter glitches).
const MIN_PULSE_US: f64 = 50.0;
/// Maximum pulse duration in microseconds.
const MAX_PULSE_US: f64 = 10000.0;
/// Gap duration that signals end of transmission (microseconds).
const END_GAP_US: f64 = 20000.0;

// ============================================================================
// Pulse Detection
// ============================================================================

/// A measured pulse or gap.
#[derive(Debug, Clone, Copy)]
struct Pulse {
    /// Duration in microseconds.
    duration_us: f64,
    /// True if high (signal present), false if low (gap).
    is_high: bool,
}

/// Pulse detector with adaptive threshold.
///
/// Converts magnitude samples into a sequence of high/low pulses
/// with measured durations.
struct PulseDetector {
    /// Samples per microsecond.
    samples_per_us: f64,
    /// Adaptive noise floor estimate.
    noise_floor: f64,
    /// Adaptive signal peak estimate.
    signal_peak: f64,
    /// Current threshold.
    threshold: f64,
    /// Whether current state is high (signal present).
    is_high: bool,
    /// Number of samples in current state.
    state_samples: usize,
    /// Accumulated pulses for current burst.
    pulses: Vec<Pulse>,
    /// Sample count since last pulse (for end-of-transmission detection).
    gap_samples: usize,
    /// Attack coefficient for noise floor tracking.
    noise_alpha: f64,
    /// Attack coefficient for signal peak tracking.
    signal_alpha: f64,
}

impl PulseDetector {
    fn new(sample_rate: f64) -> Self {
        let samples_per_us = sample_rate / 1e6;
        Self {
            samples_per_us,
            noise_floor: 0.01,
            signal_peak: 0.1,
            threshold: 0.05,
            is_high: false,
            state_samples: 0,
            pulses: Vec::with_capacity(256),
            gap_samples: 0,
            noise_alpha: 0.001,
            signal_alpha: 0.01,
        }
    }

    /// Feed one magnitude sample. Returns completed pulse trains when
    /// an end-of-transmission gap is detected.
    fn feed(&mut self, mag: f32) -> Option<Vec<Pulse>> {
        let mag = mag as f64;

        // Update adaptive threshold
        if mag < self.threshold {
            self.noise_floor += self.noise_alpha * (mag - self.noise_floor);
        } else {
            self.signal_peak += self.signal_alpha * (mag - self.signal_peak);
        }
        self.threshold = (self.noise_floor + self.signal_peak) / 2.0;
        // Clamp threshold to prevent collapse
        if self.threshold < self.noise_floor * 2.0 {
            self.threshold = self.noise_floor * 2.0;
        }

        let new_high = mag > self.threshold;

        if new_high == self.is_high {
            self.state_samples += 1;

            if !self.is_high {
                self.gap_samples += 1;
                let gap_us = self.gap_samples as f64 / self.samples_per_us;

                // End of transmission?
                if gap_us > END_GAP_US && !self.pulses.is_empty() {
                    let result = std::mem::take(&mut self.pulses);
                    self.gap_samples = 0;
                    return Some(result);
                }
            }
        } else {
            // State transition
            let duration_us = self.state_samples as f64 / self.samples_per_us;

            if (MIN_PULSE_US..=MAX_PULSE_US).contains(&duration_us) {
                self.pulses.push(Pulse {
                    duration_us,
                    is_high: self.is_high,
                });
            }

            self.is_high = new_high;
            self.state_samples = 1;
            if new_high {
                self.gap_samples = 0;
            } else {
                self.gap_samples = 1;
            }
        }

        None
    }

    fn reset(&mut self) {
        self.noise_floor = 0.01;
        self.signal_peak = 0.1;
        self.threshold = 0.05;
        self.is_high = false;
        self.state_samples = 0;
        self.pulses.clear();
        self.gap_samples = 0;
    }
}

// ============================================================================
// Manchester Decoding
// ============================================================================

/// Decode Manchester-encoded data from pulse timings.
///
/// Manchester encoding: short-short = 0 or 1 (depending on convention),
/// long = transition in the middle of a bit.
///
/// `short_us`: expected short pulse duration
/// `tolerance`: fractional tolerance (e.g., 0.3 = ±30%)
fn manchester_decode(pulses: &[Pulse], short_us: f64, tolerance: f64) -> Option<Vec<u8>> {
    let long_us = short_us * 2.0;
    let short_min = short_us * (1.0 - tolerance);
    let short_max = short_us * (1.0 + tolerance);
    let long_min = long_us * (1.0 - tolerance);
    let long_max = long_us * (1.0 + tolerance);

    let mut bits = Vec::new();
    let mut i = 0;

    while i < pulses.len() {
        let p = &pulses[i];

        if p.duration_us >= short_min && p.duration_us <= short_max {
            // Short pulse — need another short pulse to form a bit
            if i + 1 < pulses.len() {
                let next = &pulses[i + 1];
                if next.duration_us >= short_min && next.duration_us <= short_max {
                    // Two shorts: high-low = 1, low-high = 0
                    bits.push(if p.is_high { 1 } else { 0 });
                    i += 2;
                    continue;
                }
            }
            // Orphan short — error
            i += 1;
        } else if p.duration_us >= long_min && p.duration_us <= long_max {
            // Long pulse — single bit
            bits.push(if p.is_high { 1 } else { 0 });
            i += 1;
        } else {
            // Out of tolerance — skip
            i += 1;
        }
    }

    if bits.len() >= 8 {
        Some(bits)
    } else {
        None
    }
}

// ============================================================================
// Protocol: Oregon Scientific v2.1
// ============================================================================

/// Attempt to decode Oregon Scientific v2.1 weather sensor data.
///
/// Oregon Scientific v2.1 uses Manchester encoding at ~1024 baud
/// (short pulse ≈ 488 μs). Messages contain:
/// - Preamble: alternating 1/0
/// - Sync: 0x0000FFFF pattern
/// - Sensor ID (16 bits)
/// - Channel (4 bits)
/// - Rolling code (8 bits)
/// - Data (varies by sensor type)
/// - Checksum (8 bits)
fn decode_oregon_scientific(pulses: &[Pulse]) -> Option<DecodedMessage> {
    let short_us = 488.0; // ~1024 baud
    let bits = manchester_decode(pulses, short_us, 0.35)?;

    if bits.len() < 32 {
        return None;
    }

    // Look for sync pattern (skip preamble)
    let mut sync_pos = None;
    for i in 0..bits.len().saturating_sub(16) {
        // Look for 0xA (1010) nibble pattern that follows preamble
        if bits[i] == 1 && bits.get(i + 1) == Some(&0)
            && bits.get(i + 2) == Some(&1) && bits.get(i + 3) == Some(&0)
        {
            sync_pos = Some(i + 4);
            break;
        }
    }

    let start = sync_pos.unwrap_or(0);
    let data = &bits[start..];

    if data.len() < 32 {
        return None;
    }

    // Extract nibbles (LSB first within each nibble)
    let nibbles: Vec<u8> = data
        .chunks(4)
        .map(|chunk| {
            let mut n = 0u8;
            for (i, &b) in chunk.iter().enumerate() {
                n |= (b & 1) << i;
            }
            n
        })
        .collect();

    if nibbles.len() < 8 {
        return None;
    }

    // Sensor ID (nibbles 0-3)
    let sensor_id = (nibbles[0] as u16) << 12
        | (nibbles[1] as u16) << 8
        | (nibbles[2] as u16) << 4
        | nibbles[3] as u16;

    // Channel (nibble 4)
    let channel = nibbles[4];

    let mut fields = BTreeMap::new();
    fields.insert("protocol".to_string(), "Oregon-v2.1".to_string());
    fields.insert("sensor_id".to_string(), format!("{:04X}", sensor_id));
    fields.insert("channel".to_string(), channel.to_string());

    // Temperature (nibbles 6-8, BCD, sign in nibble 9)
    if nibbles.len() >= 10 {
        let temp_100 = nibbles.get(8).copied().unwrap_or(0);
        let temp_10 = nibbles.get(7).copied().unwrap_or(0);
        let temp_1 = nibbles.get(6).copied().unwrap_or(0);
        let sign = nibbles.get(9).copied().unwrap_or(0);

        let temp = temp_100 as f64 * 10.0 + temp_10 as f64 + temp_1 as f64 * 0.1;
        let temp = if sign != 0 { -temp } else { temp };
        let temp_f = temp * 9.0 / 5.0 + 32.0;

        fields.insert("temperature_c".to_string(), format!("{:.1}", temp));
        fields.insert("temperature_f".to_string(), format!("{:.1}", temp_f));
    }

    // Humidity (nibbles 10-11 if present)
    if nibbles.len() >= 12 {
        let hum = nibbles[10] + nibbles[11] * 10;
        if hum > 0 && hum <= 100 {
            fields.insert("humidity_pct".to_string(), hum.to_string());
        }
    }

    // Checksum validation (sum of nibbles)
    let sum: u16 = nibbles[..nibbles.len() - 1].iter().map(|&n| n as u16).sum();
    let check = (sum & 0xFF) as u8;
    let expected = *nibbles.last().unwrap_or(&0);
    if check != expected {
        // Checksum mismatch — still report but flag it
        fields.insert("checksum".to_string(), "fail".to_string());
    }

    let summary = if let Some(temp) = fields.get("temperature_f") {
        if let Some(hum) = fields.get("humidity_pct") {
            format!("Oregon ch{} {}°F {}%RH", channel, temp, hum)
        } else {
            format!("Oregon ch{} {}°F", channel, temp)
        }
    } else {
        format!("Oregon ch{} sensor {:04X}", channel, sensor_id)
    };

    Some(DecodedMessage {
        decoder: "ook".to_string(),
        timestamp: Instant::now(),
        summary,
        fields,
        raw_bits: Some(data.to_vec()),
    })
}

// ============================================================================
// Protocol: Acurite Weather
// ============================================================================

/// Attempt to decode Acurite weather sensor data.
///
/// Acurite uses PWM encoding: short pulse (~200μs) + short gap = 0,
/// short pulse + long gap (~400μs) = 1.
fn decode_acurite(pulses: &[Pulse]) -> Option<DecodedMessage> {
    // Acurite sync: 4 long pulses (~600μs each)
    let mut sync_end = 0;
    let mut long_count = 0;

    for (i, p) in pulses.iter().enumerate() {
        if p.is_high && p.duration_us > 400.0 && p.duration_us < 800.0 {
            long_count += 1;
            if long_count >= 3 {
                sync_end = i + 1;
                break;
            }
        } else if p.is_high {
            long_count = 0;
        }
    }

    if sync_end == 0 {
        return None;
    }

    // Decode PWM bits: short high + gap determines bit value
    let data = &pulses[sync_end..];
    let mut bits = Vec::new();

    let mut i = 0;
    while i + 1 < data.len() {
        if data[i].is_high {
            // Pulse width determines bit
            let gap = data[i + 1].duration_us;
            if gap < 300.0 {
                bits.push(0u8);
            } else {
                bits.push(1u8);
            }
            i += 2;
        } else {
            i += 1;
        }
    }

    if bits.len() < 40 {
        return None;
    }

    // Pack bits into bytes
    let bytes: Vec<u8> = bits
        .chunks(8)
        .map(|chunk| {
            let mut byte = 0u8;
            for (i, &b) in chunk.iter().enumerate() {
                byte |= (b & 1) << (7 - i);
            }
            byte
        })
        .collect();

    if bytes.len() < 5 {
        return None;
    }

    let mut fields = BTreeMap::new();
    fields.insert("protocol".to_string(), "Acurite".to_string());

    // Sensor ID (bytes 0-1)
    let sensor_id = ((bytes[0] as u16) << 8) | bytes[1] as u16;
    fields.insert("sensor_id".to_string(), format!("{:04X}", sensor_id));

    // Battery status (byte 2, bit 6)
    let battery_ok = (bytes[2] & 0x40) != 0;
    fields.insert(
        "battery".to_string(),
        (if battery_ok { "ok" } else { "low" }).to_string(),
    );

    // Channel (byte 2, bits 4-5)
    let channel = ((bytes[2] >> 4) & 0x03) + 1;
    fields.insert("channel".to_string(), channel.to_string());

    // Temperature (bytes 3-4, 12-bit signed, 0.1°C)
    if bytes.len() >= 5 {
        let temp_raw = (((bytes[3] & 0x0F) as i16) << 8) | bytes[4] as i16;
        let temp_raw = if temp_raw & 0x800 != 0 {
            temp_raw - 0x1000
        } else {
            temp_raw
        };
        let temp_c = temp_raw as f64 / 10.0;
        let temp_f = temp_c * 9.0 / 5.0 + 32.0;
        fields.insert("temperature_c".to_string(), format!("{:.1}", temp_c));
        fields.insert("temperature_f".to_string(), format!("{:.1}", temp_f));
    }

    // Humidity (byte 5 if present)
    if bytes.len() >= 6 {
        let humidity = bytes[5] & 0x7F;
        if humidity > 0 && humidity <= 100 {
            fields.insert("humidity_pct".to_string(), humidity.to_string());
        }
    }

    let summary = if let Some(temp) = fields.get("temperature_f") {
        format!("Acurite ch{} {}°F", channel, temp)
    } else {
        format!("Acurite ch{} sensor {:04X}", channel, sensor_id)
    };

    Some(DecodedMessage {
        decoder: "ook".to_string(),
        timestamp: Instant::now(),
        summary,
        fields,
        raw_bits: Some(bits),
    })
}

// ============================================================================
// Protocol: Generic TPMS
// ============================================================================

/// Attempt to decode generic TPMS (Tire Pressure Monitoring) data.
///
/// Many TPMS sensors use Manchester encoding at ~9-10 kbaud
/// (short pulse ≈ 50-55μs). Message format varies but typically:
/// - Preamble (alternating bits)
/// - Sensor ID (28-32 bits)
/// - Pressure (8 bits, PSI or kPa)
/// - Temperature (8 bits, °C offset)
/// - Status/flags (8 bits)
/// - CRC or checksum
fn decode_tpms(pulses: &[Pulse]) -> Option<DecodedMessage> {
    let short_us = 52.0; // ~9600 baud
    let bits = manchester_decode(pulses, short_us, 0.40)?;

    if bits.len() < 40 {
        return None;
    }

    // Pack into bytes
    let bytes: Vec<u8> = bits
        .chunks(8)
        .map(|chunk| {
            let mut byte = 0u8;
            for (i, &b) in chunk.iter().enumerate() {
                byte |= (b & 1) << (7 - i);
            }
            byte
        })
        .collect();

    if bytes.len() < 5 {
        return None;
    }

    let mut fields = BTreeMap::new();
    fields.insert("protocol".to_string(), "TPMS".to_string());

    // Sensor ID (first 4 bytes)
    let sensor_id = ((bytes[0] as u32) << 24)
        | ((bytes[1] as u32) << 16)
        | ((bytes[2] as u32) << 8)
        | bytes[3] as u32;
    fields.insert("sensor_id".to_string(), format!("{:08X}", sensor_id));

    // Pressure (byte 4, 0-255, typically in 0.25 PSI units)
    let pressure_raw = bytes[4];
    let pressure_psi = pressure_raw as f64 * 0.25;
    let pressure_kpa = pressure_psi * 6.895;
    fields.insert("pressure_psi".to_string(), format!("{:.1}", pressure_psi));
    fields.insert("pressure_kpa".to_string(), format!("{:.1}", pressure_kpa));

    // Temperature (byte 5, offset by 40°C)
    if bytes.len() >= 6 {
        let temp_c = bytes[5] as i16 - 40;
        let temp_f = temp_c as f64 * 9.0 / 5.0 + 32.0;
        fields.insert("temperature_c".to_string(), temp_c.to_string());
        fields.insert("temperature_f".to_string(), format!("{:.0}", temp_f));
    }

    let summary = format!(
        "TPMS {:08X} {:.1}PSI",
        sensor_id, pressure_psi
    );

    Some(DecodedMessage {
        decoder: "ook".to_string(),
        timestamp: Instant::now(),
        summary,
        fields,
        raw_bits: Some(bits),
    })
}

// ============================================================================
// OOK Decoder
// ============================================================================

/// Protocol filter for the OOK decoder.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OokFilter {
    /// Try all protocols.
    All,
    /// Weather sensors only (Oregon Scientific, Acurite).
    Weather,
    /// TPMS sensors only.
    Tpms,
}

/// OOK/FSK pulse decoder plugin.
///
/// Decodes ISM band devices using envelope detection, adaptive
/// thresholding, and protocol-specific pulse pattern matching.
pub struct OokDecoder {
    sample_rate: f64,
    center_freq: f64,
    filter: OokFilter,
    /// Pulse detector.
    detector: PulseDetector,
    /// Decoder name (varies by filter).
    decoder_name: String,
}

impl OokDecoder {
    /// Create a new OOK decoder.
    pub fn new(sample_rate: f64, center_freq: f64, filter: OokFilter) -> Self {
        let decoder_name = match filter {
            OokFilter::All => "ook".to_string(),
            OokFilter::Weather => "ook-weather".to_string(),
            OokFilter::Tpms => "ook-tpms".to_string(),
        };
        Self {
            sample_rate,
            center_freq,
            filter,
            detector: PulseDetector::new(sample_rate),
            decoder_name,
        }
    }

    /// Try all applicable protocols on a pulse train.
    fn try_decode(&self, pulses: &[Pulse]) -> Vec<DecodedMessage> {
        let mut messages = Vec::new();

        match self.filter {
            OokFilter::All => {
                if let Some(msg) = decode_oregon_scientific(pulses) {
                    messages.push(msg);
                }
                if let Some(msg) = decode_acurite(pulses) {
                    messages.push(msg);
                }
                if let Some(msg) = decode_tpms(pulses) {
                    messages.push(msg);
                }
            }
            OokFilter::Weather => {
                if let Some(msg) = decode_oregon_scientific(pulses) {
                    messages.push(msg);
                }
                if let Some(msg) = decode_acurite(pulses) {
                    messages.push(msg);
                }
            }
            OokFilter::Tpms => {
                if let Some(msg) = decode_tpms(pulses) {
                    messages.push(msg);
                }
            }
        }

        messages
    }
}

impl DecoderPlugin for OokDecoder {
    fn name(&self) -> &str {
        &self.decoder_name
    }

    fn requirements(&self) -> DecoderRequirements {
        DecoderRequirements {
            center_frequency: self.center_freq,
            sample_rate: self.sample_rate,
            bandwidth: 200000.0,
            wants_iq: true,
        }
    }

    fn process(&mut self, samples: &[Sample]) -> Vec<DecodedMessage> {
        let mut messages = Vec::new();

        for &sample in samples {
            // Envelope detection (magnitude)
            let mag = (sample.re * sample.re + sample.im * sample.im).sqrt();

            // Feed pulse detector
            if let Some(pulses) = self.detector.feed(mag) {
                let decoded = self.try_decode(&pulses);
                messages.extend(decoded);
            }
        }

        messages
    }

    fn reset(&mut self) {
        self.detector.reset();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Pulse detector tests
    // ------------------------------------------------------------------

    #[test]
    fn pulse_detector_detects_pulses() {
        let sample_rate = 250000.0;
        let mut detector = PulseDetector::new(sample_rate);

        // Generate a simple OOK burst: high for 500μs, low for 500μs, repeat
        let samples_per_500us = (sample_rate * 500e-6) as usize;

        // Let noise floor settle
        for _ in 0..1000 {
            detector.feed(0.001);
        }

        // Burst: 3 pulses
        for _ in 0..3 {
            for _ in 0..samples_per_500us {
                detector.feed(1.0); // High
            }
            for _ in 0..samples_per_500us {
                detector.feed(0.001); // Low
            }
        }

        // End gap
        let eot_samples = (sample_rate * 25e-3) as usize;
        let mut result = None;
        for _ in 0..eot_samples {
            if let Some(pulses) = detector.feed(0.001) {
                result = Some(pulses);
            }
        }

        assert!(result.is_some(), "Should detect pulse train");
        let pulses = result.unwrap();
        assert!(pulses.len() >= 4, "Should have at least 4 pulses/gaps, got {}", pulses.len());
    }

    #[test]
    fn pulse_detector_ignores_noise() {
        let mut detector = PulseDetector::new(250000.0);

        // Feed low-level noise
        for i in 0..100000 {
            let noise = ((i as f64 * 0.001).sin() * 0.002 + 0.001) as f32;
            assert!(
                detector.feed(noise).is_none(),
                "Noise should not produce pulses"
            );
        }
    }

    #[test]
    fn pulse_detector_reset() {
        let mut detector = PulseDetector::new(250000.0);

        // Feed some signal
        for _ in 0..1000 {
            detector.feed(1.0);
        }

        detector.reset();
        assert!(detector.pulses.is_empty());
        assert!(!detector.is_high);
    }

    // ------------------------------------------------------------------
    // Manchester decoding tests
    // ------------------------------------------------------------------

    #[test]
    fn manchester_decode_basic() {
        // Create alternating short-short pairs (representing bits)
        let short = 500.0;
        let pulses = vec![
            Pulse { duration_us: short, is_high: true },
            Pulse { duration_us: short, is_high: false },
            Pulse { duration_us: short, is_high: true },
            Pulse { duration_us: short, is_high: false },
            Pulse { duration_us: short, is_high: true },
            Pulse { duration_us: short, is_high: false },
            Pulse { duration_us: short, is_high: true },
            Pulse { duration_us: short, is_high: false },
            Pulse { duration_us: short, is_high: true },
            Pulse { duration_us: short, is_high: false },
            Pulse { duration_us: short, is_high: true },
            Pulse { duration_us: short, is_high: false },
            Pulse { duration_us: short, is_high: true },
            Pulse { duration_us: short, is_high: false },
            Pulse { duration_us: short, is_high: true },
            Pulse { duration_us: short, is_high: false },
        ];

        let bits = manchester_decode(&pulses, short, 0.3);
        assert!(bits.is_some(), "Should decode Manchester data");
        let bits = bits.unwrap();
        assert_eq!(bits.len(), 8, "Should produce 8 bits from 16 short pulses");
        // All short-short high-low pairs → all 1 bits
        assert!(bits.iter().all(|&b| b == 1));
    }

    #[test]
    fn manchester_decode_long_pulses() {
        // Long pulses represent single bits
        let short = 500.0;
        let long = 1000.0;

        let pulses = vec![
            Pulse { duration_us: long, is_high: true },  // 1
            Pulse { duration_us: long, is_high: false }, // 0
            Pulse { duration_us: long, is_high: true },  // 1
            Pulse { duration_us: long, is_high: false }, // 0
            Pulse { duration_us: long, is_high: true },  // 1
            Pulse { duration_us: long, is_high: false }, // 0
            Pulse { duration_us: long, is_high: true },  // 1
            Pulse { duration_us: long, is_high: false }, // 0
        ];

        let bits = manchester_decode(&pulses, short, 0.3);
        assert!(bits.is_some());
        let bits = bits.unwrap();
        assert_eq!(bits.len(), 8);
        // Long high→1, long low→0
        assert_eq!(bits, vec![1, 0, 1, 0, 1, 0, 1, 0]);
    }

    #[test]
    fn manchester_decode_too_few_bits() {
        let pulses = vec![
            Pulse { duration_us: 500.0, is_high: true },
            Pulse { duration_us: 500.0, is_high: false },
        ];
        let bits = manchester_decode(&pulses, 500.0, 0.3);
        assert!(bits.is_none(), "Too few bits should return None");
    }

    // ------------------------------------------------------------------
    // Oregon Scientific tests
    // ------------------------------------------------------------------

    #[test]
    fn oregon_decode_returns_none_for_empty() {
        assert!(decode_oregon_scientific(&[]).is_none());
    }

    #[test]
    fn oregon_decode_returns_none_for_short() {
        let pulses: Vec<Pulse> = (0..10)
            .map(|_| Pulse {
                duration_us: 488.0,
                is_high: true,
            })
            .collect();
        assert!(decode_oregon_scientific(&pulses).is_none());
    }

    // ------------------------------------------------------------------
    // Acurite tests
    // ------------------------------------------------------------------

    #[test]
    fn acurite_decode_returns_none_for_empty() {
        assert!(decode_acurite(&[]).is_none());
    }

    #[test]
    fn acurite_decode_needs_sync() {
        // Short pulses without sync pattern
        let pulses: Vec<Pulse> = (0..100)
            .map(|i| Pulse {
                duration_us: 200.0,
                is_high: i % 2 == 0,
            })
            .collect();
        assert!(decode_acurite(&pulses).is_none());
    }

    // ------------------------------------------------------------------
    // TPMS tests
    // ------------------------------------------------------------------

    #[test]
    fn tpms_decode_returns_none_for_empty() {
        assert!(decode_tpms(&[]).is_none());
    }

    // ------------------------------------------------------------------
    // Decoder plugin interface tests
    // ------------------------------------------------------------------

    #[test]
    fn decoder_plugin_interface() {
        let decoder = OokDecoder::new(250000.0, 433.92e6, OokFilter::All);
        assert_eq!(decoder.name(), "ook");
        assert!(decoder.requirements().wants_iq);
        assert!((decoder.requirements().sample_rate - 250000.0).abs() < 1.0);
    }

    #[test]
    fn decoder_weather_filter_name() {
        let decoder = OokDecoder::new(250000.0, 433.92e6, OokFilter::Weather);
        assert_eq!(decoder.name(), "ook-weather");
    }

    #[test]
    fn decoder_tpms_filter_name() {
        let decoder = OokDecoder::new(250000.0, 433.92e6, OokFilter::Tpms);
        assert_eq!(decoder.name(), "ook-tpms");
    }

    #[test]
    fn decoder_handles_empty_input() {
        let mut decoder = OokDecoder::new(250000.0, 433.92e6, OokFilter::All);
        let msgs = decoder.process(&[]);
        assert!(msgs.is_empty());
    }

    #[test]
    fn decoder_handles_noise() {
        let mut decoder = OokDecoder::new(250000.0, 433.92e6, OokFilter::All);
        let noise: Vec<Sample> = (0..10000)
            .map(|i| {
                let v = (i as f64 * 0.001).sin() * 0.001;
                Sample::new(v as f32, 0.0)
            })
            .collect();
        let msgs = decoder.process(&noise);
        assert!(msgs.is_empty(), "Noise should not produce messages");
    }

    #[test]
    fn decoder_reset_works() {
        let mut decoder = OokDecoder::new(250000.0, 433.92e6, OokFilter::All);
        let samples: Vec<Sample> = (0..1000)
            .map(|i| Sample::new((i as f32 * 0.1).sin(), 0.0))
            .collect();
        decoder.process(&samples);
        decoder.reset();
        let msgs = decoder.process(&[]);
        assert!(msgs.is_empty());
    }
}
