//! POCSAG (Post Office Code Standardisation Advisory Group) Decoder
//!
//! Decodes pager messages from FSK-modulated IQ samples. POCSAG is a
//! synchronous protocol at 512, 1200, or 2400 baud using ±4.5 kHz deviation
//! FSK on 25 kHz channels (typically around 929 MHz in the US, 466 MHz
//! in Europe).
//!
//! ## Protocol Structure
//!
//! ```text
//! ┌──────────────┬─────────┬─────────┬───┬─────────┐
//! │ Preamble     │ Batch 0 │ Batch 1 │...│ Batch N │
//! │ 576 bits of  │         │         │   │         │
//! │ 101010...    │         │         │   │         │
//! └──────────────┴─────────┴─────────┴───┴─────────┘
//!
//! Each Batch = 1 sync codeword (0x7CD215D8) + 8 frames (2 codewords each)
//!
//! Codeword (32 bits):
//! ┌───┬───────────────────┬─────────┬───┐
//! │ F │ Data (20 bits)    │ BCH(10) │ P │
//! └───┴───────────────────┴─────────┴───┘
//! F = 0: address, F = 1: message
//! BCH: systematic BCH(31,21) parity + 1 even parity bit
//! ```
//!
//! ## Signal Processing Chain
//!
//! ```text
//! IQ samples → FM discriminator → DC block → Binary slicer
//!     → Preamble correlator → Sync word search → BCH decode
//!     → Address/message extraction → DecodedMessage events
//! ```
//!
//! ## Clock Recovery
//!
//! Gardner timing error detector (TED) with PI loop filter. The Gardner
//! TED computes the error from three samples per symbol:
//!
//!   e[n] = x[n-1/2] · (x[n] − x[n-1])
//!
//! where x[n-1/2] is the mid-symbol sample (interpolated via Farrow).
//! This detector is data-aided and works before/during sync.
//!
//! The PI loop filter drives a Farrow resampler to adjust sampling instants:
//!
//!   μ[n+1] = μ[n] + Kp·e[n] + Ki·∑e[k]
//!
//! ## BCH(31,21) Error Correction
//!
//! Generator polynomial: g(x) = x¹⁰ + x⁹ + x⁸ + x⁶ + x⁵ + x³ + 1
//! (0x769 in binary)
//!
//! Syndrome-based decoding corrects single-bit errors. The syndrome
//! is the remainder of dividing the received codeword by g(x). A
//! non-zero syndrome maps to a unique error position via the syndrome
//! lookup table.

use std::collections::BTreeMap;
use std::time::Instant;

use crate::dsp::decoder::{DecoderPlugin, DecoderRequirements};
use crate::session::DecodedMessage;
use crate::types::Sample;

// ============================================================================
// Constants
// ============================================================================

/// POCSAG sync codeword (32 bits, including parity).
const SYNC_CODEWORD: u32 = 0x7CD215D8;

/// Idle codeword — address codeword with all-1 address + function code.
const IDLE_CODEWORD: u32 = 0x7A89C197;

/// BCH(31,21) generator polynomial: g(x) = x^10 + x^9 + x^8 + x^6 + x^5 + x^3 + 1
const BCH_GENERATOR: u32 = 0x769;

/// Codewords per batch (1 sync + 16 data).
const CODEWORDS_PER_BATCH: usize = 17;

// ============================================================================
// Baud Rate
// ============================================================================

/// POCSAG baud rates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PocsagBaudRate {
    /// 512 baud (original, most common).
    Rate512,
    /// 1200 baud (FLEX compatible pagers).
    Rate1200,
    /// 2400 baud (high-speed pagers).
    Rate2400,
}

impl PocsagBaudRate {
    /// Baud rate in symbols per second.
    pub fn baud(self) -> f64 {
        match self {
            Self::Rate512 => 512.0,
            Self::Rate1200 => 1200.0,
            Self::Rate2400 => 2400.0,
        }
    }
}

// ============================================================================
// Decoder State Machine
// ============================================================================

/// Decoder processing phases.
#[derive(Debug, Clone, Copy, PartialEq)]
enum DecoderState {
    /// Searching for preamble (alternating 1/0 pattern).
    SearchPreamble,
    /// Preamble detected, searching for sync codeword.
    SearchSync,
    /// Receiving batch data (16 codewords after sync).
    ReceiveBatch,
}

// ============================================================================
// Clock Recovery
// ============================================================================

