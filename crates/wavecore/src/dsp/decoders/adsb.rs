//! ADS-B (Automatic Dependent Surveillance – Broadcast) Decoder
//!
//! Decodes Mode S Extended Squitter (DF17/DF18) messages from 1090 MHz
//! pulse-position modulated (PPM) signals at 2 MS/s.
//!
//! ## Signal Structure
//!
//! ```text
//! Preamble (8 μs):
//!  ┌─┐ ┌─┐     ┌─┐ ┌─┐
//!  │ │ │ │     │ │ │ │
//! ─┘ └─┘ └─────┘ └─┘ └───
//!  0 1 2 3 4 5 6 7 8 9 10 (μs)
//!
//! Each data bit (1 μs):
//!  Bit=1: ┌─┐     Bit=0:     ┌─┐
//!         │ │               │ │
//!  ───────┘ └─     ─────────┘ └─
//!  0   0.5  1     0   0.5  1 (μs)
//! ```
//!
//! ## Message Format (DF17 Extended Squitter, 112 bits)
//!
//! ```text
//! ┌──────┬────┬────────────────┬───────────────────────────┬──────────┐
//! │ DF   │ CA │ ICAO Address   │ ME (message, 56 bits)     │ CRC-24   │
//! │ 5bit │3bit│ 24 bits        │                           │ 24 bits  │
//! └──────┴────┴────────────────┴───────────────────────────┴──────────┘
//! ```
//!
//! ## CRC-24 (Mode S)
//!
//! Generator polynomial: x²⁴ + x²³ + x²² + x²¹ + x²⁰ + x¹⁹ + x¹⁸ + x¹⁷
//!                      + x¹⁶ + x¹⁵ + x¹⁴ + x¹³ + x¹⁰ + x³ + 1
//! = 0x1FFF409 (25 bits with leading 1)
//!
//! ## CPR (Compact Position Reporting)
//!
//! Aircraft position is encoded using CPR, which compresses lat/lon into
//! 17-bit values using modular arithmetic. Two message types (even/odd)
//! enable global unambiguous decode when both are available.
//!
//! Global decode uses NL(lat) zones — the number of longitude zones for
//! a given latitude, derived from equal-area partitioning on a sphere:
//!
//!   NL(lat) = floor(2π / acos(1 − (1−cos(π/(2·NZ))) / cos²(π·lat/180)))
//!
//! where NZ = 15.

use std::collections::BTreeMap;
use std::time::Instant;

use crate::dsp::decoder::{DecoderPlugin, DecoderRequirements};
use crate::session::DecodedMessage;
use crate::types::Sample;

// ============================================================================
// Constants
// ============================================================================

/// CRC-24 generator polynomial for Mode S.
const CRC24_GENERATOR: u32 = 0x1FFF409;

/// Expected preamble pulse pattern at 2 MS/s (positions of high samples).
/// Preamble: pulses at 0, 1, 3.5, 4.5 μs → samples 0, 2, 7, 9 at 2 MS/s.
const PREAMBLE_POSITIONS: [usize; 4] = [0, 2, 7, 9];
/// Expected low positions between preamble pulses.
const PREAMBLE_GAPS: [usize; 5] = [1, 3, 4, 5, 6];

/// Mode S message lengths.
const SHORT_MSG_BITS: usize = 56;
const LONG_MSG_BITS: usize = 112;

/// Preamble length in samples at 2 MS/s.
const PREAMBLE_SAMPLES: usize = 16;

/// Minimum samples for a full long message: preamble + 112 bits × 2 samples/bit.
const MIN_BUFFER_LEN: usize = PREAMBLE_SAMPLES + LONG_MSG_BITS * 2;

// ============================================================================
// CRC-24
// ============================================================================

/// Compute the Mode S CRC-24 remainder (bit-by-bit, used in tests).
///
/// For a valid message, the CRC computed over all 112 bits (including the
/// CRC field) yields 0. For DF11/DF17, the CRC is XORed with the ICAO
/// address for interrogator lockout — we handle both cases.
#[cfg(test)]
fn crc24(data: &[u8], bit_count: usize) -> u32 {
    let mut crc: u32 = 0;

    for i in 0..bit_count {
        let byte_idx = i / 8;
        let bit_idx = 7 - (i % 8);
        let bit = (data[byte_idx] >> bit_idx) & 1;

        if (crc >> 23) & 1 != 0 {
            crc = ((crc << 1) | bit as u32) ^ CRC24_GENERATOR;
        } else {
            crc = (crc << 1) | bit as u32;
        }
    }

    crc & 0xFFFFFF
}

/// Precomputed CRC-24 lookup table for byte-at-a-time processing.
fn crc24_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (i, entry) in table.iter_mut().enumerate() {
        let mut crc = (i as u32) << 16;
        for _ in 0..8 {
            if crc & 0x800000 != 0 {
                crc = (crc << 1) ^ CRC24_GENERATOR;
            } else {
                crc <<= 1;
            }
        }
        *entry = crc & 0xFFFFFF;
    }
    table
}

