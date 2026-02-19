//! RDS (Radio Data System) / RBDS Decoder
//!
//! Decodes Radio Data System embedded in FM stereo broadcasts on the
//! 57 kHz subcarrier (3× the 19 kHz pilot). RDS uses DPSK (Differential
//! Phase Shift Keying) at 1187.5 baud.
//!
//! ## Signal Structure
//!
//! ```text
//! FM Baseband Spectrum:
//!   0-15 kHz   : L+R mono audio
//!   19 kHz     : Pilot tone
//!   23-53 kHz  : L−R stereo (DSB-SC on 38 kHz)
//!   57 kHz     : RDS subcarrier (3× pilot, DPSK)
//! ```
//!
//! ## Protocol Structure
//!
//! Each RDS group = 4 blocks × 26 bits = 104 bits.
//! Each block = 16 data bits + 10 check bits (CRC-10 + offset word).
//!
//! ```text
//! Block A: PI code (16 bits)         + check + offset A
//! Block B: Group type (4) + B0 (1)   + check + offset B
//!          + TP (1) + PTY (5) + misc (5)
//! Block C: Depends on group type     + check + offset C/C'
//! Block D: Depends on group type     + check + offset D
//! ```
//!
//! ## CRC-10 with Offset Words
//!
//! Generator polynomial: x¹⁰ + x⁸ + x⁷ + x⁵ + x⁴ + x³ + 1 = 0x5B9
//!
//! Each block has a unique offset word XORed with the CRC:
//!   A: 0x0FC, B: 0x198, C: 0x168, C': 0x350, D: 0x1B4
//!
//! Syndrome-based block synchronization: compute syndrome of each 26-bit
//! window and compare against the known offset syndromes.

use std::collections::BTreeMap;
use std::time::Instant;

use crate::dsp::decoder::{DecoderPlugin, DecoderRequirements};
use crate::dsp::decoders::util;
use crate::session::DecodedMessage;
use crate::types::Sample;

// ============================================================================
// Constants
// ============================================================================

/// CRC-10 generator polynomial for RDS.
/// g(x) = x^10 + x^8 + x^7 + x^5 + x^4 + x^3 + 1
const RDS_POLY: u32 = 0x5B9;

/// RDS offset words for block synchronization.
const OFFSET_A:  u32 = 0x0FC;
const OFFSET_B:  u32 = 0x198;
const OFFSET_C:  u32 = 0x168;
const OFFSET_CP: u32 = 0x350; // C' for type B groups
const OFFSET_D:  u32 = 0x1B4;

/// RDS baud rate: 1187.5 symbols/second.
const RDS_BAUD: f64 = 1187.5;

/// PTY (Programme Type) names (North America RBDS).
const PTY_NAMES: [&str; 32] = [
    "None", "News", "Information", "Sports",
    "Talk", "Rock", "Classic Rock", "Adult Hits",
    "Soft Rock", "Top 40", "Country", "Oldies",
    "Soft", "Nostalgia", "Jazz", "Classical",
    "R&B", "Soft R&B", "Language", "Religious Music",
    "Religious Talk", "Personality", "Public", "College",
    "Spanish Talk", "Spanish Music", "Hip Hop", "Unassigned",
    "Unassigned", "Weather", "Emergency Test", "Emergency",
];

// ============================================================================
// CRC-10
// ============================================================================

/// Compute RDS syndrome for a 26-bit block.
///
/// The syndrome is the CRC remainder of the 26-bit block. Each valid block
/// type produces a specific syndrome due to the offset word XOR:
///   syndrome(block) = offset_word (for valid blocks)
fn rds_syndrome(block: u32) -> u32 {
    let mut reg = block;
    for _ in 0..16 {
        if reg & (1 << 25) != 0 {
            reg ^= RDS_POLY << 16;
        }
        reg <<= 1;
    }
    (reg >> 16) & 0x3FF
}