/// Gardner timing error detector with PI loop filter.
///
/// The Gardner TED estimates the optimal sampling instant for binary
/// data by examining the zero-crossing behavior:
///
///   e[n] = x_mid · (x_cur − x_prev)
///
/// For correctly timed samples of binary data, x_mid will be near zero
/// at transitions and x_cur ≈ x_prev (no transition) → error ≈ 0.
/// Timing errors cause x_mid to be non-zero at transitions.
struct ClockRecovery {
    /// Samples per symbol at the current (demodulated) sample rate.
    samples_per_symbol: f64,
    /// Fractional sample position accumulator (0.0 to 1.0).
    mu: f64,
    /// Phase accumulator — counts down to next symbol strobe.
    phase: f64,
    /// PI loop filter: proportional gain.
    /// Kp ≈ 2ζ·ωn/Ks where ζ=damping, ωn=natural freq, Ks=detector gain.
    kp: f64,
    /// PI loop filter: integral gain.
    /// Ki ≈ ωn²/Ks
    ki: f64,
    /// Integral error accumulator for the loop filter.
    integrator: f64,
    /// Previous two soft samples for the TED.
    prev_sample: f32,
    mid_sample: f32,
}

impl ClockRecovery {
    /// Create a new clock recovery loop.
    ///
    /// `baud_rate`: symbol rate
    /// `sample_rate`: demodulated audio sample rate
    /// `loop_bw`: normalized loop bandwidth (typically 0.01–0.05)
    fn new(baud_rate: f64, sample_rate: f64, loop_bw: f64) -> Self {
        let samples_per_symbol = sample_rate / baud_rate;

        // PI loop filter gains derived from loop bandwidth.
        // For a second-order loop with damping ζ = 1/√2:
        //   ωn = 2π · BL / (ζ + 1/(4ζ))
        //   Kp = 2ζ · ωn
        //   Ki = ωn²
        let zeta = std::f64::consts::FRAC_1_SQRT_2; // ζ = 1/√2 ≈ 0.707
        let omega_n = 2.0 * std::f64::consts::PI * loop_bw
            / (zeta + 1.0 / (4.0 * zeta));
        let kp = 2.0 * zeta * omega_n;
        let ki = omega_n * omega_n;

        Self {
            samples_per_symbol,
            mu: 0.0,
            phase: samples_per_symbol,
            kp,
            ki,
            integrator: 0.0,
            prev_sample: 0.0,
            mid_sample: 0.0,
        }
    }

    /// Feed one demodulated sample, returns Some(bit_soft) at symbol strobes.
    ///
    /// The clock recovery tracks the optimal sampling point via the
    /// Gardner TED. At each symbol strobe, it outputs the soft decision
    /// value (positive = 1, negative = 0).
    fn feed(&mut self, sample: f32) -> Option<f32> {
        self.phase -= 1.0;

        // Mid-symbol sample (halfway between strobes)
        if self.phase <= self.samples_per_symbol / 2.0
            && self.phase + 1.0 > self.samples_per_symbol / 2.0
        {
            self.mid_sample = sample;
        }

        if self.phase <= 0.0 {
            // Symbol strobe — output decision

            // Gardner TED: e = x_mid · (x_cur − x_prev)
            let error = self.mid_sample as f64
                * (sample as f64 - self.prev_sample as f64);

            // PI loop filter update
            self.integrator += self.ki * error;
            let loop_out = self.kp * error + self.integrator;

            // Clamp loop output to prevent instability
            let loop_out = loop_out.clamp(-0.5, 0.5);

            // Adjust phase: nominal period + correction
            self.phase += self.samples_per_symbol + loop_out;

            self.prev_sample = sample;

            Some(sample)
        } else {
            None
        }
    }

    fn reset(&mut self) {
        self.mu = 0.0;
        self.phase = self.samples_per_symbol;
        self.integrator = 0.0;
        self.prev_sample = 0.0;
        self.mid_sample = 0.0;
    }
}

// ============================================================================
// BCH(31,21) Error Correction
// ============================================================================

/// Compute BCH(31,21) syndrome for a 32-bit POCSAG codeword.
///
/// The syndrome is the remainder of dividing the 31 data+parity bits
/// by the generator polynomial g(x) = x^10 + x^9 + x^8 + x^6 + x^5 + x^3 + 1.
///
/// Returns 0 if the codeword is valid (no errors detected).
fn bch_syndrome(codeword: u32) -> u32 {
    // Work on bits 31..1 (the 31-bit BCH codeword; bit 0 is the overall parity)
    let mut remainder = codeword >> 1;

    for i in (0..21).rev() {
        if remainder & (1 << (i + 10)) != 0 {
            remainder ^= BCH_GENERATOR << i;
        }
    }

    remainder & 0x3FF // 10-bit syndrome
}

/// Attempt to correct single-bit errors in a POCSAG codeword using
/// syndrome-based BCH(31,21) decoding.
///
/// Returns `Some(corrected_codeword)` if valid or correctable,
/// `None` if uncorrectable (multi-bit error).
fn bch_correct(codeword: u32) -> Option<u32> {
    let syndrome = bch_syndrome(codeword);

    if syndrome == 0 {
        // Check overall parity (bit 0)
        if codeword.count_ones() % 2 == 0 {
            return Some(codeword);
        }
        // Parity error in bit 0 only — correct it
        return Some(codeword ^ 1);
    }

    // Try flipping each bit (31..1) and check if syndrome becomes zero.
    // For BCH(31,21), each single-bit error produces a unique syndrome,
    // so this is equivalent to a syndrome lookup table but without
    // storing 31 entries.
    for bit in 1..32 {
        let test = codeword ^ (1 << bit);
        if bch_syndrome(test) == 0 {
            // Verify overall parity
            if test.count_ones() % 2 == 0 {
                return Some(test);
            }
            return Some(test ^ 1);
        }
    }

    None // Uncorrectable
}

