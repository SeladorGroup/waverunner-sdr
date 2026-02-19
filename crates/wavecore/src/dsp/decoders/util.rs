//! Shared decoder utilities.
//!
//! Reusable building blocks for protocol decoders that share common
//! framing and encoding schemes.
//!
//! ## Components
//!
//! - [`NrziDecoder`]: Non-Return-to-Zero Inverted bit decoding
//! - [`HdlcDeframer`]: HDLC frame extraction with bit destuffing
//! - [`crc16_ccitt`]: CRC-16-CCITT checksum (used by AX.25 and AIS)
//! - [`ClockRecovery`]: Gardner TED with PI loop filter

use std::f64::consts::PI;

// ============================================================================
// NRZI Decoder
// ============================================================================

/// Non-Return-to-Zero Inverted (NRZI) decoder.
///
/// In NRZI encoding:
/// - A **transition** (level change) represents a **0** bit
/// - **No transition** (same level) represents a **1** bit
///
/// Used by AX.25 (APRS) and AIS protocols.
pub struct NrziDecoder {
    last_level: u8,
}

impl Default for NrziDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl NrziDecoder {
    pub fn new() -> Self {
        Self { last_level: 0 }
    }

    /// Decode one NRZI-encoded level to a data bit.
    ///
    /// Returns 1 if level matches previous (no transition),
    /// 0 if level differs (transition).
    #[inline]
    pub fn decode(&mut self, level: u8) -> u8 {
        let bit = if level == self.last_level { 1 } else { 0 };
        self.last_level = level;
        bit
    }

    pub fn reset(&mut self) {
        self.last_level = 0;
    }
}

// ============================================================================
// HDLC Deframer
// ============================================================================

/// HDLC frame extraction with bit destuffing.
///
/// Searches for 0x7E flag sequences, removes bit stuffing (zero inserted
/// after five consecutive ones), and extracts complete frames.
///
/// ## Bit Stuffing
///
/// The transmitter inserts a 0 bit after every five consecutive 1 bits
/// in the data. The receiver removes these inserted zeros. This prevents
/// the data from mimicking the 0x7E flag pattern (01111110).
pub struct HdlcDeframer {
    /// Accumulates bits for the current frame.
    bit_buffer: Vec<u8>,
    /// Whether we're inside a frame (between opening and closing flags).
    in_frame: bool,
    /// Count of consecutive 1 bits (for destuffing and flag detection).
    ones_count: usize,
    /// Shift register for flag detection (8 bits).
    shift_reg: u8,
    /// Number of bits shifted in.
    bits_shifted: usize,
}

impl Default for HdlcDeframer {
    fn default() -> Self {
        Self::new()
    }
}

impl HdlcDeframer {
    pub fn new() -> Self {
        Self {
            bit_buffer: Vec::with_capacity(512),
            in_frame: false,
            ones_count: 0,
            shift_reg: 0,
            bits_shifted: 0,
        }
    }

    /// Feed one data bit (after NRZI decoding) into the deframer.
    ///
    /// Returns `Some(frame_bytes)` when a complete frame is extracted,
    /// or `None` if still accumulating. The returned bytes do NOT include
    /// the flag bytes or CRC — the caller should validate the CRC.
    pub fn feed(&mut self, bit: u8) -> Option<Vec<u8>> {
        // Track the bit in the shift register for flag detection
        self.shift_reg = (self.shift_reg << 1) | (bit & 1);
        self.bits_shifted += 1;

        // Check for flag pattern: 01111110 (0x7E)
        if self.bits_shifted >= 8 && self.shift_reg == 0x7E {
            let result = if self.in_frame {
                // End of frame — pack bits into bytes.
                // Remove the last 7 bits that are part of the flag
                // (they were added before we detected the flag pattern).
                let valid_bits = self.bit_buffer.len().saturating_sub(7);
                if valid_bits >= 16 {
                    // At least 2 bytes (minimum for any useful frame)
                    let frame = self.bits_to_bytes(&self.bit_buffer[..valid_bits]);
                    Some(frame)
                } else {
                    None
                }
            } else {
                None
            };

            // Start new frame after flag
            self.bit_buffer.clear();
            self.in_frame = true;
            self.ones_count = 0;
            return result;
        }

        if !self.in_frame {
            return None;
        }

        // Check for abort (7+ consecutive ones)
        if bit == 1 {
            self.ones_count += 1;
            if self.ones_count >= 7 {
                // Abort — reset frame
                self.in_frame = false;
                self.bit_buffer.clear();
                self.ones_count = 0;
                return None;
            }
            self.bit_buffer.push(1);
        } else {
            // bit == 0
            if self.ones_count == 5 {
                // Stuffed bit — discard this zero (destuffing)
                self.ones_count = 0;
                // Don't push this bit
            } else {
                self.ones_count = 0;
                self.bit_buffer.push(0);
            }
        }

        None
    }

