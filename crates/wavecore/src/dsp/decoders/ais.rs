//! AIS (Automatic Identification System) Decoder
//!
//! Decodes maritime vessel transponder messages from GMSK-modulated IQ
//! samples. AIS uses 9600 baud GMSK (BT=0.4) on VHF channels:
//! - Channel A: 161.975 MHz (AIS 1)
//! - Channel B: 162.025 MHz (AIS 2)
//!
//! ## Protocol Structure
//!
//! ```text
//! ┌───────────┬──────┬─────────────────────────┬────────┬──────┐
//! │ Training  │ Flag │ Payload (168-1008 bits)  │ CRC-16 │ Flag │
//! │ 24+ bits  │ 0x7E │ 6-bit ASCII encoded      │ CCITT  │ 0x7E │
//! └───────────┴──────┴─────────────────────────┴────────┴──────┘
//! ```
//!
//! ## Signal Processing Chain
//!
//! ```text
//! IQ samples → FM discriminator → Gaussian matched filter
//!   → Clock recovery (9600 baud) → Bit slicer
//!   → NRZI decode → HDLC destuff → CRC-16 validate
//!   → 6-bit ASCII unpack → Message type parse
//! ```

use std::collections::{BTreeMap, HashMap};
use std::f64::consts::PI;
use std::time::Instant;

use crate::dsp::decoder::{DecoderPlugin, DecoderRequirements};
use crate::session::DecodedMessage;
use crate::types::Sample;

use super::util::{self, ClockRecovery, HdlcDeframer, NrziDecoder};

// ============================================================================
// Constants
// ============================================================================

/// AIS Channel A frequency.
const AIS_CHANNEL_A: f64 = 161.975e6;
/// AIS Channel B frequency.
#[cfg(test)]
const AIS_CHANNEL_B: f64 = 162.025e6;
/// AIS baud rate.
const BAUD_RATE: f64 = 9600.0;
/// Vessel state expiry (5 minutes).
const VESSEL_EXPIRY_SECS: u64 = 300;

// ============================================================================
// Gaussian Matched Filter
// ============================================================================

/// Pre-computed Gaussian matched filter coefficients for GMSK BT=0.4.
///
/// Filter length = 4 symbol periods, computed at 5 samples/symbol (48000/9600).
/// h(t) = exp(-2π²(BT)²t²/ln2) normalized to unit energy.
fn gaussian_filter_coefficients(samples_per_symbol: f64) -> Vec<f32> {
    let bt = 0.4;
    let span = 4; // filter spans 4 symbols
    let n = (span as f64 * samples_per_symbol) as usize | 1; // ensure odd for symmetry
    let mut coeffs = Vec::with_capacity(n);
    let mut energy = 0.0f64;
    let center = (n - 1) as f64 / 2.0;

    for i in 0..n {
        let t = (i as f64 - center) / samples_per_symbol;
        let alpha = 2.0 * PI * bt / (2.0 * 2.0f64.ln()).sqrt();
        let h = (-(alpha * t).powi(2) / 2.0).exp();
        coeffs.push(h as f32);
        energy += h * h;
    }

    // Normalize
    let norm = (energy).sqrt() as f32;
    if norm > 0.0 {
        for c in &mut coeffs {
            *c /= norm;
        }
    }

    coeffs
}

/// Simple FIR filter for the Gaussian matched filter.
struct FirFilter {
    coeffs: Vec<f32>,
    delay_line: Vec<f32>,
    pos: usize,
}

impl FirFilter {
    fn new(coeffs: Vec<f32>) -> Self {
        let len = coeffs.len();
        Self {
            coeffs,
            delay_line: vec![0.0; len],
            pos: 0,
        }
    }

    #[inline]
    fn process(&mut self, sample: f32) -> f32 {
        self.delay_line[self.pos] = sample;
        let mut out = 0.0f32;
        let len = self.coeffs.len();
        for i in 0..len {
            let idx = (self.pos + len - i) % len;
            out += self.delay_line[idx] * self.coeffs[i];
        }
        self.pos = (self.pos + 1) % len;
        out
    }

    fn reset(&mut self) {
        self.delay_line.fill(0.0);
        self.pos = 0;
    }
}

// ============================================================================
// AIS Message Types
// ============================================================================