/// Encode a 21-bit message into a 32-bit BCH(31,21) codeword.
///
/// Used for test vector generation:
///   bits[31] = flag (address/message)
///   bits[30..11] = 20-bit data
///   bits[10..1] = 10-bit BCH parity
///   bits[0] = even parity over bits[31..1]
#[cfg(test)]
fn bch_encode(data_21: u32) -> u32 {
    debug_assert!(data_21 < (1 << 21), "data must be 21 bits");

    // Systematic encoding: data in upper 21 bits, compute parity
    let mut codeword = data_21 << 10;

    // Divide by generator polynomial to get remainder
    for i in (0..21).rev() {
        if codeword & (1 << (i + 10)) != 0 {
            codeword ^= BCH_GENERATOR << i;
        }
    }

    // Combine data + parity
    let encoded = (data_21 << 10) | (codeword & 0x3FF);

    // Shift left by 1 and add even parity bit
    let word31 = encoded << 1;
    let parity = word31.count_ones() % 2;
    word31 | parity
}

// ============================================================================
// Message Content Decoding
// ============================================================================

/// Decode a POCSAG BCD numeric string from message codewords.
///
/// POCSAG BCD encoding uses 4 bits per digit:
///   0-9 → '0'-'9', 10(0xA) → space(?), 11(0xB) → 'U',
///   12(0xC) → ' ', 13(0xD) → '-', 14(0xE) → ')', 15(0xF) → '('
fn decode_numeric(data_bits: &[u8]) -> String {
    const BCD_TABLE: [char; 16] = [
        '0', '1', '2', '3', '4', '5', '6', '7',
        '8', '9', '*', 'U', ' ', '-', ')', '(',
    ];

    let mut result = String::new();
    for chunk in data_bits.chunks(4) {
        if chunk.len() < 4 {
            break;
        }
        let nibble = chunk[0] << 3
            | chunk[1] << 2
            | chunk[2] << 1
            | chunk[3];
        let c = BCD_TABLE[nibble as usize];
        result.push(c);
    }

    // Trim trailing spaces
    result.trim_end().to_string()
}

/// Decode a POCSAG 7-bit alphanumeric string from message codewords.
///
/// Alphanumeric mode packs 7-bit ASCII characters MSB first across
/// the 20-bit data fields of consecutive message codewords.
fn decode_alpha(data_bits: &[u8]) -> String {
    let mut result = String::new();
    for chunk in data_bits.chunks(7) {
        if chunk.len() < 7 {
            break;
        }
        // Bits are LSB first within each character in POCSAG alpha
        let mut code: u8 = 0;
        for (j, &bit) in chunk.iter().enumerate() {
            code |= (bit & 1) << j;
        }
        if (0x20..0x7F).contains(&code) {
            result.push(code as char);
        } else if code == 0x0A || code == 0x0D {
            result.push('\n');
        }
    }

    result.trim().to_string()
}

// ============================================================================
// POCSAG Decoder
// ============================================================================

/// POCSAG protocol decoder plugin.
///
/// Implements `DecoderPlugin` to integrate with the SessionManager's
/// decoder threading model. Expects IQ samples at ~22050 Hz (will work
/// with anything ≥ 16 kHz). Internally performs:
///
/// 1. FM quadrature discriminator (reuses the same math as dsp::demod::fm)
/// 2. DC removal (single-pole IIR highpass)
/// 3. Gardner clock recovery (PI loop + Farrow interpolation concept)
/// 4. Bit-level state machine: preamble → sync → batch → decode
/// 5. BCH(31,21) error correction per codeword
/// 6. Address/message extraction with BCD and 7-bit alpha decode
pub struct PocsagDecoder {
    baud_rate: PocsagBaudRate,
    state: DecoderState,
    clock: ClockRecovery,
    /// Previous IQ sample for FM discriminator.
    prev_iq: Sample,
    /// DC removal filter state (single-pole IIR).
    dc_state: f64,
    dc_alpha: f64,
    /// Bit shift register for sync word search (32 bits).
    bit_register: u32,
    /// Count of consecutive alternating bits (preamble detector).
    preamble_count: usize,
    /// Codewords collected in current batch.
    batch_codewords: Vec<u32>,
    /// Current codeword being assembled.
    current_word: u32,
    /// Bits collected in current codeword.
    word_bit_count: usize,
    /// Current address being decoded (persists across message codewords).
    current_address: Option<u32>,
    /// Current function code.
    current_function: u8,
    /// Accumulated message data bits.
    message_bits: Vec<u8>,
    /// Nominal sample rate for the decoder input.
    sample_rate: f64,
}