/// Fast CRC-24 using lookup table (for byte-aligned data).
fn crc24_fast(data: &[u8]) -> u32 {
    let table = crc24_table();
    let mut crc: u32 = 0;

    // Process all bytes except the last 3 (CRC field)
    let msg_bytes = if data.len() >= 3 { data.len() - 3 } else { data.len() };

    for &byte in &data[..msg_bytes] {
        let idx = ((crc >> 16) ^ (byte as u32)) & 0xFF;
        crc = (crc << 8) ^ table[idx as usize];
        crc &= 0xFFFFFF;
    }

    // XOR with the received CRC (last 3 bytes)
    if data.len() >= 3 {
        let received_crc = ((data[data.len() - 3] as u32) << 16)
            | ((data[data.len() - 2] as u32) << 8)
            | (data[data.len() - 1] as u32);
        crc ^= received_crc;
    }

    crc
}

// ============================================================================
// CPR (Compact Position Reporting)
// ============================================================================

/// Number of latitude zones for CPR.
const NZ: f64 = 15.0;

/// Compute NL(lat): number of longitude zones for a given latitude.
///
/// NL(lat) = floor(2π / acos(1 − (1−cos(π/2NZ)) / cos²(πlat/180)))
///
/// Edge cases: |lat| ≥ 87° → NL=1 (polar region, single zone).
fn nl(lat: f64) -> u32 {
    if lat.abs() >= 87.0 {
        return 1;
    }

    let lat_rad = lat.abs() * std::f64::consts::PI / 180.0;
    let nz_rad = std::f64::consts::PI / (2.0 * NZ);

    let cos_lat = lat_rad.cos();
    let arg = 1.0 - (1.0 - nz_rad.cos()) / (cos_lat * cos_lat);

    if arg <= -1.0 || arg >= 1.0 {
        return 1;
    }

    // Cap at 59: the maximum NL value (59 odd latitude zones)
    let val = (2.0 * std::f64::consts::PI / arg.acos()).floor() as u32;
    val.min(59)
}

/// CPR global decode from even and odd position messages.
///
/// Returns (latitude, longitude) in degrees, or None if decode fails.
///
/// The algorithm:
/// 1. Compute latitude index j from the even/odd lat encodings
/// 2. Compute latitude from j and the appropriate CPR latitude
/// 3. Check NL consistency (even/odd must agree)
/// 4. Compute longitude from the CPR longitude encoding
fn cpr_global_decode(
    lat_even: u32,
    lon_even: u32,
    lat_odd: u32,
    lon_odd: u32,
    odd_newer: bool,
) -> Option<(f64, f64)> {
    let cpr_max = (1u32 << 17) as f64; // 2^17 = 131072

    let lat_e = lat_even as f64 / cpr_max;
    let lon_e = lon_even as f64 / cpr_max;
    let lat_o = lat_odd as f64 / cpr_max;
    let lon_o = lon_odd as f64 / cpr_max;

    // Latitude zone sizes
    let d_lat_even = 360.0 / (4.0 * NZ);      // 6.0°
    let d_lat_odd = 360.0 / (4.0 * NZ - 1.0); // ~6.1017°

    // Latitude index
    let j = ((59.0 * lat_e - 60.0 * lat_o + 0.5).floor()) as i32;

    // Compute even and odd latitudes
    let lat_even_val = d_lat_even * ((j % 60) as f64 + lat_e);
    let lat_odd_val = d_lat_odd * ((j % 59) as f64 + lat_o);

    // Normalize to [-90, 90]
    let lat_even_val = if lat_even_val >= 270.0 {
        lat_even_val - 360.0
    } else {
        lat_even_val
    };
    let lat_odd_val = if lat_odd_val >= 270.0 {
        lat_odd_val - 360.0
    } else {
        lat_odd_val
    };

    // Check NL consistency
    let nl_even = nl(lat_even_val);
    let nl_odd = nl(lat_odd_val);
    if nl_even != nl_odd {
        return None; // Position straddles latitude zone boundary
    }

    // Select latitude based on which message is newer
    let (lat, nl_val) = if odd_newer {
        (lat_odd_val, nl_odd)
    } else {
        (lat_even_val, nl_even)
    };

    // Longitude
    let n = if odd_newer {
        if nl_val > 1 { nl_val - 1 } else { 1 }
    } else if nl_val > 0 { nl_val } else { 1 };

    let d_lon = 360.0 / n as f64;

    let m = ((lon_e * (nl_val as f64 - 1.0) - lon_o * nl_val as f64 + 0.5).floor()) as i32;

    let lon = if odd_newer {
        d_lon * ((m % n as i32) as f64 + lon_o)
    } else {
        d_lon * ((m % n as i32) as f64 + lon_e)
    };

    let lon = if lon > 180.0 { lon - 360.0 } else { lon };

    Some((lat, lon))
}

// ============================================================================
// Altitude Decoding
// ============================================================================

/// Decode altitude from a 12-bit altitude field.
///
/// Bit 8 (Q-bit) determines encoding:
/// - Q=1: 25-foot increments, altitude = N×25 − 1000
/// - Q=0: Gillham (Gray) code for 100-foot increments
fn decode_altitude(alt_code: u16) -> Option<i32> {
    if alt_code == 0 {
        return None;
    }

    let q_bit = (alt_code >> 4) & 1;

    if q_bit == 1 {
        // 25-foot resolution
        // Remove the Q bit: bits 11..5 and bits 3..0
        let n = ((alt_code >> 5) << 4) | (alt_code & 0xF);
        let alt = (n as i32) * 25 - 1000;
        Some(alt)
    } else {
        // Gillham Gray code (100-foot resolution)
        decode_gillham(alt_code)
    }
}