/// Check which block type (A/B/C/C'/D) a 26-bit word matches.
/// Returns the offset type and the 16-bit data if valid.
fn check_block(block: u32) -> Option<(BlockType, u16)> {
    let syndrome = rds_syndrome(block);
    let data = (block >> 10) as u16;

    match syndrome {
        s if s == OFFSET_A  => Some((BlockType::A, data)),
        s if s == OFFSET_B  => Some((BlockType::B, data)),
        s if s == OFFSET_C  => Some((BlockType::C, data)),
        s if s == OFFSET_CP => Some((BlockType::CP, data)),
        s if s == OFFSET_D  => Some((BlockType::D, data)),
        _ => None,
    }
}

/// Encode a 16-bit data word into a 26-bit RDS block with CRC + offset.
#[cfg(test)]
fn rds_encode_block(data: u16, offset: u32) -> u32 {
    let reg = (data as u32) << 10;
    // Compute CRC: divide data by polynomial
    let mut temp = reg;
    for _ in 0..16 {
        if temp & (1 << 25) != 0 {
            temp ^= RDS_POLY << 16;
        }
        temp <<= 1;
    }
    let crc = (temp >> 16) & 0x3FF;
    // Block = data | (CRC XOR offset)
    reg | (crc ^ offset)
}

// ============================================================================
// Block Types
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq)]
enum BlockType {
    A,
    B,
    C,
    CP,
    D,
}

// ============================================================================
// Decoder State
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq)]
enum SyncState {
    /// Searching for block sync by sliding a 26-bit window.
    Searching,
    /// Synchronized — expecting blocks in A-B-C-D order.
    Synced,
}

// ============================================================================
// RDS Decoder
// ============================================================================

/// RDS/RBDS decoder plugin.
///
/// Expects baseband FM-demodulated audio at a sample rate high enough
/// to contain the 57 kHz subcarrier (typically ≥ 120 kHz, ideally 228 kHz
/// or 256 kHz from a WFM demodulator).
///
/// ## Processing Chain
///
/// ```text
/// FM baseband → Bandpass 57 kHz → DPSK demod (costas-like)
///     → Clock recovery → Bit stream → Syndrome check
///     → Block assembly → Group decode → DecodedMessage
/// ```
///
/// ## Implemented Group Types
///
/// - **0A/0B**: Programme Service name (8 chars, 2 per group)
/// - **2A/2B**: Radio Text (64/32 chars)
pub struct RdsDecoder {
    /// Input sample rate.
    sample_rate: f64,
    /// 57 kHz NCO phase (radians).
    nco_phase: f64,
    /// 57 kHz NCO frequency increment (radians/sample).
    nco_freq: f64,
    /// Matched filter / integrate-and-dump state.
    integrator: f64,
    /// Samples per symbol at current rate.
    samples_per_symbol: f64,
    /// Sample counter within current symbol.
    sample_count: f64,
    /// Previous DPSK phase for differential decoding.
    prev_phase: f64,
    /// Bit shift register (26 bits for block sync).
    bit_register: u32,
    /// Bits received count.
    bit_count: usize,
    /// Sync state.
    sync_state: SyncState,
    /// Expected next block type when synced.
    expected_block: usize, // 0=A, 1=B, 2=C, 3=D
    /// Current group's 4 block data values.
    group_data: [u16; 4],
    /// Block validity flags.
    group_valid: [bool; 4],
    /// Consecutive sync errors.
    sync_errors: usize,
    /// Programme Service name (8 chars).
    ps_name: [u8; 8],
    /// Radio Text (64 chars).
    radio_text: [u8; 64],
    /// Radio Text A/B flag (toggles on new text).
    rt_ab_flag: Option<bool>,
    /// PI code.
    pi_code: Option<u16>,
    /// PTY code.
    pty: Option<u8>,
    /// Previous IQ sample for FM discriminator.
    prev_sample: Sample,
    /// Lowpass state for I channel after mixing.
    lpf_i: f64,
    /// Lowpass state for Q channel after mixing.
    lpf_q: f64,
    /// Lowpass alpha (cutoff ≈ baud rate).
    lpf_alpha: f64,
}