    /// Convert a bit vector to packed bytes (MSB first within each byte).
    fn bits_to_bytes(&self, bits: &[u8]) -> Vec<u8> {
        let num_bytes = bits.len() / 8;
        let mut bytes = Vec::with_capacity(num_bytes);
        for chunk in bits.chunks_exact(8) {
            let mut byte = 0u8;
            // HDLC transmits LSB first, so bit[0] is the LSB
            for (i, &bit) in chunk.iter().enumerate() {
                byte |= (bit & 1) << i;
            }
            bytes.push(byte);
        }
        bytes
    }

    pub fn reset(&mut self) {
        self.bit_buffer.clear();
        self.in_frame = false;
        self.ones_count = 0;
        self.shift_reg = 0;
        self.bits_shifted = 0;
    }
}

// ============================================================================
// CRC-16-CCITT
// ============================================================================

/// CRC-16-CCITT lookup table (polynomial 0x8408, reflected/LSB-first).
///
/// This is the standard CRC used by AX.25 (APRS) and AIS.
/// Initial value: 0xFFFF, final XOR: 0xFFFF.
const CRC16_TABLE: [u16; 256] = {
    let mut table = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u16;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0x8408;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// Compute CRC-16-CCITT over a byte slice.
///
/// Uses the reflected polynomial 0x8408 with initial value 0xFFFF
/// and final XOR 0xFFFF. This is the standard checksum for AX.25 and AIS.
///
/// For frame validation, compute the CRC over the data bytes (excluding
/// the 2-byte CRC field). The result should match the received CRC
/// (transmitted LSB first).
pub fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        let idx = ((crc ^ byte as u16) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC16_TABLE[idx];
    }
    crc ^ 0xFFFF
}