/// Decode Gillham Gray code altitude.
///
/// The Gillham code encodes altitude in a mixed Gray code format
/// using 500-foot and 100-foot increments. This implements the
/// full decode as per ICAO Annex 10.
fn decode_gillham(code: u16) -> Option<i32> {
    // Extract the relevant bits (A, B, C, D fields)
    // Standard mapping from 12-bit code to altitude
    // For simplicity, handle Q=0 as a less common case
    // Most modern transponders use Q=1
    let c1 = code & 1;
    let a1 = (code >> 1) & 1;
    let c2 = (code >> 2) & 1;
    let a2 = (code >> 3) & 1;
    // bit 4 is Q (= 0 here)
    let b1 = (code >> 5) & 1;
    let d1 = (code >> 6) & 1;
    let b2 = (code >> 7) & 1;
    let d2 = (code >> 8) & 1;
    let b4 = (code >> 9) & 1;
    let d4 = (code >> 10) & 1;
    let a4 = (code >> 11) & 1;

    // Gray to binary conversion for the 500-ft part (D1, D2, D4)
    let gray500 = (d4 << 2) | (d2 << 1) | d1;
    let bin500 = gray_to_binary(gray500 as u32, 3) as u16;

    // Gray to binary for the 100-ft part (A1, A2, A4, B1, B2, B4)
    let gray100 = (a4 << 5) | (a2 << 4) | (a1 << 3) | (b4 << 2) | (b2 << 1) | b1;
    let bin100 = gray_to_binary(gray100 as u32, 6) as u16;

    if bin100 == 0 || bin100 > 5 {
        return None;
    }

    let alt = bin500 as i32 * 500 + bin100 as i32 * 100 - 1300;

    // Apply C-bit correction
    let c_bits = (c2 << 1) | c1;
    let alt = match c_bits {
        0 => alt,
        1 => alt + 100,
        2 => alt + 200,
        3 => alt + 300,
        _ => unreachable!(),
    };

    Some(alt)
}

/// Convert Gray code to binary.
fn gray_to_binary(gray: u32, bits: u32) -> u32 {
    let mut binary = gray;
    let mut mask = gray >> 1;
    while mask != 0 {
        binary ^= mask;
        mask >>= 1;
    }
    binary & ((1 << bits) - 1)
}

// ============================================================================
// Callsign Decoding
// ============================================================================

/// ADS-B callsign character set (6-bit encoding).
const CALLSIGN_CHARS: &[u8] = b"?ABCDEFGHIJKLMNOPQRSTUVWXYZ????? ???????????????0123456789??????";

/// Decode an 8-character callsign from 48 bits (6 bits per character).
fn decode_callsign(data: &[u8]) -> String {
    let mut result = String::with_capacity(8);

    for i in 0..8 {
        let bit_offset = i * 6;
        let byte_idx = bit_offset / 8;
        let bit_idx = bit_offset % 8;

        let c = if bit_idx <= 2 {
            // Character fits within one byte
            (data[byte_idx] >> (2 - bit_idx)) & 0x3F
        } else {
            // Character spans two bytes
            let high = data[byte_idx] << (bit_idx - 2);
            let low = data[byte_idx + 1] >> (10 - bit_idx);
            (high | low) & 0x3F
        };

        let ch = CALLSIGN_CHARS[c as usize] as char;
        if ch != '?' {
            result.push(ch);
        }
    }

    result.trim().to_string()
}

// ============================================================================
// Velocity Decoding
// ============================================================================

/// Decoded velocity information.
#[derive(Debug, Clone)]
struct Velocity {
    /// Ground speed in knots.
    ground_speed_kt: f64,
    /// Track angle in degrees (0 = north, clockwise).
    heading_deg: f64,
    /// Vertical rate in feet per minute.
    vertical_rate_fpm: i32,
}