impl RdsDecoder {
    pub fn new(sample_rate: f64) -> Self {
        let nco_freq = 2.0 * std::f64::consts::PI * 57000.0 / sample_rate;
        let samples_per_symbol = sample_rate / RDS_BAUD;
        let lpf_alpha = 1.0 - (-2.0 * std::f64::consts::PI * RDS_BAUD / sample_rate).exp();

        Self {
            sample_rate,
            nco_phase: 0.0,
            nco_freq,
            integrator: 0.0,
            samples_per_symbol,
            sample_count: 0.0,
            prev_phase: 0.0,
            bit_register: 0,
            bit_count: 0,
            sync_state: SyncState::Searching,
            expected_block: 0,
            group_data: [0; 4],
            group_valid: [false; 4],
            sync_errors: 0,
            ps_name: [b' '; 8],
            radio_text: [b' '; 64],
            rt_ab_flag: None,
            pi_code: None,
            pty: None,
            prev_sample: Sample::new(0.0, 0.0),
            lpf_i: 0.0,
            lpf_q: 0.0,
            lpf_alpha,
        }
    }

    /// Process one demodulated bit from the DPSK decoder.
    fn process_bit(&mut self, bit: u8, messages: &mut Vec<DecodedMessage>) {
        self.bit_register = ((self.bit_register << 1) | (bit as u32)) & 0x3FFFFFF; // 26 bits
        self.bit_count += 1;

        match self.sync_state {
            SyncState::Searching => {
                // Check if the 26-bit register matches any block syndrome
                if let Some((block_type, _data)) = check_block(self.bit_register) {
                    // Found a valid block — start sync from the next block
                    self.sync_state = SyncState::Synced;
                    self.expected_block = match block_type {
                        BlockType::A => 1,  // Next should be B
                        BlockType::B => 2,  // Next should be C
                        BlockType::C | BlockType::CP => 3, // Next should be D
                        BlockType::D => 0,  // Next should be A
                    };
                    // Record this block's data
                    let idx = match block_type {
                        BlockType::A => 0,
                        BlockType::B => 1,
                        BlockType::C | BlockType::CP => 2,
                        BlockType::D => 3,
                    };
                    self.group_data[idx] = _data;
                    self.group_valid[idx] = true;
                    self.bit_count = 0;
                    self.sync_errors = 0;
                }
            }

            SyncState::Synced => {
                if self.bit_count < 26 {
                    return; // Not enough bits for a block yet
                }

                self.bit_count = 0;

                // Check the expected block type
                let expected_offsets = match self.expected_block {
                    0 => &[OFFSET_A][..],
                    1 => &[OFFSET_B][..],
                    2 => &[OFFSET_C, OFFSET_CP][..],
                    3 => &[OFFSET_D][..],
                    _ => unreachable!(),
                };

                let syndrome = rds_syndrome(self.bit_register);
                let data = (self.bit_register >> 10) as u16;
                let matched = expected_offsets.contains(&syndrome);

                if matched {
                    self.group_data[self.expected_block] = data;
                    self.group_valid[self.expected_block] = true;
                    self.sync_errors = 0;
                } else {
                    self.group_valid[self.expected_block] = false;
                    self.sync_errors += 1;

                    if self.sync_errors > 5 {
                        // Lost sync
                        self.sync_state = SyncState::Searching;
                        self.group_valid = [false; 4];
                        return;
                    }
                }

                // Advance to next block
                self.expected_block = (self.expected_block + 1) % 4;

                // If we completed a group (just finished block D), decode it
                if self.expected_block == 0 {
                    if self.group_valid[0] && self.group_valid[1] {
                        self.decode_group(messages);
                    }
                    self.group_valid = [false; 4];
                }
            }
        }
    }