/// Validate a frame with appended CRC-16-CCITT.
///
/// The frame should include the 2-byte CRC at the end. Returns true
/// if the CRC check passes.
pub fn crc16_check(frame_with_crc: &[u8]) -> bool {
    if frame_with_crc.len() < 3 {
        return false;
    }
    // Run CRC over entire frame including the CRC bytes.
    // For a valid frame, the residue should be 0xF0B8.
    let mut crc: u16 = 0xFFFF;
    for &byte in frame_with_crc {
        let idx = ((crc ^ byte as u16) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC16_TABLE[idx];
    }
    crc == 0xF0B8
}

// ============================================================================
// Clock Recovery (Gardner TED)
// ============================================================================

/// Gardner timing error detector with PI loop filter.
///
/// Generalized from the POCSAG decoder's clock recovery. Tracks the
/// optimal sampling instant for binary data using the Gardner TED:
///
///   e[n] = x_mid · (x_cur − x_prev)
///
/// The PI loop filter adjusts the sample phase to minimize timing error.
pub struct ClockRecovery {
    /// Samples per symbol at the demodulated sample rate.
    samples_per_symbol: f64,
    /// Phase accumulator — counts down to next symbol strobe.
    phase: f64,
    /// PI loop filter: proportional gain.
    kp: f64,
    /// PI loop filter: integral gain.
    ki: f64,
    /// Integral error accumulator.
    integrator: f64,
    /// Previous symbol sample.
    prev_sample: f32,
    /// Mid-symbol sample (for Gardner TED).
    mid_sample: f32,
}

impl ClockRecovery {
    /// Create a new clock recovery loop.
    ///
    /// - `baud_rate`: symbol rate in baud
    /// - `sample_rate`: input sample rate in Hz
    /// - `loop_bw`: normalized loop bandwidth (typically 0.01–0.05)
    pub fn new(baud_rate: f64, sample_rate: f64, loop_bw: f64) -> Self {
        let samples_per_symbol = sample_rate / baud_rate;

        // PI loop filter gains from loop bandwidth.
        // Second-order loop with damping ζ = 1/√2:
        let zeta = std::f64::consts::FRAC_1_SQRT_2;
        let omega_n = 2.0 * PI * loop_bw / (zeta + 1.0 / (4.0 * zeta));
        let kp = 2.0 * zeta * omega_n;
        let ki = omega_n * omega_n;

        Self {
            samples_per_symbol,
            phase: samples_per_symbol,
            kp,
            ki,
            integrator: 0.0,
            prev_sample: 0.0,
            mid_sample: 0.0,
        }
    }

    /// Feed one demodulated sample. Returns `Some(soft_value)` at symbol strobes.
    #[inline]
    pub fn feed(&mut self, sample: f32) -> Option<f32> {
        self.phase -= 1.0;

        // Capture mid-symbol sample
        if self.phase <= self.samples_per_symbol / 2.0
            && self.phase + 1.0 > self.samples_per_symbol / 2.0
        {
            self.mid_sample = sample;
        }

        if self.phase <= 0.0 {
            // Symbol strobe — compute Gardner TED error
            let error =
                self.mid_sample as f64 * (sample as f64 - self.prev_sample as f64);

            // PI loop filter
            self.integrator += self.ki * error;
            let loop_out = (self.kp * error + self.integrator).clamp(-0.5, 0.5);

            // Adjust phase
            self.phase += self.samples_per_symbol + loop_out;
            self.prev_sample = sample;

            Some(sample)
        } else {
            None
        }
    }

    pub fn reset(&mut self) {
        self.phase = self.samples_per_symbol;
        self.integrator = 0.0;
        self.prev_sample = 0.0;
        self.mid_sample = 0.0;
    }
}

// ============================================================================
// FM Discriminator (shared)
// ============================================================================

/// FM quadrature discriminator.
///
/// Computes instantaneous frequency from consecutive IQ samples:
///   Δφ = atan2(Q[n]·I[n−1] − I[n]·Q[n−1], I[n]·I[n−1] + Q[n]·Q[n−1])
#[inline]
pub fn fm_discriminate(current: num_complex::Complex<f32>, previous: num_complex::Complex<f32>) -> f32 {
    let dot = current.re * previous.re + current.im * previous.im;
    let cross = current.im * previous.re - current.re * previous.im;
    cross.atan2(dot)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // NRZI tests
    // ------------------------------------------------------------------

    #[test]
    fn nrzi_no_transition_is_one() {
        let mut nrzi = NrziDecoder::new();
        // Starting level is 0, feed 0 → no transition → bit 1
        assert_eq!(nrzi.decode(0), 1);
        // Feed 0 again → no transition → bit 1
        assert_eq!(nrzi.decode(0), 1);
    }

    #[test]
    fn nrzi_transition_is_zero() {
        let mut nrzi = NrziDecoder::new();
        // Start at 0, feed 1 → transition → bit 0
        assert_eq!(nrzi.decode(1), 0);
        // Feed 0 → transition → bit 0
        assert_eq!(nrzi.decode(0), 0);
    }

    #[test]
    fn nrzi_alternating_pattern() {
        let mut nrzi = NrziDecoder::new();
        // All transitions → all zeros
        let levels = [1, 0, 1, 0, 1, 0];
        let bits: Vec<u8> = levels.iter().map(|&l| nrzi.decode(l)).collect();
        assert_eq!(bits, vec![0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn nrzi_constant_level_is_ones() {
        let mut nrzi = NrziDecoder::new();
        nrzi.decode(1); // initial transition to set level
        let bits: Vec<u8> = (0..5).map(|_| nrzi.decode(1)).collect();
        assert_eq!(bits, vec![1, 1, 1, 1, 1]);
    }

    // ------------------------------------------------------------------
    // HDLC deframer tests
    // ------------------------------------------------------------------

    /// Helper: encode bytes to HDLC bit stream with flags and bit stuffing.
    fn hdlc_encode(data: &[u8]) -> Vec<u8> {
        let mut bits = Vec::new();

        // Opening flag 0x7E (MSB first for simplicity — we're testing
        // deframer logic, not the exact bit order of a specific protocol)
        let flag_bits = [0u8, 1, 1, 1, 1, 1, 1, 0];
        bits.extend_from_slice(&flag_bits);

        // Data with bit stuffing (LSB first within each byte, matching HDLC)
        let mut ones_count = 0;
        for &byte in data {
            for i in 0..8 {
                let bit = (byte >> i) & 1;
                bits.push(bit);
                if bit == 1 {
                    ones_count += 1;
                    if ones_count == 5 {
                        bits.push(0); // Stuff a zero
                        ones_count = 0;
                    }
                } else {
                    ones_count = 0;
                }
            }
        }

        // Closing flag
        bits.extend_from_slice(&flag_bits);
        bits
    }

    #[test]
    fn hdlc_extracts_simple_frame() {
        let data = vec![0x03, 0xF0, 0x21, 0x42];
        let bits = hdlc_encode(&data);

        let mut deframer = HdlcDeframer::new();
        let mut frames: Vec<Vec<u8>> = Vec::new();

        for &bit in &bits {
            if let Some(frame) = deframer.feed(bit) {
                frames.push(frame);
            }
        }

        assert_eq!(frames.len(), 1, "Should extract exactly one frame");
        assert_eq!(frames[0], data);
    }

    #[test]
    fn hdlc_handles_data_with_stuffing() {
        // 0xFF has 8 consecutive ones — needs stuffing
        let data = vec![0xFF, 0xFF];
        let bits = hdlc_encode(&data);

        let mut deframer = HdlcDeframer::new();
        let mut frames: Vec<Vec<u8>> = Vec::new();

        for &bit in &bits {
            if let Some(frame) = deframer.feed(bit) {
                frames.push(frame);
            }
        }

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], data);
    }

    #[test]
    fn hdlc_rejects_too_short() {
        // Frame with only 1 byte — below 2-byte minimum
        let data = vec![0x01];
        let bits = hdlc_encode(&data);

        let mut deframer = HdlcDeframer::new();
        let mut frames: Vec<Vec<u8>> = Vec::new();

        for &bit in &bits {
            if let Some(frame) = deframer.feed(bit) {
                frames.push(frame);
            }
        }

        // 1 byte = 8 bits < 16 minimum
        assert!(frames.is_empty(), "Single-byte frame should be rejected");
    }

    #[test]
    fn hdlc_multiple_frames() {
        let data1 = vec![0x01, 0x02, 0x03, 0x04];
        let data2 = vec![0x05, 0x06, 0x07, 0x08];

        let mut bits = Vec::new();
        bits.extend(hdlc_encode(&data1));
        bits.extend(hdlc_encode(&data2));

        let mut deframer = HdlcDeframer::new();
        let mut frames: Vec<Vec<u8>> = Vec::new();

        for &bit in &bits {
            if let Some(frame) = deframer.feed(bit) {
                frames.push(frame);
            }
        }

        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], data1);
        assert_eq!(frames[1], data2);
    }

    // ------------------------------------------------------------------
    // CRC-16-CCITT tests
    // ------------------------------------------------------------------

    #[test]
    fn crc16_known_vector() {
        // Standard test: CRC of "123456789" should be 0x906E
        let data = b"123456789";
        let crc = crc16_ccitt(data);
        assert_eq!(crc, 0x906E, "CRC-16-CCITT of '123456789' should be 0x906E, got 0x{:04X}", crc);
    }

    #[test]
    fn crc16_empty() {
        let crc = crc16_ccitt(&[]);
        assert_eq!(crc, 0xFFFF ^ 0xFFFF, "CRC of empty data should be 0x0000");
    }

    #[test]
    fn crc16_check_valid_frame() {
        let data = b"Hello";
        let crc = crc16_ccitt(data);

        // Append CRC (little-endian) to data
        let mut frame = data.to_vec();
        frame.push((crc & 0xFF) as u8);
        frame.push((crc >> 8) as u8);

        assert!(crc16_check(&frame), "CRC check should pass for valid frame");
    }

    #[test]
    fn crc16_check_invalid_frame() {
        let data = b"Hello";
        let mut frame = data.to_vec();
        frame.push(0x00);
        frame.push(0x00);

        assert!(!crc16_check(&frame), "CRC check should fail for invalid frame");
    }

    // ------------------------------------------------------------------
    // Clock recovery tests
    // ------------------------------------------------------------------

    #[test]
    fn clock_recovery_produces_symbols_at_baud_rate() {
        let baud = 1200.0;
        let sample_rate = 22050.0;
        let mut clock = ClockRecovery::new(baud, sample_rate, 0.02);

        // Generate square wave at baud rate
        let samples_per_bit = (sample_rate / baud) as usize;
        let mut symbol_count = 0;

        for bit_idx in 0..20 {
            let val = if bit_idx % 2 == 0 { 1.0f32 } else { -1.0 };
            for _ in 0..samples_per_bit {
                if clock.feed(val).is_some() {
                    symbol_count += 1;
                }
            }
        }

        // Should produce approximately 20 symbols (±2 for startup)
        assert!(
            (18..=22).contains(&symbol_count),
            "Expected ~20 symbols, got {symbol_count}"
        );
    }

    #[test]
    fn clock_recovery_correct_decisions() {
        let baud = 1200.0;
        let sample_rate = 22050.0;
        let mut clock = ClockRecovery::new(baud, sample_rate, 0.02);

        let samples_per_bit = (sample_rate / baud) as usize;
        let pattern = [1.0f32, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0];
        let mut decisions: Vec<f32> = Vec::new();

        for &val in &pattern {
            for _ in 0..samples_per_bit {
                if let Some(soft) = clock.feed(val) {
                    decisions.push(soft);
                }
            }
        }

        // After lock-in, decisions should follow the pattern signs
        // (skip first couple for convergence)
        if decisions.len() >= 4 {
            let late = &decisions[decisions.len() - 4..];
            // Last 4 should alternate correctly
            for (i, &d) in late.iter().enumerate() {
                let expected_sign = pattern[pattern.len() - 4 + i];
                assert_eq!(
                    d > 0.0, expected_sign > 0.0,
                    "Decision {} should match pattern sign", i
                );
            }
        }
    }

    // ------------------------------------------------------------------
    // FM discriminator test
    // ------------------------------------------------------------------

    #[test]
    fn fm_discriminator_detects_frequency() {
        use num_complex::Complex;

        let sample_rate = 22050.0;
        let freq = 1000.0; // 1 kHz tone
        let step = 2.0 * std::f64::consts::PI * freq / sample_rate;

        let mut prev = Complex::new(1.0f32, 0.0);
        let mut demod_sum = 0.0f64;
        let n = 100;

        for i in 1..=n {
            let phase = step * i as f64;
            let current = Complex::new(phase.cos() as f32, phase.sin() as f32);
            let d = fm_discriminate(current, prev);
            demod_sum += d as f64;
            prev = current;
        }

        let avg = demod_sum / n as f64;
        let expected = step; // Discriminator output ≈ phase step
        assert!(
            (avg - expected).abs() < 0.01,
            "FM discriminator average {avg:.4} should be close to phase step {expected:.4}"
        );
    }
}