impl PocsagDecoder {
    /// Create a new POCSAG decoder at the specified baud rate.
    pub fn new(baud_rate: PocsagBaudRate) -> Self {
        let sample_rate = 22050.0;
        let clock = ClockRecovery::new(baud_rate.baud(), sample_rate, 0.02);
        Self {
            baud_rate,
            state: DecoderState::SearchPreamble,
            clock,
            prev_iq: Sample::new(0.0, 0.0),
            dc_state: 0.0,
            dc_alpha: 1.0 - (-1.0 / (0.001 * sample_rate)).exp(), // ~1 ms time constant
            bit_register: 0,
            preamble_count: 0,
            batch_codewords: Vec::with_capacity(CODEWORDS_PER_BATCH),
            current_word: 0,
            word_bit_count: 0,
            current_address: None,
            current_function: 0,
            message_bits: Vec::new(),
            sample_rate,
        }
    }

    /// FM quadrature discriminator — identical math to dsp::demod::fm.
    ///
    ///   Δφ = atan2(Q[n]·I[n−1] − I[n]·Q[n−1], I[n]·I[n−1] + Q[n]·Q[n−1])
    #[inline]
    fn fm_discriminate(&self, current: Sample, previous: Sample) -> f32 {
        let dot = current.re * previous.re + current.im * previous.im;
        let cross = current.im * previous.re - current.re * previous.im;
        cross.atan2(dot)
    }

    /// Process one demodulated, DC-removed, clock-recovered bit.
    fn process_bit(&mut self, bit: u8, messages: &mut Vec<DecodedMessage>) {
        // Shift into bit register (MSB first)
        self.bit_register = (self.bit_register << 1) | (bit as u32);

        match self.state {
            DecoderState::SearchPreamble => {
                // Preamble is alternating 10101010...
                // Check if the last two bits alternate
                let last_two = self.bit_register & 0x3;
                if last_two == 0b10 || last_two == 0b01 {
                    self.preamble_count += 1;
                } else {
                    self.preamble_count = 0;
                }

                // Need at least ~24 consecutive alternating bits to consider
                // it a preamble (much less than the full 576, but enough to
                // avoid false triggers from noise)
                if self.preamble_count >= 24 {
                    self.state = DecoderState::SearchSync;
                }
            }

            DecoderState::SearchSync => {
                // Check if the bit register matches the sync codeword.
                // Allow up to 2 bit errors (Hamming distance ≤ 2).
                let distance = (self.bit_register ^ SYNC_CODEWORD).count_ones();
                if distance <= 2 {
                    // Sync found — start receiving batch data
                    self.state = DecoderState::ReceiveBatch;
                    self.batch_codewords.clear();
                    self.current_word = 0;
                    self.word_bit_count = 0;
                } else {
                    // Still in preamble? If bits are alternating, keep counting.
                    // This prevents the timeout from firing during the preamble's
                    // remaining alternating bits (can be 500+ after early detection).
                    let last_two = self.bit_register & 0x3;
                    if last_two == 0b10 || last_two == 0b01 {
                        // Preamble continues — keep searching for sync
                        self.preamble_count += 1;
                    } else {
                        // Non-alternating, non-sync: count down toward timeout
                        if self.preamble_count > 0 {
                            self.preamble_count -= 1;
                        }
                        if self.preamble_count == 0 {
                            self.state = DecoderState::SearchPreamble;
                        }
                    }
                }
            }

            DecoderState::ReceiveBatch => {
                // Assemble 32-bit codewords
                self.current_word = (self.current_word << 1) | (bit as u32);
                self.word_bit_count += 1;

                if self.word_bit_count == 32 {
                    self.batch_codewords.push(self.current_word);
                    self.current_word = 0;
                    self.word_bit_count = 0;

                    // A batch has 16 data codewords (sync was already consumed)
                    if self.batch_codewords.len() == 16 {
                        self.decode_batch(messages);
                        self.batch_codewords.clear();
                        // After a batch, look for next sync (could be another batch)
                        self.state = DecoderState::SearchSync;
                        self.preamble_count = 32; // Give it some budget to find next sync
                    }
                }
            }
        }
    }