    /// Decode a complete RDS group (blocks A-D).
    fn decode_group(&mut self, messages: &mut Vec<DecodedMessage>) {
        let block_a = self.group_data[0];
        let block_b = self.group_data[1];
        let block_c = self.group_data[2];
        let block_d = self.group_data[3];

        // Block A: PI code (Programme Identification)
        let pi = block_a;
        self.pi_code = Some(pi);

        // Block B: group type, version, TP, PTY
        let group_type = (block_b >> 12) & 0xF;
        let version_b = (block_b >> 11) & 1; // 0=A, 1=B
        let _tp = (block_b >> 10) & 1;
        let pty = ((block_b >> 5) & 0x1F) as u8;
        self.pty = Some(pty);

        let group_label = format!("{}{}",
            group_type,
            if version_b == 0 { 'A' } else { 'B' });

        match (group_type, version_b) {
            // Type 0A/0B: Programme Service name
            (0, _) => {
                let segment = (block_b & 0x3) as usize;
                if self.group_valid[3] {
                    // Block D contains 2 PS characters
                    let c1 = ((block_d >> 8) & 0xFF) as u8;
                    let c2 = (block_d & 0xFF) as u8;
                    let pos = segment * 2;
                    if pos + 1 < 8 {
                        self.ps_name[pos] = if (0x20..0x7F).contains(&c1) { c1 } else { b' ' };
                        self.ps_name[pos + 1] = if (0x20..0x7F).contains(&c2) { c2 } else { b' ' };
                    }
                }

                let ps = String::from_utf8_lossy(&self.ps_name).trim().to_string();
                if !ps.is_empty() {
                    let mut fields = BTreeMap::new();
                    fields.insert("pi".to_string(), format!("{:04X}", pi));
                    fields.insert("group".to_string(), group_label);
                    fields.insert("pty".to_string(), PTY_NAMES[pty as usize].to_string());
                    fields.insert("ps_name".to_string(), ps.clone());

                    messages.push(DecodedMessage {
                        decoder: "rds".to_string(),
                        timestamp: Instant::now(),
                        summary: format!("RDS PI:{:04X} PS:\"{}\" PTY:{}", pi, ps,
                            PTY_NAMES[pty as usize]),
                        fields,
                        raw_bits: None,
                    });
                }
            }

            // Type 2A: Radio Text (64 chars, 4 chars per group)
            (2, 0) => {
                let ab = (block_b >> 4) & 1;
                let segment = (block_b & 0xF) as usize;

                // A/B flag toggle means new text
                let ab_bool = ab == 1;
                if self.rt_ab_flag.is_some() && self.rt_ab_flag != Some(ab_bool) {
                    self.radio_text = [b' '; 64];
                }
                self.rt_ab_flag = Some(ab_bool);

                // Block C has 2 chars, Block D has 2 chars
                let pos = segment * 4;
                if self.group_valid[2] && pos + 1 < 64 {
                    let c1 = ((block_c >> 8) & 0xFF) as u8;
                    let c2 = (block_c & 0xFF) as u8;
                    self.radio_text[pos] = if (0x20..0x7F).contains(&c1) { c1 } else { b' ' };
                    self.radio_text[pos + 1] = if (0x20..0x7F).contains(&c2) { c2 } else { b' ' };
                }
                if self.group_valid[3] && pos + 3 < 64 {
                    let c3 = ((block_d >> 8) & 0xFF) as u8;
                    let c4 = (block_d & 0xFF) as u8;
                    self.radio_text[pos + 2] = if (0x20..0x7F).contains(&c3) { c3 } else { b' ' };
                    self.radio_text[pos + 3] = if (0x20..0x7F).contains(&c4) { c4 } else { b' ' };
                }

                let rt = String::from_utf8_lossy(&self.radio_text).trim().to_string();
                if !rt.is_empty() {
                    let mut fields = BTreeMap::new();
                    fields.insert("pi".to_string(), format!("{:04X}", pi));
                    fields.insert("group".to_string(), group_label);
                    fields.insert("radio_text".to_string(), rt.clone());

                    messages.push(DecodedMessage {
                        decoder: "rds".to_string(),
                        timestamp: Instant::now(),
                        summary: format!("RDS PI:{:04X} RT:\"{}\"", pi, rt),
                        fields,
                        raw_bits: None,
                    });
                }
            }

            // Type 2B: Radio Text (32 chars, 2 chars per group from block D only)
            (2, 1) => {
                let segment = (block_b & 0xF) as usize;
                let pos = segment * 2;

                if self.group_valid[3] && pos + 1 < 64 {
                    let c1 = ((block_d >> 8) & 0xFF) as u8;
                    let c2 = (block_d & 0xFF) as u8;
                    self.radio_text[pos] = if (0x20..0x7F).contains(&c1) { c1 } else { b' ' };
                    self.radio_text[pos + 1] = if (0x20..0x7F).contains(&c2) { c2 } else { b' ' };
                }
            }

            _ => {
                // Other group types not decoded
            }
        }
    }
}