/// Navigation status values (message types 1-3).
const NAV_STATUS: [&str; 16] = [
    "Under way using engine",
    "At anchor",
    "Not under command",
    "Restricted manoeuvrability",
    "Constrained by draught",
    "Moored",
    "Aground",
    "Engaged in fishing",
    "Under way sailing",
    "Reserved (HSC)",
    "Reserved (WIG)",
    "Reserved",
    "Reserved",
    "Reserved",
    "AIS-SART",
    "Not defined",
];

/// Ship type categories.
fn ship_type_name(code: u8) -> &'static str {
    match code {
        0 => "Not available",
        20..=29 => "Wing in ground",
        30 => "Fishing",
        31 => "Towing",
        32 => "Towing (large)",
        33 => "Dredging",
        34 => "Diving ops",
        35 => "Military ops",
        36 => "Sailing",
        37 => "Pleasure craft",
        40..=49 => "High speed craft",
        50 => "Pilot vessel",
        51 => "SAR vessel",
        52 => "Tug",
        53 => "Port tender",
        54 => "Anti-pollution",
        55 => "Law enforcement",
        60..=69 => "Passenger",
        70..=79 => "Cargo",
        80..=89 => "Tanker",
        90..=99 => "Other",
        _ => "Unknown",
    }
}

// ============================================================================
// AIS Payload Decoder
// ============================================================================

/// Extract bits from a 6-bit encoded AIS payload (NMEA armored format).
///
/// Each ASCII character (after adjustment) represents 6 bits of payload.
/// Character values 0-39 map from ASCII 48-87 ('0'-'W'),
/// values 40-63 map from ASCII 96-119 ('`'-'w').
fn _decode_ais_payload(data: &[u8]) -> Vec<u8> {
    let mut bits = Vec::with_capacity(data.len() * 6);
    for &byte in data {
        let val = if byte >= 96 {
            byte - 56
        } else if byte >= 48 {
            byte - 48
        } else {
            continue;
        };
        for i in (0..6).rev() {
            bits.push((val >> i) & 1);
        }
    }
    bits
}

/// Extract an unsigned integer from a bit vector (MSB first).
fn bits_to_uint(bits: &[u8], start: usize, len: usize) -> u64 {
    let mut val = 0u64;
    for i in 0..len {
        if start + i < bits.len() {
            val = (val << 1) | bits[start + i] as u64;
        }
    }
    val
}

/// Extract a signed integer from a bit vector (MSB first, two's complement).
fn bits_to_int(bits: &[u8], start: usize, len: usize) -> i64 {
    let val = bits_to_uint(bits, start, len);
    let sign_bit = 1u64 << (len - 1);
    if val & sign_bit != 0 {
        // Negative: sign-extend
        val as i64 - (1i64 << len)
    } else {
        val as i64
    }
}

/// Extract a 6-bit ASCII string from a bit vector.
///
/// AIS uses a modified 6-bit ASCII: 0-31 → '@'-'_', 32-63 → ' '-'?'
fn bits_to_string(bits: &[u8], start: usize, num_chars: usize) -> String {
    let mut result = String::with_capacity(num_chars);
    for i in 0..num_chars {
        let char_start = start + i * 6;
        let val = bits_to_uint(bits, char_start, 6) as u8;
        let c = if val < 32 {
            (val + 64) as char // '@', 'A'-'Z', '[', '\', ']', '^', '_'
        } else {
            (val) as char // ' ', '!'-'?'
        };
        result.push(c);
    }
    result.trim_end_matches('@').trim().to_string()
}

// ============================================================================
// Vessel Tracking
// ============================================================================

/// Tracked vessel state.
struct VesselInfo {
    name: Option<String>,
    last_seen: Instant,
}

// ============================================================================
// AIS Decoder
// ============================================================================

/// AIS protocol decoder plugin.
///
/// Decodes GMSK-modulated AIS messages from IQ samples using FM
/// discrimination followed by Gaussian matched filtering and clock
/// recovery at 9600 baud.
pub struct AisDecoder {
    sample_rate: f64,
    channel_freq: f64,
    /// Previous IQ sample for FM discriminator.
    prev_iq: Sample,
    /// DC removal filter state.
    dc_state: f64,
    dc_alpha: f64,
    /// Gaussian matched filter for GMSK.
    matched_filter: FirFilter,
    /// Clock recovery (9600 baud).
    clock: ClockRecovery,
    /// NRZI decoder.
    nrzi: NrziDecoder,
    /// HDLC deframer.
    hdlc: HdlcDeframer,
    /// Vessel tracking state.
    vessels: HashMap<u32, VesselInfo>,
    /// Decoder name (varies by channel).
    decoder_name: String,
}