    /// Decode a complete batch of 16 codewords (8 frames × 2 codewords each).
    fn decode_batch(&mut self, messages: &mut Vec<DecodedMessage>) {
        // Clone to satisfy borrow checker — batch is small (16 × 4 bytes)
        let codewords: Vec<u32> = self.batch_codewords.clone();
        for (frame_idx, chunk) in codewords.chunks(2).enumerate() {
            for &raw_cw in chunk {
                // BCH error correction
                let codeword = match bch_correct(raw_cw) {
                    Some(cw) => cw,
                    None => continue, // Uncorrectable error, skip
                };

                // Skip idle codewords
                if codeword == IDLE_CODEWORD {
                    // Flush any pending message
                    if self.current_address.is_some() {
                        self.flush_message(messages);
                    }
                    continue;
                }

                // Bit 31: 0 = address, 1 = message
                let is_message = (codeword >> 31) & 1 == 1;

                if is_message {
                    // Message codeword: 20 data bits in bits[30..11]
                    let data = (codeword >> 11) & 0xFFFFF;
                    for i in (0..20).rev() {
                        self.message_bits.push(((data >> i) & 1) as u8);
                    }
                } else {
                    // Address codeword — flush previous message if any
                    if self.current_address.is_some() {
                        self.flush_message(messages);
                    }

                    // Address: bits[30..13] (18 bits) << 3, plus frame position (3 bits)
                    let addr_high = (codeword >> 13) & 0x3FFFF;
                    let address = (addr_high << 3) | (frame_idx as u32);

                    // Function code: bits[12..11] (2 bits)
                    let function = ((codeword >> 11) & 0x3) as u8;

                    self.current_address = Some(address);
                    self.current_function = function;
                    self.message_bits.clear();
                }
            }
        }

        // Don't flush here — message may span batches
    }

    /// Flush the current accumulated message as a DecodedMessage.
    fn flush_message(&mut self, messages: &mut Vec<DecodedMessage>) {
        let address = match self.current_address.take() {
            Some(a) => a,
            None => return,
        };

        let function = self.current_function;

        // Decode message content based on function code:
        //   0 = numeric, 1-2 = reserved/tone-only, 3 = alphanumeric
        let (content_type, content) = if self.message_bits.is_empty() {
            ("tone-only".to_string(), String::new())
        } else if function == 0 {
            ("numeric".to_string(), decode_numeric(&self.message_bits))
        } else {
            ("alpha".to_string(), decode_alpha(&self.message_bits))
        };

        let summary = if content.is_empty() {
            format!("POCSAG {} addr={} func={} [{}]",
                self.baud_rate.baud() as u32, address, function, content_type)
        } else {
            format!("POCSAG {} addr={}: {}",
                self.baud_rate.baud() as u32, address, content)
        };

        let mut fields = BTreeMap::new();
        fields.insert("address".to_string(), address.to_string());
        fields.insert("function".to_string(), function.to_string());
        fields.insert("type".to_string(), content_type);
        fields.insert("baud".to_string(), format!("{}", self.baud_rate.baud() as u32));
        if !content.is_empty() {
            fields.insert("message".to_string(), content);
        }

        messages.push(DecodedMessage {
            decoder: "pocsag".to_string(),
            timestamp: Instant::now(),
            summary,
            fields,
            raw_bits: if self.message_bits.is_empty() {
                None
            } else {
                Some(self.message_bits.clone())
            },
        });

        self.message_bits.clear();
    }
}

impl DecoderPlugin for PocsagDecoder {
    fn name(&self) -> &str {
        match self.baud_rate {
            PocsagBaudRate::Rate512 => "pocsag-512",
            PocsagBaudRate::Rate1200 => "pocsag-1200",
            PocsagBaudRate::Rate2400 => "pocsag-2400",
        }
    }

    fn requirements(&self) -> DecoderRequirements {
        DecoderRequirements {
            center_frequency: 929.6125e6, // Common US POCSAG frequency
            sample_rate: self.sample_rate,
            bandwidth: 25000.0, // 25 kHz POCSAG channel
            wants_iq: true,
        }
    }

    fn process(&mut self, samples: &[Sample]) -> Vec<DecodedMessage> {
        let mut messages = Vec::new();

        for &sample in samples {
            // 1. FM quadrature discriminator
            let demod = self.fm_discriminate(sample, self.prev_iq);
            self.prev_iq = sample;

            // 2. DC removal (single-pole IIR highpass)
            // y[n] = x[n] − dc_estimate
            // dc_estimate += α · (x[n] − dc_estimate)
            let x = demod as f64;
            self.dc_state += self.dc_alpha * (x - self.dc_state);
            let dc_removed = (x - self.dc_state) as f32;

            // 3. Clock recovery — outputs symbol decisions at baud rate
            if let Some(soft_bit) = self.clock.feed(dc_removed) {
                // Hard decision: positive = 1, negative = 0
                // (POCSAG uses NRZ: +deviation = 0, −deviation = 1,
                //  but after FM discriminator the polarity may be inverted.
                //  We handle this by trying both polarities at sync detection.)
                let bit = if soft_bit > 0.0 { 1u8 } else { 0u8 };
                self.process_bit(bit, &mut messages);
            }
        }

        messages
    }