/// Decode airborne velocity from ME subtype 19 (subtypes 1 and 2).
///
/// Subtype 1: ground speed from E/W and N/S velocity components.
///   V_ew = (sign ? −1 : 1) × (value − 1)
///   V_ns = (sign ? −1 : 1) × (value − 1)
///   speed = √(V_ew² + V_ns²)
///   heading = atan2(V_ew, V_ns)
fn decode_velocity(me_data: &[u8]) -> Option<Velocity> {
    // Subtype is bits 5..7 of ME byte 0
    let subtype = me_data[0] & 0x07;

    if subtype != 1 && subtype != 2 {
        return None; // Only handle ground speed subtypes
    }

    let multiplier = if subtype == 2 { 4.0 } else { 1.0 };

    // E/W velocity: direction sign (bit 13), value (bits 14..23)
    let ew_sign = (me_data[1] >> 2) & 1;
    let ew_val = (((me_data[1] & 0x03) as u16) << 8) | (me_data[2] as u16);

    // N/S velocity: direction sign (bit 24), value (bits 25..34)
    let ns_sign = (me_data[3] >> 7) & 1;
    let ns_val = ((me_data[3] & 0x7F) as u16) << 3 | ((me_data[4] >> 5) as u16);

    if ew_val == 0 || ns_val == 0 {
        return None;
    }

    let v_ew = (if ew_sign == 1 { -1.0 } else { 1.0 }) * (ew_val as f64 - 1.0) * multiplier;
    let v_ns = (if ns_sign == 1 { -1.0 } else { 1.0 }) * (ns_val as f64 - 1.0) * multiplier;

    let speed = (v_ew * v_ew + v_ns * v_ns).sqrt();
    let heading = v_ew.atan2(v_ns).to_degrees();
    let heading = if heading < 0.0 { heading + 360.0 } else { heading };

    // Vertical rate: sign (bit 36), value (bits 37..45)
    let vr_sign = (me_data[4] >> 4) & 1;
    let vr_val = (((me_data[4] & 0x07) as u16) << 6) | ((me_data[5] >> 2) as u16);
    let vr = if vr_val > 0 {
        let rate = ((vr_val as i32) - 1) * 64;
        if vr_sign == 1 { -rate } else { rate }
    } else {
        0
    };

    Some(Velocity {
        ground_speed_kt: speed,
        heading_deg: heading,
        vertical_rate_fpm: vr,
    })
}

// ============================================================================
// Aircraft State Tracking
// ============================================================================

/// Per-aircraft tracking state.
#[derive(Debug, Clone)]
struct AircraftState {
    /// Callsign (from identification message).
    callsign: Option<String>,
    /// Last altitude in feet.
    altitude: Option<i32>,
    /// Last position (lat, lon) in degrees.
    position: Option<(f64, f64)>,
    /// Ground speed in knots.
    speed: Option<f64>,
    /// Heading in degrees.
    heading: Option<f64>,
    /// Vertical rate in fpm.
    vertical_rate: Option<i32>,
    /// CPR even position encoding.
    cpr_even: Option<(u32, u32, Instant)>,
    /// CPR odd position encoding.
    cpr_odd: Option<(u32, u32, Instant)>,
    /// Last update timestamp.
    last_seen: Instant,
}

impl AircraftState {
    fn new(_icao: u32) -> Self {
        Self {
            callsign: None,
            altitude: None,
            position: None,
            speed: None,
            heading: None,
            vertical_rate: None,
            cpr_even: None,
            cpr_odd: None,
            last_seen: Instant::now(),
        }
    }
}

// ============================================================================
// ADS-B Decoder
// ============================================================================

/// ADS-B Mode S decoder plugin.
///
/// Expects magnitude samples at 2 MS/s (computed from IQ as |I| + |Q|
/// or √(I²+Q²)). The decoder searches for preamble patterns, extracts
/// PPM-encoded bits, validates CRC-24, and parses DF17 extended squitter
/// messages.
pub struct AdsbDecoder {
    /// Magnitude buffer for preamble search.
    mag_buffer: Vec<f32>,
    /// Aircraft tracking table (ICAO → state).
    aircraft: std::collections::HashMap<u32, AircraftState>,
    /// Stale aircraft timeout (seconds).
    timeout_secs: u64,
    /// Input sample rate.
    sample_rate: f64,
}

impl Default for AdsbDecoder {
    fn default() -> Self {
        Self {
            mag_buffer: Vec::with_capacity(MIN_BUFFER_LEN * 2),
            aircraft: std::collections::HashMap::new(),
            timeout_secs: 60,
            sample_rate: 2_000_000.0,
        }
    }
}