impl AisDecoder {
    /// Create a new AIS decoder for the given channel frequency.
    pub fn new(sample_rate: f64, channel_freq: f64) -> Self {
        let decoder_name = if (channel_freq - AIS_CHANNEL_A).abs() < 1e3 {
            "ais-a"
        } else {
            "ais-b"
        };

        Self::named(sample_rate, channel_freq, decoder_name)
    }

    /// Create a new AIS decoder with an explicit runtime name.
    pub fn named(sample_rate: f64, channel_freq: f64, decoder_name: impl Into<String>) -> Self {
        let sps = sample_rate / BAUD_RATE;
        let coeffs = gaussian_filter_coefficients(sps);
        let dc_alpha = 1.0 - (-1.0 / (0.001 * sample_rate)).exp();

        Self {
            sample_rate,
            channel_freq,
            prev_iq: Sample::new(0.0, 0.0),
            dc_state: 0.0,
            dc_alpha,
            matched_filter: FirFilter::new(coeffs),
            clock: ClockRecovery::new(BAUD_RATE, sample_rate, 0.03),
            nrzi: NrziDecoder::new(),
            hdlc: HdlcDeframer::new(),
            vessels: HashMap::new(),
            decoder_name: decoder_name.into(),
        }
    }

    /// Process a raw HDLC frame into an AIS DecodedMessage.
    fn process_frame(&mut self, frame_bytes: &[u8]) -> Option<DecodedMessage> {
        // Minimum: 1 byte payload + 2 bytes CRC = 3 bytes
        if frame_bytes.len() < 5 {
            return None;
        }

        // Validate CRC-16-CCITT
        if !util::crc16_check(frame_bytes) {
            return None;
        }

        // Strip CRC
        let payload = &frame_bytes[..frame_bytes.len() - 2];

        // AIS payload is already in the HDLC info field as raw bytes.
        // Convert payload bits to AIS bit vector.
        let mut bits = Vec::with_capacity(payload.len() * 8);
        for &byte in payload {
            for i in (0..8).rev() {
                bits.push((byte >> i) & 1);
            }
        }

        if bits.len() < 38 {
            return None; // Too short for any AIS message
        }

        // Parse common header
        let msg_type = bits_to_uint(&bits, 0, 6) as u8;
        let _repeat = bits_to_uint(&bits, 6, 2) as u8;
        let mmsi = bits_to_uint(&bits, 8, 30) as u32;

        let mut fields = BTreeMap::new();
        fields.insert("mmsi".to_string(), format!("{:09}", mmsi));
        fields.insert("msg_type".to_string(), msg_type.to_string());

        let summary = match msg_type {
            1..=3 => {
                // Position Report
                self.parse_position_report(&bits, mmsi, msg_type, &mut fields)
            }
            5 => {
                // Static and Voyage Related Data
                self.parse_static_voyage(&bits, mmsi, &mut fields)
            }
            18 => {
                // Standard Class B Position Report
                self.parse_class_b_position(&bits, mmsi, &mut fields)
            }
            24 => {
                // Class B Static Data
                self.parse_class_b_static(&bits, mmsi, &mut fields)
            }
            _ => {
                format!("AIS type {} from MMSI {:09}", msg_type, mmsi)
            }
        };

        // Expire stale vessels
        let now = Instant::now();
        self.vessels
            .retain(|_, v| now.duration_since(v.last_seen).as_secs() < VESSEL_EXPIRY_SECS);

        Some(DecodedMessage {
            decoder: self.decoder_name.clone(),
            timestamp: Instant::now(),
            summary,
            fields,
            raw_bits: Some(frame_bytes.to_vec()),
        })
    }