impl DecoderPlugin for RdsDecoder {
    fn name(&self) -> &str {
        "rds"
    }

    fn requirements(&self) -> DecoderRequirements {
        DecoderRequirements {
            center_frequency: 0.0, // RDS is embedded in FM — no specific center freq
            sample_rate: self.sample_rate,
            bandwidth: 4000.0, // ±2 kHz around 57 kHz subcarrier
            wants_iq: true, // Receives raw IQ, applies own FM discriminator
        }
    }

    fn process(&mut self, samples: &[Sample]) -> Vec<DecodedMessage> {
        let mut messages = Vec::new();

        for &sample in samples {
            // Apply FM discriminator to extract baseband from raw IQ.
            // The FM-demodulated signal contains the 57 kHz RDS subcarrier.
            let x = util::fm_discriminate(sample, self.prev_sample) as f64;
            self.prev_sample = sample;

            // Mix with 57 kHz NCO to bring RDS subcarrier to baseband
            let nco_i = self.nco_phase.cos();
            let nco_q = self.nco_phase.sin();
            self.nco_phase += self.nco_freq;
            if self.nco_phase > std::f64::consts::PI {
                self.nco_phase -= 2.0 * std::f64::consts::PI;
            }

            // Baseband I/Q after mixing
            let bb_i = x * nco_i;
            let bb_q = x * nco_q;

            // Lowpass filter (single-pole IIR)
            self.lpf_i += self.lpf_alpha * (bb_i - self.lpf_i);
            self.lpf_q += self.lpf_alpha * (bb_q - self.lpf_q);

            // Symbol timing: integrate and dump
            self.sample_count += 1.0;
            self.integrator += self.lpf_i;

            if self.sample_count >= self.samples_per_symbol {
                self.sample_count -= self.samples_per_symbol;

                // DPSK demodulation: compare phase to previous symbol
                let phase = self.lpf_q.atan2(self.lpf_i);
                let delta = phase - self.prev_phase;
                self.prev_phase = phase;

                // Normalize phase difference to [-π, π]
                let delta = if delta > std::f64::consts::PI {
                    delta - 2.0 * std::f64::consts::PI
                } else if delta < -std::f64::consts::PI {
                    delta + 2.0 * std::f64::consts::PI
                } else {
                    delta
                };

                // DPSK decision: phase change near 0 → bit 0, near π → bit 1
                let bit = if delta.abs() > std::f64::consts::FRAC_PI_2 { 1u8 } else { 0u8 };

                self.process_bit(bit, &mut messages);

                self.integrator = 0.0;
            }
        }

        messages
    }