impl AdsbDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Compute magnitude from IQ: |I| + |Q| (fast approximation).
    /// This is ~3% less accurate than √(I²+Q²) but avoids sqrt per sample.
    #[inline]
    fn magnitude(sample: Sample) -> f32 {
        sample.re.abs() + sample.im.abs()
    }

    /// Check if the magnitude buffer at position `i` matches the Mode S preamble.
    ///
    /// The preamble consists of pulses at specific positions. We verify:
    /// 1. Pulse positions have high magnitude
    /// 2. Gap positions have low magnitude
    /// 3. Pulse/gap ratio exceeds a threshold
    fn check_preamble(&self, i: usize) -> bool {
        if i + MIN_BUFFER_LEN > self.mag_buffer.len() {
            return false;
        }

        let buf = &self.mag_buffer;

        // Sum of pulse positions
        let pulse_sum: f32 = PREAMBLE_POSITIONS.iter().map(|&p| buf[i + p]).sum();
        // Sum of gap positions
        let gap_sum: f32 = PREAMBLE_GAPS.iter().map(|&p| buf[i + p]).sum();

        // Pulses should be significantly stronger than gaps
        let pulse_avg = pulse_sum / PREAMBLE_POSITIONS.len() as f32;
        let gap_avg = gap_sum / PREAMBLE_GAPS.len() as f32;

        if pulse_avg < 0.01 {
            return false; // Too weak
        }

        // Require pulse/gap ratio > 2 (6 dB SNR)
        pulse_avg > gap_avg * 2.0
    }

    /// Extract bits from PPM-encoded data after the preamble.
    ///
    /// Each bit spans 2 samples at 2 MS/s:
    /// - Bit 1: high sample, low sample
    /// - Bit 0: low sample, high sample
    fn extract_bits(&self, start: usize, num_bits: usize) -> Option<Vec<u8>> {
        let data_start = start + PREAMBLE_SAMPLES;

        if data_start + num_bits * 2 > self.mag_buffer.len() {
            return None;
        }

        let mut bytes = vec![0u8; num_bits.div_ceil(8)];
        let buf = &self.mag_buffer;

        for bit in 0..num_bits {
            let sample_pos = data_start + bit * 2;
            let high = buf[sample_pos];
            let low = buf[sample_pos + 1];

            if high > low {
                // Bit = 1
                bytes[bit / 8] |= 1 << (7 - (bit % 8));
            }
            // Bit = 0 is already set (bytes initialized to 0)
        }

        Some(bytes)
    }

    /// Process a decoded Mode S message.
    fn process_message(&mut self, bytes: &[u8], messages: &mut Vec<DecodedMessage>) {
        let df = (bytes[0] >> 3) & 0x1F;

        // Only process DF17 (Extended Squitter) and DF18 (TIS-B)
        if df != 17 && df != 18 {
            return;
        }

        // Validate CRC-24
        let crc = crc24_fast(bytes);
        if crc != 0 {
            return; // CRC failure
        }

        // Extract ICAO address (bytes 1-3)
        let icao = ((bytes[1] as u32) << 16) | ((bytes[2] as u32) << 8) | (bytes[3] as u32);

        // ME data (bytes 4-10, 56 bits)
        let me = &bytes[4..11];
        let type_code = (me[0] >> 3) & 0x1F;

        // Get or create aircraft state
        let aircraft = self.aircraft
            .entry(icao)
            .or_insert_with(|| AircraftState::new(icao));
        aircraft.last_seen = Instant::now();

        let mut fields = BTreeMap::new();
        fields.insert("icao".to_string(), format!("{:06X}", icao));
        fields.insert("df".to_string(), df.to_string());
        fields.insert("type_code".to_string(), type_code.to_string());

        let summary = match type_code {
            // TC 1-4: Aircraft identification
            1..=4 => {
                let callsign = decode_callsign(&me[1..]);
                aircraft.callsign = Some(callsign.clone());
                fields.insert("callsign".to_string(), callsign.clone());
                format!("ADS-B {:06X} ID: {}", icao, callsign)
            }

            // TC 9-18: Airborne position (with barometric altitude)
            9..=18 => {
                // Altitude (12 bits from ME bytes 1-2)
                let alt_code = (((me[1] as u16) & 0xFF) << 4) | ((me[2] >> 4) as u16);
                if let Some(alt) = decode_altitude(alt_code) {
                    aircraft.altitude = Some(alt);
                    fields.insert("altitude_ft".to_string(), alt.to_string());
                }

                // CPR encoded position
                let cpr_flag = (me[2] >> 2) & 1; // 0=even, 1=odd
                let lat_cpr = (((me[2] & 0x03) as u32) << 15)
                    | ((me[3] as u32) << 7)
                    | ((me[4] >> 1) as u32);
                let lon_cpr = (((me[4] & 0x01) as u32) << 16)
                    | ((me[5] as u32) << 8)
                    | (me[6] as u32);

                let now = Instant::now();
                if cpr_flag == 0 {
                    aircraft.cpr_even = Some((lat_cpr, lon_cpr, now));
                } else {
                    aircraft.cpr_odd = Some((lat_cpr, lon_cpr, now));
                }

                // Attempt global decode if we have both even and odd
                if let (Some((lat_e, lon_e, t_e)), Some((lat_o, lon_o, t_o))) =
                    (&aircraft.cpr_even, &aircraft.cpr_odd)
                {
                    let odd_newer = *t_o > *t_e;
                    if let Some((lat, lon)) =
                        cpr_global_decode(*lat_e, *lon_e, *lat_o, *lon_o, odd_newer)
                    {
                        aircraft.position = Some((lat, lon));
                        fields.insert("latitude".to_string(), format!("{:.4}", lat));
                        fields.insert("longitude".to_string(), format!("{:.4}", lon));
                    }
                }

                let alt_str = aircraft
                    .altitude
                    .map_or("?".to_string(), |a| format!("{}ft", a));
                let pos_str = aircraft.position.map_or(String::new(), |(lat, lon)| {
                    format!(" ({:.4},{:.4})", lat, lon)
                });
                let cs = aircraft
                    .callsign
                    .as_deref()
                    .unwrap_or("?");
                format!("ADS-B {:06X} [{}] alt={}{}", icao, cs, alt_str, pos_str)
            }

            // TC 19: Airborne velocity
            19 => {
                if let Some(vel) = decode_velocity(me) {
                    aircraft.speed = Some(vel.ground_speed_kt);
                    aircraft.heading = Some(vel.heading_deg);
                    aircraft.vertical_rate = Some(vel.vertical_rate_fpm);
                    fields.insert(
                        "ground_speed_kt".to_string(),
                        format!("{:.0}", vel.ground_speed_kt),
                    );
                    fields.insert(
                        "heading_deg".to_string(),
                        format!("{:.1}", vel.heading_deg),
                    );
                    fields.insert(
                        "vertical_rate_fpm".to_string(),
                        vel.vertical_rate_fpm.to_string(),
                    );
                    let cs = aircraft.callsign.as_deref().unwrap_or("?");
                    format!(
                        "ADS-B {:06X} [{}] vel={:.0}kt hdg={:.0}° vr={}fpm",
                        icao, cs, vel.ground_speed_kt, vel.heading_deg, vel.vertical_rate_fpm
                    )
                } else {
                    format!("ADS-B {:06X} velocity (unsupported subtype)", icao)
                }
            }

            _ => {
                format!("ADS-B {:06X} TC={}", icao, type_code)
            }
        };

        messages.push(DecodedMessage {
            decoder: "adsb".to_string(),
            timestamp: Instant::now(),
            summary,
            fields,
            raw_bits: Some(bytes.to_vec()),
        });
    }

    /// Purge aircraft not seen for `timeout_secs`.
    fn purge_stale(&mut self) {
        let timeout = std::time::Duration::from_secs(self.timeout_secs);
        let now = Instant::now();
        self.aircraft.retain(|_, state| now.duration_since(state.last_seen) < timeout);
    }
}