    fn parse_position_report(
        &mut self,
        bits: &[u8],
        mmsi: u32,
        msg_type: u8,
        fields: &mut BTreeMap<String, String>,
    ) -> String {
        if bits.len() < 168 {
            return format!("AIS type {} from {:09} (truncated)", msg_type, mmsi);
        }

        let nav_status = bits_to_uint(bits, 38, 4) as usize;
        let rot_raw = bits_to_int(bits, 42, 8);
        let sog_raw = bits_to_uint(bits, 50, 10);
        let pos_acc = bits_to_uint(bits, 60, 1);
        let lon_raw = bits_to_int(bits, 61, 28);
        let lat_raw = bits_to_int(bits, 89, 27);
        let cog_raw = bits_to_uint(bits, 116, 12);
        let heading_raw = bits_to_uint(bits, 128, 9);

        // Convert position (1/10000 minute → degrees)
        let longitude = if lon_raw == 0x6791AC0 {
            None // Not available
        } else {
            Some(lon_raw as f64 / 600000.0)
        };
        let latitude = if lat_raw == 0x3412140 {
            None
        } else {
            Some(lat_raw as f64 / 600000.0)
        };

        // Speed over ground (1/10 knot)
        let sog = if sog_raw == 1023 {
            None
        } else {
            Some(sog_raw as f64 / 10.0)
        };

        // Course over ground (1/10 degree)
        let cog = if cog_raw == 3600 {
            None
        } else {
            Some(cog_raw as f64 / 10.0)
        };

        // Heading (degrees)
        let heading = if heading_raw == 511 {
            None
        } else {
            Some(heading_raw as f64)
        };

        if nav_status < NAV_STATUS.len() {
            fields.insert("nav_status".to_string(), NAV_STATUS[nav_status].to_string());
        }

        if let Some(lon) = longitude {
            fields.insert("longitude".to_string(), format!("{:.6}", lon));
        }
        if let Some(lat) = latitude {
            fields.insert("latitude".to_string(), format!("{:.6}", lat));
        }
        if let Some(s) = sog {
            fields.insert("speed_kt".to_string(), format!("{:.1}", s));
        }
        if let Some(c) = cog {
            fields.insert("course_deg".to_string(), format!("{:.1}", c));
        }
        if let Some(h) = heading {
            fields.insert("heading_deg".to_string(), format!("{:.0}", h));
        }
        if rot_raw != -128 {
            fields.insert("rot".to_string(), rot_raw.to_string());
        }
        fields.insert("position_accuracy".to_string(), pos_acc.to_string());

        // Update vessel tracking
        let name = self.vessels.get(&mmsi).and_then(|v| v.name.clone());
        self.vessels.insert(
            mmsi,
            VesselInfo {
                name: name.clone(),
                last_seen: Instant::now(),
            },
        );

        let name_str = name.map(|n| format!(" ({})", n)).unwrap_or_default();

        match (latitude, longitude) {
            (Some(lat), Some(lon)) => {
                format!("AIS {:09}{} at {:.4},{:.4}", mmsi, name_str, lat, lon)
            }
            _ => format!("AIS {:09}{} position report", mmsi, name_str),
        }
    }

    fn parse_static_voyage(
        &mut self,
        bits: &[u8],
        mmsi: u32,
        fields: &mut BTreeMap<String, String>,
    ) -> String {
        if bits.len() < 424 {
            return format!("AIS type 5 from {:09} (truncated)", mmsi);
        }

        let _imo = bits_to_uint(bits, 40, 30) as u32;
        let callsign = bits_to_string(bits, 70, 7);
        let name = bits_to_string(bits, 112, 20);
        let ship_type = bits_to_uint(bits, 232, 8) as u8;
        let dim_bow = bits_to_uint(bits, 240, 9) as u16;
        let dim_stern = bits_to_uint(bits, 249, 9) as u16;
        let dim_port = bits_to_uint(bits, 258, 6) as u16;
        let dim_starboard = bits_to_uint(bits, 264, 6) as u16;
        let draught_raw = bits_to_uint(bits, 294, 8);
        let destination = bits_to_string(bits, 302, 20);

        if !callsign.is_empty() {
            fields.insert("callsign".to_string(), callsign);
        }
        if !name.is_empty() {
            fields.insert("name".to_string(), name.clone());
        }
        fields.insert(
            "ship_type".to_string(),
            ship_type_name(ship_type).to_string(),
        );
        fields.insert("ship_type_code".to_string(), ship_type.to_string());

        let length = dim_bow + dim_stern;
        let beam = dim_port + dim_starboard;
        if length > 0 {
            fields.insert("length_m".to_string(), length.to_string());
        }
        if beam > 0 {
            fields.insert("beam_m".to_string(), beam.to_string());
        }

        let draught = draught_raw as f64 / 10.0;
        if draught > 0.0 {
            fields.insert("draught_m".to_string(), format!("{:.1}", draught));
        }
        if !destination.is_empty() {
            fields.insert("destination".to_string(), destination);
        }

        // Update vessel tracking
        self.vessels.insert(
            mmsi,
            VesselInfo {
                name: if name.is_empty() {
                    None
                } else {
                    Some(name.clone())
                },
                last_seen: Instant::now(),
            },
        );

        if name.is_empty() {
            format!("AIS {:09} static/voyage data", mmsi)
        } else {
            format!("AIS {:09} \"{}\" static/voyage data", mmsi, name)
        }
    }