    fn reset(&mut self) {
        self.nco_phase = 0.0;
        self.integrator = 0.0;
        self.sample_count = 0.0;
        self.prev_phase = 0.0;
        self.bit_register = 0;
        self.bit_count = 0;
        self.sync_state = SyncState::Searching;
        self.expected_block = 0;
        self.group_data = [0; 4];
        self.group_valid = [false; 4];
        self.sync_errors = 0;
        self.ps_name = [b' '; 8];
        self.radio_text = [b' '; 64];
        self.rt_ab_flag = None;
        self.pi_code = None;
        self.pty = None;
        self.prev_sample = Sample::new(0.0, 0.0);
        self.lpf_i = 0.0;
        self.lpf_q = 0.0;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // CRC-10 / Syndrome tests
    // ------------------------------------------------------------------

    #[test]
    fn rds_encode_decode_roundtrip() {
        // Encode a block with each offset type and verify syndrome matches
        let data: u16 = 0x1234;

        let block_a = rds_encode_block(data, OFFSET_A);
        assert_eq!(rds_syndrome(block_a), OFFSET_A, "Block A syndrome mismatch");

        let block_b = rds_encode_block(data, OFFSET_B);
        assert_eq!(rds_syndrome(block_b), OFFSET_B, "Block B syndrome mismatch");

        let block_c = rds_encode_block(data, OFFSET_C);
        assert_eq!(rds_syndrome(block_c), OFFSET_C, "Block C syndrome mismatch");

        let block_d = rds_encode_block(data, OFFSET_D);
        assert_eq!(rds_syndrome(block_d), OFFSET_D, "Block D syndrome mismatch");
    }

    #[test]
    fn check_block_identifies_type() {
        let data: u16 = 0xABCD;

        let block_a = rds_encode_block(data, OFFSET_A);
        let result = check_block(block_a);
        assert!(result.is_some());
        let (bt, d) = result.unwrap();
        assert_eq!(bt, BlockType::A);
        assert_eq!(d, data);

        let block_d = rds_encode_block(data, OFFSET_D);
        let result = check_block(block_d);
        assert!(result.is_some());
        assert_eq!(result.unwrap().0, BlockType::D);
    }

    #[test]
    fn syndrome_detects_corruption() {
        let data: u16 = 0x5678;
        let block = rds_encode_block(data, OFFSET_A);

        // Corrupt one bit
        let corrupted = block ^ (1 << 15);
        let result = check_block(corrupted);
        // Should not match any valid offset
        assert!(result.is_none() || result.unwrap().0 != BlockType::A);
    }

    // ------------------------------------------------------------------
    // Group decode tests (bit-level)
    // ------------------------------------------------------------------

    #[test]
    fn bit_level_ps_name_decode() {
        // Construct RDS Type 0A groups that spell out "TEST FM "
        // PS name is built 2 chars at a time over 4 groups (segments 0-3)

        let pi: u16 = 0x1234;
        let pty: u8 = 10; // Country

        // Build 4 groups, each with PS segment
        let ps_chars = [b'T', b'E', b'S', b'T', b' ', b'F', b'M', b' '];

        let mut decoder = RdsDecoder::new(2_048_000.0);
        let mut messages = Vec::new();

        for segment in 0..4u16 {
            // Block A: PI code
            let block_a = rds_encode_block(pi, OFFSET_A);

            // Block B: group_type=0, version_A=0, TP=0, PTY, segment
            let block_b_data = ((pty as u16) << 5) // PTY
                | (segment & 0x3);                 // PS segment
            let block_b = rds_encode_block(block_b_data, OFFSET_B);

            // Block C: not used for PS in 0A, fill with zeros
            let block_c = rds_encode_block(0, OFFSET_C);

            // Block D: 2 PS characters
            let c1 = ps_chars[(segment as usize) * 2] as u16;
            let c2 = ps_chars[(segment as usize) * 2 + 1] as u16;
            let block_d_data = (c1 << 8) | c2;
            let block_d = rds_encode_block(block_d_data, OFFSET_D);

            // Feed bits to decoder (MSB first, 26 bits per block)
            for block in [block_a, block_b, block_c, block_d] {
                for i in (0..26).rev() {
                    let bit = ((block >> i) & 1) as u8;
                    decoder.process_bit(bit, &mut messages);
                }
            }
        }

        // Should have decoded PS name messages
        assert!(!messages.is_empty(), "Should decode PS name from RDS groups");

        // The last message should have the full PS name
        let last = messages.last().unwrap();
        assert_eq!(last.decoder, "rds");
        assert!(last.fields.contains_key("ps_name"));
        // PS name builds up over groups, final should contain "TEST FM"
        let ps = &last.fields["ps_name"];
        assert!(ps.contains("TEST"), "PS should contain 'TEST': got '{}'", ps);
    }

    #[test]
    fn bit_level_radio_text_decode() {
        // Construct RDS Type 2A group with radio text "Hello World"
        let pi: u16 = 0x5678;
        let pty: u8 = 5; // Rock

        let rt_text = b"Hello World                                                     ";

        let mut decoder = RdsDecoder::new(2_048_000.0);
        let mut messages = Vec::new();

        // First send a type 0A group to establish sync
        let block_a = rds_encode_block(pi, OFFSET_A);
        let block_b = rds_encode_block(0, OFFSET_B);
        let block_c = rds_encode_block(0, OFFSET_C);
        let block_d = rds_encode_block(0, OFFSET_D);
        for block in [block_a, block_b, block_c, block_d] {
            for i in (0..26).rev() {
                decoder.process_bit(((block >> i) & 1) as u8, &mut messages);
            }
        }

        // Now send type 2A groups
        for segment in 0..4u16 {
            let block_a = rds_encode_block(pi, OFFSET_A);

            // Block B: group_type=2, version_A=0, TP=0, PTY, AB_flag=0, segment
            let block_b_data = (2u16 << 12)
                | ((pty as u16) << 5)
                | (segment & 0xF);
            let block_b = rds_encode_block(block_b_data, OFFSET_B);

            let pos = (segment as usize) * 4;
            let c1 = rt_text[pos] as u16;
            let c2 = rt_text[pos + 1] as u16;
            let block_c = rds_encode_block((c1 << 8) | c2, OFFSET_C);

            let c3 = rt_text[pos + 2] as u16;
            let c4 = rt_text[pos + 3] as u16;
            let block_d = rds_encode_block((c3 << 8) | c4, OFFSET_D);

            for block in [block_a, block_b, block_c, block_d] {
                for i in (0..26).rev() {
                    decoder.process_bit(((block >> i) & 1) as u8, &mut messages);
                }
            }
        }

        // Should have some radio text messages
        let rt_msgs: Vec<_> = messages.iter()
            .filter(|m| m.fields.contains_key("radio_text"))
            .collect();
        assert!(!rt_msgs.is_empty(), "Should decode radio text");

        let last_rt = rt_msgs.last().unwrap();
        let rt = &last_rt.fields["radio_text"];
        assert!(rt.contains("Hello"), "RT should contain 'Hello': got '{}'", rt);
    }

    // ------------------------------------------------------------------
    // PTY names
    // ------------------------------------------------------------------

    #[test]
    fn pty_names_coverage() {
        assert_eq!(PTY_NAMES[0], "None");
        assert_eq!(PTY_NAMES[1], "News");
        assert_eq!(PTY_NAMES[10], "Country");
        assert_eq!(PTY_NAMES[31], "Emergency");
        assert_eq!(PTY_NAMES.len(), 32);
    }

    // ------------------------------------------------------------------
    // Decoder plugin interface
    // ------------------------------------------------------------------

    #[test]
    fn decoder_plugin_interface() {
        let decoder = RdsDecoder::new(2_048_000.0);
        assert_eq!(decoder.name(), "rds");
        assert!(decoder.requirements().wants_iq); // Receives raw IQ, applies FM discriminator
        assert!((decoder.requirements().sample_rate - 2_048_000.0).abs() < 1.0);
    }

    #[test]
    fn decoder_handles_empty_input() {
        let mut decoder = RdsDecoder::new(2_048_000.0);
        let msgs = decoder.process(&[]);
        assert!(msgs.is_empty());
    }

    #[test]
    fn decoder_reset_clears_state() {
        let mut decoder = RdsDecoder::new(2_048_000.0);
        decoder.ps_name = *b"TEST FM ";
        decoder.pi_code = Some(0x1234);
        decoder.reset();
        assert_eq!(decoder.pi_code, None);
        assert_eq!(&decoder.ps_name, b"        ");
        assert_eq!(decoder.sync_state, SyncState::Searching);
    }
}