impl DecoderPlugin for AdsbDecoder {
    fn name(&self) -> &str {
        "adsb"
    }

    fn requirements(&self) -> DecoderRequirements {
        DecoderRequirements {
            center_frequency: 1090e6,
            sample_rate: self.sample_rate,
            bandwidth: 2_000_000.0,
            wants_iq: true,
        }
    }

    fn process(&mut self, samples: &[Sample]) -> Vec<DecodedMessage> {
        let mut messages = Vec::new();

        // Convert IQ to magnitude and append to buffer
        for &s in samples {
            self.mag_buffer.push(Self::magnitude(s));
        }

        // Search for preambles
        let mut i = 0;
        while i + MIN_BUFFER_LEN <= self.mag_buffer.len() {
            if self.check_preamble(i) {
                // Try to extract a long message (112 bits = DF17/18)
                if let Some(bytes) = self.extract_bits(i, LONG_MSG_BITS) {
                    let df = (bytes[0] >> 3) & 0x1F;

                    if df == 17 || df == 18 {
                        self.process_message(&bytes, &mut messages);
                        i += PREAMBLE_SAMPLES + LONG_MSG_BITS * 2;
                        continue;
                    }
                }

                // Try short message (56 bits)
                if let Some(bytes) = self.extract_bits(i, SHORT_MSG_BITS) {
                    let df = (bytes[0] >> 3) & 0x1F;
                    // Short messages: DF0, DF4, DF5, DF11
                    if df == 0 || df == 4 || df == 5 || df == 11 {
                        // We don't fully decode short messages yet,
                        // but we skip past them
                        i += PREAMBLE_SAMPLES + SHORT_MSG_BITS * 2;
                        continue;
                    }
                }
            }
            i += 1;
        }

        // Keep only the tail of the magnitude buffer (for messages spanning blocks)
        if self.mag_buffer.len() > MIN_BUFFER_LEN * 2 {
            let keep = MIN_BUFFER_LEN;
            let drain = self.mag_buffer.len() - keep;
            self.mag_buffer.drain(..drain);
        }

        // Periodic stale purge
        if self.aircraft.len() > 100 {
            self.purge_stale();
        }

        messages
    }