    fn parse_class_b_position(
        &mut self,
        bits: &[u8],
        mmsi: u32,
        fields: &mut BTreeMap<String, String>,
    ) -> String {
        if bits.len() < 168 {
            return format!("AIS type 18 from {:09} (truncated)", mmsi);
        }

        let sog_raw = bits_to_uint(bits, 46, 10);
        let lon_raw = bits_to_int(bits, 57, 28);
        let lat_raw = bits_to_int(bits, 85, 27);
        let cog_raw = bits_to_uint(bits, 112, 12);
        let heading_raw = bits_to_uint(bits, 124, 9);

        let longitude = if lon_raw == 0x6791AC0 {
            None
        } else {
            Some(lon_raw as f64 / 600000.0)
        };
        let latitude = if lat_raw == 0x3412140 {
            None
        } else {
            Some(lat_raw as f64 / 600000.0)
        };

        let sog = if sog_raw == 1023 {
            None
        } else {
            Some(sog_raw as f64 / 10.0)
        };
        let cog = if cog_raw == 3600 {
            None
        } else {
            Some(cog_raw as f64 / 10.0)
        };
        let heading = if heading_raw == 511 {
            None
        } else {
            Some(heading_raw as f64)
        };

        fields.insert("class".to_string(), "B".to_string());
        if let Some(lon) = longitude {
            fields.insert("longitude".to_string(), format!("{:.6}", lon));
        }
        if let Some(lat) = latitude {
            fields.insert("latitude".to_string(), format!("{:.6}", lat));
        }
        if let Some(s) = sog {
            fields.insert("speed_kt".to_string(), format!("{:.1}", s));
        }
        if let Some(c) = cog {
            fields.insert("course_deg".to_string(), format!("{:.1}", c));
        }
        if let Some(h) = heading {
            fields.insert("heading_deg".to_string(), format!("{:.0}", h));
        }

        let name = self.vessels.get(&mmsi).and_then(|v| v.name.clone());
        self.vessels.insert(
            mmsi,
            VesselInfo {
                name: name.clone(),
                last_seen: Instant::now(),
            },
        );

        let name_str = name.map(|n| format!(" ({})", n)).unwrap_or_default();

        match (latitude, longitude) {
            (Some(lat), Some(lon)) => {
                format!("AIS B {:09}{} at {:.4},{:.4}", mmsi, name_str, lat, lon)
            }
            _ => format!("AIS B {:09}{} position", mmsi, name_str),
        }
    }

    fn parse_class_b_static(
        &mut self,
        bits: &[u8],
        mmsi: u32,
        fields: &mut BTreeMap<String, String>,
    ) -> String {
        if bits.len() < 160 {
            return format!("AIS type 24 from {:09} (truncated)", mmsi);
        }

        let part = bits_to_uint(bits, 38, 2) as u8;
        fields.insert(
            "part".to_string(),
            if part == 0 { "A" } else { "B" }.to_string(),
        );

        if part == 0 {
            // Part A: name
            let name = bits_to_string(bits, 40, 20);
            if !name.is_empty() {
                fields.insert("name".to_string(), name.clone());
                self.vessels.insert(
                    mmsi,
                    VesselInfo {
                        name: Some(name.clone()),
                        last_seen: Instant::now(),
                    },
                );
                return format!("AIS B {:09} \"{}\"", mmsi, name);
            }
        } else if bits.len() >= 168 {
            // Part B: ship type, dimensions, callsign
            let ship_type = bits_to_uint(bits, 40, 8) as u8;
            let callsign = bits_to_string(bits, 90, 7);
            fields.insert(
                "ship_type".to_string(),
                ship_type_name(ship_type).to_string(),
            );
            if !callsign.is_empty() {
                fields.insert("callsign".to_string(), callsign);
            }
        }

        format!("AIS B {:09} static data", mmsi)
    }
}

impl DecoderPlugin for AisDecoder {
    fn name(&self) -> &str {
        &self.decoder_name
    }