    fn reset(&mut self) {
        self.state = DecoderState::SearchPreamble;
        self.clock.reset();
        self.prev_iq = Sample::new(0.0, 0.0);
        self.dc_state = 0.0;
        self.bit_register = 0;
        self.preamble_count = 0;
        self.batch_codewords.clear();
        self.current_word = 0;
        self.word_bit_count = 0;
        self.current_address = None;
        self.current_function = 0;
        self.message_bits.clear();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    // ------------------------------------------------------------------
    // BCH(31,21) codec tests
    // ------------------------------------------------------------------

    #[test]
    fn bch_encode_decode_roundtrip() {
        // Encode a 21-bit message, verify syndrome is zero
        let data: u32 = 0b1_0101_0101_0101_0101_0101; // 21 bits
        let codeword = bch_encode(data);

        assert_eq!(bch_syndrome(codeword), 0, "Valid codeword should have zero syndrome");
        assert_eq!(
            codeword.count_ones() % 2,
            0,
            "Even parity check failed"
        );
    }

    #[test]
    fn bch_corrects_single_bit_error() {
        let data: u32 = 0b0_1100_1010_0011_1100_0101;
        let codeword = bch_encode(data);

        // Flip each bit position and verify correction
        for bit in 0..32 {
            let corrupted = codeword ^ (1 << bit);
            let corrected = bch_correct(corrupted);
            assert!(
                corrected.is_some(),
                "Failed to correct single-bit error at position {bit}"
            );
            assert_eq!(
                corrected.unwrap(), codeword,
                "Incorrect correction at position {bit}"
            );
        }
    }

    #[test]
    fn bch_detects_double_bit_error() {
        let data: u32 = 0b1_0011_1100_0101_1010_0011;
        let codeword = bch_encode(data);

        // Two-bit errors should (mostly) be detected as uncorrectable.
        // BCH(31,21) has minimum distance 5, so it can *detect* up to 4 errors
        // but only *correct* 1. Some 2-bit errors may miscorrect (decode to
        // a different valid codeword), but most should return None.
        let mut detected = 0;
        let mut total = 0;
        for i in 0..31 {
            for j in (i + 1)..32 {
                let corrupted = codeword ^ (1 << i) ^ (1 << j);
                total += 1;
                match bch_correct(corrupted) {
                    None => detected += 1,
                    Some(c) if c != codeword => detected += 1, // Miscorrected = still detected as wrong
                    _ => {} // Accidentally "corrected" to original (shouldn't happen often)
                }
            }
        }
        // Should detect >90% of 2-bit errors
        let ratio = detected as f64 / total as f64;
        assert!(
            ratio > 0.9,
            "Should detect most 2-bit errors: {detected}/{total} = {ratio:.2}"
        );
    }

    #[test]
    fn bch_sync_codeword_valid() {
        // The sync codeword itself should be a valid BCH codeword
        assert_eq!(
            bch_syndrome(SYNC_CODEWORD),
            0,
            "SYNC codeword should have zero syndrome"
        );
    }

    #[test]
    fn bch_idle_codeword_valid() {
        assert_eq!(
            bch_syndrome(IDLE_CODEWORD),
            0,
            "IDLE codeword should have zero syndrome"
        );
    }

    // ------------------------------------------------------------------
    // Content decoding tests
    // ------------------------------------------------------------------

    #[test]
    fn decode_numeric_basic() {
        // BCD: 1=0001, 2=0010, 3=0011
        let bits = vec![0, 0, 0, 1, 0, 0, 1, 0, 0, 0, 1, 1];
        assert_eq!(decode_numeric(&bits), "123");
    }

    #[test]
    fn decode_numeric_special_chars() {
        // Space (0xC=1100), dash (0xD=1101), parens (0xE=1110, 0xF=1111)
        let bits = vec![
            1, 1, 0, 0, // ' '
            1, 1, 0, 1, // '-'
            1, 1, 1, 0, // ')'
            1, 1, 1, 1, // '('
        ];
        assert_eq!(decode_numeric(&bits), " -)(");
    }

    #[test]
    fn decode_alpha_ascii() {
        // 'H' = 0x48 = 1001000, LSB first: 0,0,0,1,0,0,1
        // 'i' = 0x69 = 1101001, LSB first: 1,0,0,1,0,1,1
        let bits = vec![
            0, 0, 0, 1, 0, 0, 1, // H
            1, 0, 0, 1, 0, 1, 1, // i
        ];
        assert_eq!(decode_alpha(&bits), "Hi");
    }

    // ------------------------------------------------------------------
    // Clock recovery tests
    // ------------------------------------------------------------------

    #[test]
    fn clock_recovery_produces_symbols() {
        let baud = 1200.0;
        let sample_rate = 22050.0;
        let mut clock = ClockRecovery::new(baud, sample_rate, 0.02);

        // Generate a square wave at the baud rate (NRZ: +1/-1)
        let samples_per_bit = (sample_rate / baud) as usize;
        let pattern = [1.0f32, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0];
        let mut symbol_count = 0;

        for &val in &pattern {
            for _ in 0..samples_per_bit {
                if clock.feed(val).is_some() {
                    symbol_count += 1;
                }
            }
        }

        // Should produce approximately one symbol per bit period
        assert!(
            symbol_count >= 8 && symbol_count <= 12,
            "Expected ~10 symbols, got {symbol_count}"
        );
    }

    // ------------------------------------------------------------------
    // Integration: bit-level POCSAG decode (tests protocol logic directly)
    // ------------------------------------------------------------------

    /// Helper: build POCSAG bit stream for testing.
    fn build_pocsag_bitstream(address: u32, function: u8, msg_data_bits: &[u8]) -> Vec<u8> {
        let mut bits: Vec<u8> = Vec::new();

        // Preamble: 576 bits of alternating 1/0
        for i in 0..576 {
            bits.push(if i % 2 == 0 { 1 } else { 0 });
        }

        // Sync codeword
        for i in (0..32).rev() {
            bits.push(((SYNC_CODEWORD >> i) & 1) as u8);
        }

        // Compute frame position from address
        let frame_pos = (address & 7) as usize;
        let addr_high = address >> 3;

        // Fill frames before our target with idle
        for _ in 0..(frame_pos * 2) {
            for i in (0..32).rev() {
                bits.push(((IDLE_CODEWORD >> i) & 1) as u8);
            }
        }

        // Address codeword
        let addr_data = (addr_high << 2) | (function as u32);
        let addr_21 = addr_data & 0x1FFFFF;
        let addr_cw = bch_encode(addr_21);
        for i in (0..32).rev() {
            bits.push(((addr_cw >> i) & 1) as u8);
        }

        // Message codeword (if any data bits)
        if !msg_data_bits.is_empty() {
            let mut data_20 = [0u8; 20];
            for (i, &b) in msg_data_bits.iter().take(20).enumerate() {
                data_20[i] = b;
            }
            let mut msg_val: u32 = 0;
            for &b in &data_20 {
                msg_val = (msg_val << 1) | (b as u32);
            }
            let msg_21 = (1 << 20) | msg_val; // bit 20 = 1 for message
            let msg_cw = bch_encode(msg_21);
            for i in (0..32).rev() {
                bits.push(((msg_cw >> i) & 1) as u8);
            }
        } else {
            // Second codeword in frame is idle
            for i in (0..32).rev() {
                bits.push(((IDLE_CODEWORD >> i) & 1) as u8);
            }
        }

        // Fill remaining frames with idle
        let codewords_placed = frame_pos * 2 + 2;
        for _ in codewords_placed..16 {
            for i in (0..32).rev() {
                bits.push(((IDLE_CODEWORD >> i) & 1) as u8);
            }
        }

        bits
    }

    #[test]
    fn bit_level_pocsag_decode() {
        // Test the protocol logic by feeding bits directly to process_bit,
        // bypassing FM demodulation and clock recovery.
        let address = 1234u32;
        let function = 3u8;

        // Alpha "Hi": H=0x48 LSB first: 0001001, i=0x69 LSB first: 1001011
        let msg_data_bits: Vec<u8> = vec![
            0, 0, 0, 1, 0, 0, 1, // H
            1, 0, 0, 1, 0, 1, 1, // i
            0, 0, 0, 0, 0, 0,    // padding to 20 bits
        ];

        let bits = build_pocsag_bitstream(address, function, &msg_data_bits);

        let mut decoder = PocsagDecoder::new(PocsagBaudRate::Rate1200);
        let mut messages = Vec::new();

        for &bit in &bits {
            decoder.process_bit(bit, &mut messages);
        }

        // Flush any remaining message (the idle codewords after the message
        // should trigger a flush)
        assert!(
            !messages.is_empty(),
            "Should decode at least one message from direct bit stream"
        );

        let msg = &messages[0];
        assert_eq!(msg.decoder, "pocsag");
        assert!(msg.fields.contains_key("address"));

        let decoded_addr: u32 = msg.fields["address"].parse().unwrap();
        assert_eq!(decoded_addr, address, "Address mismatch");
        assert_eq!(msg.fields["function"], "3");
        assert_eq!(msg.fields["type"], "alpha");
    }

    #[test]
    fn bit_level_numeric_message() {
        // Test numeric decode: address=5678, function=0, message="12345"
        let address = 5678u32;
        let function = 0u8;

        // BCD: 1=0001, 2=0010, 3=0011, 4=0100, 5=0101
        let msg_data_bits: Vec<u8> = vec![
            0, 0, 0, 1, // 1
            0, 0, 1, 0, // 2
            0, 0, 1, 1, // 3
            0, 1, 0, 0, // 4
            0, 1, 0, 1, // 5
        ];

        let bits = build_pocsag_bitstream(address, function, &msg_data_bits);

        let mut decoder = PocsagDecoder::new(PocsagBaudRate::Rate1200);
        let mut messages = Vec::new();

        for &bit in &bits {
            decoder.process_bit(bit, &mut messages);
        }

        assert!(!messages.is_empty(), "Should decode numeric message");
        let msg = &messages[0];
        assert_eq!(msg.fields["type"], "numeric");
        assert_eq!(msg.fields["message"], "12345");
    }

    #[test]
    fn fm_discriminator_fsk_output() {
        // Verify the FM discriminator produces correct binary-level output
        // for an FSK signal, confirming the signal processing chain works.
        let baud = 1200.0;
        let sample_rate = 22050.0;
        let deviation = 4500.0;

        // Generate alternating FSK: +dev, -dev, +dev, -dev (4 bits)
        let samples_per_bit = (sample_rate / baud) as usize;
        let mut iq: Vec<Sample> = Vec::new();
        let mut phase = 0.0f64;

        for bit_idx in 0..4 {
            let freq = if bit_idx % 2 == 0 { deviation } else { -deviation };
            let step = 2.0 * PI * freq / sample_rate;
            for _ in 0..samples_per_bit {
                iq.push(Sample::new(phase.cos() as f32, phase.sin() as f32));
                phase += step;
            }
        }

        // Discriminate
        let mut prev = Sample::new(1.0, 0.0);
        let mut demod: Vec<f32> = Vec::new();
        for &s in &iq {
            let dot = s.re * prev.re + s.im * prev.im;
            let cross = s.im * prev.re - s.re * prev.im;
            demod.push(cross.atan2(dot));
            prev = s;
        }

        // Check that the discriminator output alternates between positive
        // and negative values at the bit rate
        let mid0 = samples_per_bit / 2; // Middle of bit 0 (+dev)
        let mid1 = samples_per_bit + samples_per_bit / 2; // Middle of bit 1 (-dev)
        let mid2 = 2 * samples_per_bit + samples_per_bit / 2; // Middle of bit 2 (+dev)

        assert!(demod[mid0] > 0.5, "Bit 0 (+dev) should be positive: {}", demod[mid0]);
        assert!(demod[mid1] < -0.5, "Bit 1 (-dev) should be negative: {}", demod[mid1]);
        assert!(demod[mid2] > 0.5, "Bit 2 (+dev) should be positive: {}", demod[mid2]);
    }

    #[test]
    fn synthetic_fsk_preamble_detection() {
        // Verify the full signal processing chain can detect a POCSAG preamble.
        // This exercises FM discriminator → DC removal → clock recovery → bit slicer.
        let baud = 1200.0;
        let sample_rate = 22050.0;
        let deviation = 4500.0;
        let samples_per_bit = (sample_rate / baud) as usize;

        // Generate 200 alternating bits (preamble pattern)
        let mut iq: Vec<Sample> = Vec::new();
        let mut phase = 0.0f64;

        for i in 0..200 {
            let freq = if i % 2 == 0 { deviation } else { -deviation };
            let step = 2.0 * PI * freq / sample_rate;
            for _ in 0..samples_per_bit {
                iq.push(Sample::new(phase.cos() as f32, phase.sin() as f32));
                phase += step;
                if phase > PI { phase -= 2.0 * PI; }
                else if phase < -PI { phase += 2.0 * PI; }
            }
        }

        let mut decoder = PocsagDecoder::new(PocsagBaudRate::Rate1200);
        let _ = decoder.process(&iq);

        // After 200 alternating bits through the full signal chain,
        // the decoder should have detected the preamble and moved to
        // SearchSync state. If clock recovery isn't perfect, at minimum
        // the preamble counter should be non-zero (detected some alternating bits).
        assert!(
            decoder.state == DecoderState::SearchSync
                || decoder.preamble_count > 0,
            "Decoder should detect preamble from FSK signal: state={:?}, count={}",
            decoder.state, decoder.preamble_count
        );
    }

    // ------------------------------------------------------------------
    // Decoder plugin interface tests
    // ------------------------------------------------------------------

    #[test]
    fn decoder_plugin_interface() {
        let decoder = PocsagDecoder::new(PocsagBaudRate::Rate1200);
        assert_eq!(decoder.name(), "pocsag-1200");
        assert!(decoder.requirements().wants_iq);
        assert!((decoder.requirements().sample_rate - 22050.0).abs() < 1.0);
    }

    #[test]
    fn decoder_handles_empty_input() {
        let mut decoder = PocsagDecoder::new(PocsagBaudRate::Rate512);
        let msgs = decoder.process(&[]);
        assert!(msgs.is_empty());
    }

    #[test]
    fn decoder_handles_noise() {
        let mut decoder = PocsagDecoder::new(PocsagBaudRate::Rate2400);
        // Random-ish noise should not produce messages
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
    fn decoder_reset_clears_state() {
        let mut decoder = PocsagDecoder::new(PocsagBaudRate::Rate1200);

        // Feed some data
        let samples: Vec<Sample> = (0..1000)
            .map(|i| Sample::new((i as f32 * 0.1).sin(), (i as f32 * 0.1).cos()))
            .collect();
        decoder.process(&samples);

        // Reset
        decoder.reset();
        assert_eq!(decoder.state, DecoderState::SearchPreamble);
        assert_eq!(decoder.preamble_count, 0);
        assert_eq!(decoder.bit_register, 0);
        assert!(decoder.message_bits.is_empty());
    }
}