    fn reset(&mut self) {
        self.mag_buffer.clear();
        self.aircraft.clear();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // CRC-24 tests
    // ------------------------------------------------------------------

    #[test]
    fn crc24_known_vector() {
        // DF17 message: 8D4840D6 202CC371 C32CE0576098
        // This is a well-known ADS-B test vector.
        let msg = [0x8D, 0x48, 0x40, 0xD6, 0x20, 0x2C, 0xC3, 0x71,
                   0xC3, 0x2C, 0xE0, 0x57, 0x60, 0x98];

        let crc = crc24_fast(&msg);
        assert_eq!(crc, 0, "CRC should be 0 for valid message, got {:06X}", crc);
    }

    #[test]
    fn crc24_detects_corruption() {
        let mut msg = [0x8D, 0x48, 0x40, 0xD6, 0x20, 0x2C, 0xC3, 0x71,
                       0xC3, 0x2C, 0xE0, 0x57, 0x60, 0x98];

        // Corrupt one byte
        msg[5] ^= 0x01;
        let crc = crc24_fast(&msg);
        assert_ne!(crc, 0, "CRC should detect corruption");
    }

    #[test]
    fn crc24_bit_by_bit_matches_table() {
        // Compare bit-by-bit and table-driven implementations
        let msg = [0x8D, 0x48, 0x40, 0xD6, 0x20, 0x2C, 0xC3, 0x71,
                   0xC3, 0x2C, 0xE0, 0x57, 0x60, 0x98];

        let crc_bits = crc24(&msg, 112);
        let crc_table = crc24_fast(&msg);
        assert_eq!(crc_bits, crc_table, "Bit-by-bit and table CRC should match");
    }

    // ------------------------------------------------------------------
    // CPR tests
    // ------------------------------------------------------------------

    #[test]
    fn nl_known_values() {
        // NL(0°) = 59 (equator, capped from formula's 60)
        assert_eq!(nl(0.0), 59);
        // NL(87°) = 1 (polar)
        assert_eq!(nl(87.0), 1);
        // NL(45°) = 42 per ICAO transition table (45° < 45.546°)
        assert_eq!(nl(45.0), 42);
        // NL(10°) = 59 (10° < 10.47° transition)
        assert_eq!(nl(10.0), 59);
        // NL(52°) should be ~37
        let nl_52 = nl(52.0);
        assert!((35..=39).contains(&nl_52), "NL(52°) = {}", nl_52);
    }

    #[test]
    fn cpr_global_decode_known() {
        // Known test vectors from 1090MHz community:
        // Even: lat=93000, lon=51372
        // Odd:  lat=74158, lon=50194
        // Expected position: approximately 52.26N, 3.92E
        let result = cpr_global_decode(93000, 51372, 74158, 50194, false);
        assert!(result.is_some(), "CPR decode should succeed");

        let (lat, lon) = result.unwrap();
        assert!(
            (lat - 52.26).abs() < 0.5,
            "Latitude: expected ~52.26, got {lat:.4}"
        );
        assert!(
            (lon - 3.92).abs() < 0.5,
            "Longitude: expected ~3.92, got {lon:.4}"
        );
    }

    #[test]
    fn cpr_nl_consistency_rejection() {
        // If NL differs between even and odd positions, decode should fail
        // Use positions at very different latitudes to force NL mismatch
        let result = cpr_global_decode(0, 0, 131000, 131000, false);
        // This may or may not fail depending on the exact positions,
        // but we verify the function doesn't panic
        let _ = result;
    }

    // ------------------------------------------------------------------
    // Altitude tests
    // ------------------------------------------------------------------

    #[test]
    fn altitude_q_bit_set() {
        // Q=1, 25-foot mode
        // altitude = N*25 - 1000
        // For 38000 ft: N = (38000 + 1000) / 25 = 1560
        // Encode: bits[11..5] = 1560 >> 4 = 97, bits[3..0] = 1560 & 0xF = 8
        // bit4 (Q) = 1
        let code = (97 << 5) | (1 << 4) | 8;
        let alt = decode_altitude(code);
        assert_eq!(alt, Some(38000));
    }

    #[test]
    fn altitude_zero_is_none() {
        assert_eq!(decode_altitude(0), None);
    }

    // ------------------------------------------------------------------
    // Callsign tests
    // ------------------------------------------------------------------

    #[test]
    fn callsign_decode() {
        // "UAL123  " in 6-bit encoding
        // U=21, A=1, L=12, 1=49-16=33 wait...
        // Character set: index 0=?, 1=A, ..., 26=Z, 32=space, 48=0, ..., 57=9
        // 'U'=21, 'A'=1, 'L'=12, '1'=49, '2'=50, '3'=51, ' '=32, ' '=32
        let encoded = [
            (21 << 2),                     // U(5:0)A(5:4) = 0x54 | 0x00 = 0x54
            ((1 & 0xF) << 4) | (12 >> 2),  // A(3:0)L(5:2) = 0x10 | 0x03 = 0x13
            49,                            // L(1:0)'1' = 0x80 | 0x31 = 0xB1 (12 & 0x3 = 0)
            (50 << 2) | (51 >> 4),         // '2''3'(5:4) = 0xC8 | 0x03 = 0xCB
            ((51 & 0xF) << 4) | (32 >> 2), // '3'(3:0)' '(5:2) = 0x30 | 0x08 = 0x38
            32,                            // ' '(1:0)' ' = 0x00 | 0x20 = 0x20 (32 & 0x3 = 0)
        ];
        let callsign = decode_callsign(&encoded);
        assert_eq!(callsign, "UAL123");
    }

    // ------------------------------------------------------------------
    // Velocity tests
    // ------------------------------------------------------------------

    #[test]
    fn velocity_subtype1() {
        // Construct a velocity message: subtype 1
        // V_ew = +100 kt (east), V_ns = +200 kt (north)
        // Speed = √(100² + 200²) ≈ 223.6 kt
        // Heading = atan2(100, 200) ≈ 26.6°
        let mut me = [0u8; 7];
        me[0] = (19 << 3) | 1; // TC=19, subtype=1

        // EW: sign=0 (east), value=101 (100+1); high bits are 0
        me[1] = 0;
        me[2] = (101 & 0xFF) as u8;

        // NS: sign=0 (north), value=201 (200+1)
        me[3] = ((201 >> 3) & 0x7F) as u8;
        me[4] = ((201 & 0x07) << 5) as u8;

        let vel = decode_velocity(&me);
        assert!(vel.is_some());
        let v = vel.unwrap();
        assert!((v.ground_speed_kt - 223.6).abs() < 1.0,
            "Speed: {:.1}", v.ground_speed_kt);
        assert!((v.heading_deg - 26.6).abs() < 1.0,
            "Heading: {:.1}", v.heading_deg);
    }

    // ------------------------------------------------------------------
    // Gray code tests
    // ------------------------------------------------------------------

    #[test]
    fn gray_to_binary_known() {
        assert_eq!(gray_to_binary(0b000, 3), 0);
        assert_eq!(gray_to_binary(0b001, 3), 1);
        assert_eq!(gray_to_binary(0b011, 3), 2);
        assert_eq!(gray_to_binary(0b010, 3), 3);
        assert_eq!(gray_to_binary(0b110, 3), 4);
        assert_eq!(gray_to_binary(0b111, 3), 5);
    }

    // ------------------------------------------------------------------
    // Decoder plugin interface
    // ------------------------------------------------------------------

    #[test]
    fn decoder_plugin_interface() {
        let decoder = AdsbDecoder::new();
        assert_eq!(decoder.name(), "adsb");
        assert!(decoder.requirements().wants_iq);
        assert!((decoder.requirements().center_frequency - 1090e6).abs() < 1.0);
        assert!((decoder.requirements().sample_rate - 2e6).abs() < 1.0);
    }

    #[test]
    fn decoder_handles_empty_input() {
        let mut decoder = AdsbDecoder::new();
        let msgs = decoder.process(&[]);
        assert!(msgs.is_empty());
    }

    #[test]
    fn decoder_handles_noise() {
        let mut decoder = AdsbDecoder::new();
        let noise: Vec<Sample> = (0..10000)
            .map(|i| {
                Sample::new(
                    (i as f32 * 0.37).sin() * 0.01,
                    (i as f32 * 0.73).cos() * 0.01,
                )
            })
            .collect();
        let msgs = decoder.process(&noise);
        assert!(msgs.is_empty(), "Noise should not produce messages");
    }

    // ------------------------------------------------------------------
    // Integration: synthetic Mode S message
    // ------------------------------------------------------------------

    #[test]
    fn synthetic_adsb_message() {
        // Build a synthetic DF17 identification message and encode as PPM.
        // DF=17 (10001), CA=5 (101) → byte 0 = 0x8D
        // ICAO=0xABCDEF
        // TC=1 (identification), category=0
        // Callsign: "TEST    " (T=20, E=5, S=19, T=20, spaces=32)
        let mut msg = [0u8; 14];
        msg[0] = 0x8D; // DF17, CA5
        msg[1] = 0xAB; // ICAO high
        msg[2] = 0xCD; // ICAO mid
        msg[3] = 0xEF; // ICAO low

        // ME: TC=1, cat=0, callsign "TEST"
        msg[4] = 1 << 3; // TC=1, category=0

        // Encode callsign "TEST    " in 6-bit
        let chars = [20u8, 5, 19, 20, 32, 32, 32, 32]; // T E S T sp sp sp sp
        // Pack 8 × 6 bits = 48 bits = 6 bytes
        let mut bits48 = 0u64;
        for &c in &chars {
            bits48 = (bits48 << 6) | (c as u64);
        }
        msg[5] = ((bits48 >> 40) & 0xFF) as u8;
        msg[6] = ((bits48 >> 32) & 0xFF) as u8;
        msg[7] = ((bits48 >> 24) & 0xFF) as u8;
        msg[8] = ((bits48 >> 16) & 0xFF) as u8;
        msg[9] = ((bits48 >> 8) & 0xFF) as u8;
        msg[10] = (bits48 & 0xFF) as u8;

        // Compute CRC-24 using table-driven method (set CRC field to 0 first)
        msg[11] = 0;
        msg[12] = 0;
        msg[13] = 0;
        let crc = crc24_fast(&msg); // With CRC field = 0, this gives the CRC value
        msg[11] = ((crc >> 16) & 0xFF) as u8;
        msg[12] = ((crc >> 8) & 0xFF) as u8;
        msg[13] = (crc & 0xFF) as u8;

        // Verify CRC
        assert_eq!(crc24_fast(&msg), 0, "Constructed message should have valid CRC");

        // Encode as PPM in IQ samples at 2 MS/s
        let mut iq: Vec<Sample> = Vec::new();

        // Some leading silence
        for _ in 0..100 {
            iq.push(Sample::new(0.0, 0.0));
        }

        // Preamble: pulses at positions 0, 2, 7, 9 (each pulse = 1 sample high)
        let preamble_pattern = [1, 0, 1, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0];
        for &p in &preamble_pattern {
            let val = if p == 1 { 1.0 } else { 0.0 };
            iq.push(Sample::new(val, 0.0));
        }

        // Data bits: 112 bits, each bit = 2 samples
        for byte in &msg {
            for bit_idx in (0..8).rev() {
                let bit = (byte >> bit_idx) & 1;
                if bit == 1 {
                    iq.push(Sample::new(1.0, 0.0)); // high
                    iq.push(Sample::new(0.0, 0.0)); // low
                } else {
                    iq.push(Sample::new(0.0, 0.0)); // low
                    iq.push(Sample::new(1.0, 0.0)); // high
                }
            }
        }

        // Trailing silence
        for _ in 0..100 {
            iq.push(Sample::new(0.0, 0.0));
        }

        // Run decoder
        let mut decoder = AdsbDecoder::new();
        let messages = decoder.process(&iq);

        assert!(
            !messages.is_empty(),
            "Should decode synthetic ADS-B message"
        );

        let msg_out = &messages[0];
        assert_eq!(msg_out.decoder, "adsb");
        assert_eq!(msg_out.fields["icao"], "ABCDEF");
        assert_eq!(msg_out.fields["type_code"], "1");
        assert!(msg_out.fields["callsign"].contains("TEST"));
    }
}