    fn requirements(&self) -> DecoderRequirements {
        DecoderRequirements {
            center_frequency: self.channel_freq,
            sample_rate: self.sample_rate,
            bandwidth: 25000.0,
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
            let dc_removed = (x - self.dc_state) as f32;

            // 3. Gaussian matched filter
            let filtered = self.matched_filter.process(dc_removed);

            // 4. Clock recovery at 9600 baud
            if let Some(soft_bit) = self.clock.feed(filtered) {
                let level = if soft_bit > 0.0 { 1u8 } else { 0u8 };

                // 5. NRZI decode
                let bit = self.nrzi.decode(level);

                // 6. HDLC deframing
                if let Some(frame) = self.hdlc.feed(bit) {
                    // 7. Process complete frame
                    if let Some(msg) = self.process_frame(&frame) {
                        messages.push(msg);
                    }
                }
            }
        }

        messages
    }

    fn reset(&mut self) {
        self.prev_iq = Sample::new(0.0, 0.0);
        self.dc_state = 0.0;
        self.matched_filter.reset();
        self.clock.reset();
        self.nrzi.reset();
        self.hdlc.reset();
        self.vessels.clear();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::decoders::util;

    // ------------------------------------------------------------------
    // Bit manipulation tests
    // ------------------------------------------------------------------

    #[test]
    fn bits_to_uint_basic() {
        let bits = vec![1, 0, 1, 0]; // = 10
        assert_eq!(bits_to_uint(&bits, 0, 4), 10);
    }

    #[test]
    fn bits_to_uint_offset() {
        let bits = vec![0, 0, 1, 1, 0, 1]; // bits 2..6 = 1101 = 13
        assert_eq!(bits_to_uint(&bits, 2, 4), 13);
    }

    #[test]
    fn bits_to_int_positive() {
        let bits = vec![0, 1, 1, 0]; // +6 in 4-bit signed
        assert_eq!(bits_to_int(&bits, 0, 4), 6);
    }

    #[test]
    fn bits_to_int_negative() {
        let bits = vec![1, 1, 1, 0]; // -2 in 4-bit signed (two's complement)
        assert_eq!(bits_to_int(&bits, 0, 4), -2);
    }

    #[test]
    fn bits_to_string_basic() {
        // 'H' in 6-bit AIS = 8 (value < 32, so char = 8+64 = 72 = 'H')
        // 'I' = 9 → 73 = 'I'
        // 8 = 001000, 9 = 001001
        let bits = vec![0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 1];
        let s = bits_to_string(&bits, 0, 2);
        assert_eq!(s, "HI");
    }

    #[test]
    fn bits_to_string_with_padding() {
        // '@' in AIS 6-bit is 0, which is the padding character
        // String "AB@@@" should trim to "AB"
        // A=1 (000001), B=2 (000010), @=0 (000000)
        let bits = vec![
            0, 0, 0, 0, 0, 1, // A
            0, 0, 0, 0, 1, 0, // B
            0, 0, 0, 0, 0, 0, // @
            0, 0, 0, 0, 0, 0, // @
        ];
        let s = bits_to_string(&bits, 0, 4);
        assert_eq!(s, "AB");
    }

    // ------------------------------------------------------------------
    // Position decoding tests
    // ------------------------------------------------------------------

    #[test]
    fn position_decoding() {
        // Test longitude: 122.0 degrees = 122.0 * 600000 = 73200000
        let lon_raw = 73200000i64;
        let lon = lon_raw as f64 / 600000.0;
        assert!((lon - 122.0).abs() < 0.001, "lon={}", lon);

        // Test latitude: -33.85 degrees = -33.85 * 600000 = -20310000
        let lat_raw = -20310000i64;
        let lat = lat_raw as f64 / 600000.0;
        assert!((lat - (-33.85)).abs() < 0.001, "lat={}", lat);
    }

    // ------------------------------------------------------------------
    // Message type parsing tests
    // ------------------------------------------------------------------

    /// Build a fake AIS bit vector for a position report (type 1).
    fn build_type1_bits(mmsi: u32, lat: f64, lon: f64, sog: f64, cog: f64) -> Vec<u8> {
        let mut bits = vec![0u8; 168];

        // Message type = 1 (6 bits)
        set_bits(&mut bits, 0, 6, 1);
        // Repeat = 0 (2 bits)
        set_bits(&mut bits, 6, 2, 0);
        // MMSI (30 bits)
        set_bits(&mut bits, 8, 30, mmsi as u64);
        // Nav status = 0 (4 bits)
        set_bits(&mut bits, 38, 4, 0);
        // ROT = 0 (8 bits)
        set_bits(&mut bits, 42, 8, 0);
        // SOG (10 bits, 1/10 knot)
        set_bits(&mut bits, 50, 10, (sog * 10.0) as u64);
        // Position accuracy (1 bit)
        set_bits(&mut bits, 60, 1, 1);
        // Longitude (28 bits, signed, 1/10000 minute)
        set_bits_signed(&mut bits, 61, 28, (lon * 600000.0) as i64);
        // Latitude (27 bits, signed, 1/10000 minute)
        set_bits_signed(&mut bits, 89, 27, (lat * 600000.0) as i64);
        // COG (12 bits, 1/10 degree)
        set_bits(&mut bits, 116, 12, (cog * 10.0) as u64);
        // Heading = 511 (not available) (9 bits)
        set_bits(&mut bits, 128, 9, 511);

        bits
    }

    fn set_bits(bits: &mut [u8], start: usize, len: usize, val: u64) {
        for i in 0..len {
            bits[start + i] = ((val >> (len - 1 - i)) & 1) as u8;
        }
    }

    fn set_bits_signed(bits: &mut [u8], start: usize, len: usize, val: i64) {
        let uval = if val < 0 {
            ((1i64 << len) + val) as u64
        } else {
            val as u64
        };
        set_bits(bits, start, len, uval);
    }

    #[test]
    fn parse_position_report_type1() {
        let mmsi = 123456789u32;
        let lat = 37.8085;
        let lon = -122.4711;
        let sog = 12.3;
        let cog = 245.5;

        let bits = build_type1_bits(mmsi, lat, lon, sog, cog);

        let mut decoder = AisDecoder::new(48000.0, AIS_CHANNEL_A);
        let mut fields = BTreeMap::new();
        fields.insert("mmsi".to_string(), format!("{:09}", mmsi));
        fields.insert("msg_type".to_string(), "1".to_string());

        let summary = decoder.parse_position_report(&bits, mmsi, 1, &mut fields);

        assert!(summary.contains("123456789"));
        assert!(fields.contains_key("latitude"));
        assert!(fields.contains_key("longitude"));
        assert!(fields.contains_key("speed_kt"));

        let parsed_lat: f64 = fields["latitude"].parse().unwrap();
        let parsed_lon: f64 = fields["longitude"].parse().unwrap();
        let parsed_sog: f64 = fields["speed_kt"].parse().unwrap();

        assert!((parsed_lat - lat).abs() < 0.001, "lat={}", parsed_lat);
        assert!((parsed_lon - lon).abs() < 0.001, "lon={}", parsed_lon);
        assert!((parsed_sog - sog).abs() < 0.2, "sog={}", parsed_sog);
    }

    // ------------------------------------------------------------------
    // Ship type tests
    // ------------------------------------------------------------------

    #[test]
    fn ship_type_names() {
        assert_eq!(ship_type_name(70), "Cargo");
        assert_eq!(ship_type_name(80), "Tanker");
        assert_eq!(ship_type_name(30), "Fishing");
        assert_eq!(ship_type_name(52), "Tug");
        assert_eq!(ship_type_name(0), "Not available");
    }

    // ------------------------------------------------------------------
    // Gaussian filter tests
    // ------------------------------------------------------------------

    #[test]
    fn gaussian_filter_has_correct_length() {
        let sps = 48000.0 / 9600.0; // 5 samples/symbol
        let coeffs = gaussian_filter_coefficients(sps);
        assert_eq!(coeffs.len(), 21); // 4 symbols × 5 samples + 1 (odd for symmetry)
    }

    #[test]
    fn gaussian_filter_symmetric() {
        let sps = 48000.0 / 9600.0;
        let coeffs = gaussian_filter_coefficients(sps);
        let n = coeffs.len();
        for i in 0..n / 2 {
            assert!(
                (coeffs[i] - coeffs[n - 1 - i]).abs() < 1e-6,
                "Filter should be symmetric: coeffs[{}]={} vs coeffs[{}]={}",
                i,
                coeffs[i],
                n - 1 - i,
                coeffs[n - 1 - i]
            );
        }
    }

    #[test]
    fn gaussian_filter_peak_at_center() {
        let sps = 48000.0 / 9600.0;
        let coeffs = gaussian_filter_coefficients(sps);
        let mid = coeffs.len() / 2;
        for (i, &c) in coeffs.iter().enumerate() {
            assert!(
                c <= coeffs[mid] + 1e-6,
                "Peak should be at center: coeffs[{}]={} > coeffs[{}]={}",
                i,
                c,
                mid,
                coeffs[mid]
            );
        }
    }

    // ------------------------------------------------------------------
    // Frame processing tests
    // ------------------------------------------------------------------

    #[test]
    fn process_frame_with_valid_crc() {
        let mut decoder = AisDecoder::new(48000.0, AIS_CHANNEL_A);

        // Build a minimal frame with type 1 position report bits packed into bytes
        let bits = build_type1_bits(123456789, 37.8, -122.4, 10.0, 180.0);

        // Pack bits into bytes
        let mut payload = Vec::new();
        for chunk in bits.chunks(8) {
            let mut byte = 0u8;
            for (i, &bit) in chunk.iter().enumerate() {
                byte |= bit << (7 - i);
            }
            payload.push(byte);
        }

        // Add CRC
        let crc = util::crc16_ccitt(&payload);
        payload.push((crc & 0xFF) as u8);
        payload.push((crc >> 8) as u8);

        let msg = decoder.process_frame(&payload);
        assert!(msg.is_some(), "Should decode valid frame");
        let msg = msg.unwrap();
        assert_eq!(msg.fields["mmsi"], "123456789");
    }

    #[test]
    fn process_frame_invalid_crc() {
        let mut decoder = AisDecoder::new(48000.0, AIS_CHANNEL_A);
        let payload = vec![0x01, 0x02, 0x03, 0x04, 0x00, 0x00]; // Bad CRC
        assert!(decoder.process_frame(&payload).is_none());
    }

    // ------------------------------------------------------------------
    // Decoder plugin interface tests
    // ------------------------------------------------------------------

    #[test]
    fn decoder_plugin_interface_channel_a() {
        let decoder = AisDecoder::new(48000.0, AIS_CHANNEL_A);
        assert_eq!(decoder.name(), "ais-a");
        assert!(decoder.requirements().wants_iq);
        assert!((decoder.requirements().center_frequency - AIS_CHANNEL_A).abs() < 1.0);
    }

    #[test]
    fn decoder_plugin_interface_channel_b() {
        let decoder = AisDecoder::new(48000.0, AIS_CHANNEL_B);
        assert_eq!(decoder.name(), "ais-b");
        assert!((decoder.requirements().center_frequency - AIS_CHANNEL_B).abs() < 1.0);
    }

    #[test]
    fn decoder_handles_empty_input() {
        let mut decoder = AisDecoder::new(48000.0, AIS_CHANNEL_A);
        let msgs = decoder.process(&[]);
        assert!(msgs.is_empty());
    }

    #[test]
    fn decoder_handles_noise() {
        let mut decoder = AisDecoder::new(48000.0, AIS_CHANNEL_A);
        let noise: Vec<Sample> = (0..10000)
            .map(|i| {
                let phase = (i as f64 * 0.37).sin();
                Sample::new(phase as f32, (phase + 1.0) as f32 * 0.5)
            })
            .collect();
        let msgs = decoder.process(&noise);
        assert!(msgs.is_empty(), "Noise should not produce messages");
    }

    #[test]
    fn decoder_reset_clears_vessels() {
        let mut decoder = AisDecoder::new(48000.0, AIS_CHANNEL_A);
        decoder.vessels.insert(
            123,
            VesselInfo {
                name: Some("Test".to_string()),
                last_seen: Instant::now(),
            },
        );
        decoder.reset();
        assert!(decoder.vessels.is_empty());
    }

    // ------------------------------------------------------------------
    // Nav status tests
    // ------------------------------------------------------------------

    #[test]
    fn nav_status_lookup() {
        assert_eq!(NAV_STATUS[0], "Under way using engine");
        assert_eq!(NAV_STATUS[1], "At anchor");
        assert_eq!(NAV_STATUS[5], "Moored");
        assert_eq!(NAV_STATUS[7], "Engaged in fishing");
    }

    // ------------------------------------------------------------------
    // FIR filter tests
    // ------------------------------------------------------------------

    #[test]
    fn fir_filter_impulse_response() {
        let coeffs = vec![1.0, 2.0, 3.0];
        let mut filter = FirFilter::new(coeffs.clone());

        // Feed impulse
        let y0 = filter.process(1.0); // [1,0,0] dot [1,2,3] = 1
        let y1 = filter.process(0.0); // [0,1,0] dot [1,2,3] = 2
        let y2 = filter.process(0.0); // [0,0,1] dot [1,2,3] = 3

        assert!((y0 - 1.0).abs() < 1e-6);
        assert!((y1 - 2.0).abs() < 1e-6);
        assert!((y2 - 3.0).abs() < 1e-6);
    }
}
