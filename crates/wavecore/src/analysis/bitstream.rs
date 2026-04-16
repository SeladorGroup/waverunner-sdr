//! Bit-level inspection for reverse engineering unknown protocols.
//!
//! Analyzes raw bitstreams from decoder output: pattern search,
//! autocorrelation for frame boundaries, run-length distribution,
//! entropy, ASCII extraction, and hex dump.

/// Configuration for bitstream analysis.
#[derive(Debug, Clone)]
pub struct BitstreamConfig {
    /// Raw bits to analyze (each byte is 0 or 1).
    pub bits: Vec<u8>,
    /// Bit patterns to search for (each byte is 0 or 1).
    pub search_patterns: Vec<Vec<u8>>,
}

/// Pattern match result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PatternMatch {
    /// Pattern in hex notation.
    pub pattern_hex: String,
    /// Bit offset of first occurrence.
    pub bit_offset: usize,
    /// Total number of occurrences.
    pub occurrences: usize,
}

/// ASCII string fragment found in the bitstream.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AsciiFragment {
    /// Byte offset where the string starts.
    pub byte_offset: usize,
    /// The ASCII text.
    pub text: String,
}

/// Bitstream analysis results.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BitstreamReport {
    /// Total bit count.
    pub length: usize,
    /// Fraction of 1-bits (0.0 – 1.0).
    pub ones_fraction: f32,
    /// Longest consecutive run of identical bits.
    pub max_run_length: usize,
    /// Run-length distribution: (run_length, count).
    pub run_length_dist: Vec<(usize, usize)>,
    /// Detected patterns with offsets.
    pub patterns: Vec<PatternMatch>,
    /// Byte-aligned ASCII strings found (min 3 chars).
    pub ascii_strings: Vec<AsciiFragment>,
    /// Autocorrelation peaks — possible frame/packet lengths in bits.
    pub frame_lengths: Vec<usize>,
    /// Shannon entropy per byte (0 = uniform, 8 = max entropy).
    pub entropy_per_byte: f32,
    /// Possible encoding detected (e.g., "NRZI", "Manchester", "8b10b").
    pub encoding_guess: Option<String>,
    /// Hex dump of the first 256 bytes.
    pub hex_dump: String,
}

/// Analyze a bitstream for patterns, structure, and statistics.
pub fn analyze_bitstream(config: &BitstreamConfig) -> BitstreamReport {
    let bits = &config.bits;

    if bits.is_empty() {
        return BitstreamReport {
            length: 0,
            ones_fraction: 0.0,
            max_run_length: 0,
            run_length_dist: Vec::new(),
            patterns: Vec::new(),
            ascii_strings: Vec::new(),
            frame_lengths: Vec::new(),
            entropy_per_byte: 0.0,
            encoding_guess: None,
            hex_dump: String::new(),
        };
    }

    let ones_count = bits.iter().filter(|&&b| b != 0).count();
    let ones_fraction = ones_count as f32 / bits.len() as f32;

    // Run-length analysis
    let (max_run, run_dist) = run_length_analysis(bits);

    // Pattern search
    let patterns: Vec<PatternMatch> = config
        .search_patterns
        .iter()
        .filter_map(|pattern| {
            let offsets = find_pattern(bits, pattern);
            if offsets.is_empty() {
                None
            } else {
                Some(PatternMatch {
                    pattern_hex: bits_to_hex(pattern),
                    bit_offset: offsets[0],
                    occurrences: offsets.len(),
                })
            }
        })
        .collect();

    // ASCII extraction
    let ascii_strings = extract_ascii(bits);

    // Autocorrelation for frame length detection
    let max_lag = bits.len().min(2048);
    let frame_lengths = bit_autocorrelation(bits, max_lag);

    // Entropy
    let entropy = byte_entropy(bits);

    // Encoding guess
    let encoding_guess = guess_encoding(bits, &run_dist);

    // Hex dump
    let hex_dump = make_hex_dump(bits, 256);

    BitstreamReport {
        length: bits.len(),
        ones_fraction,
        max_run_length: max_run,
        run_length_dist: run_dist,
        patterns,
        ascii_strings,
        frame_lengths,
        entropy_per_byte: entropy,
        encoding_guess,
        hex_dump,
    }
}

/// Search for a bit pattern in a bitstream, returning all bit offsets.
pub fn find_pattern(bits: &[u8], pattern: &[u8]) -> Vec<usize> {
    if pattern.is_empty() || bits.len() < pattern.len() {
        return Vec::new();
    }

    let mut offsets = Vec::new();
    for i in 0..=bits.len() - pattern.len() {
        if &bits[i..i + pattern.len()] == pattern {
            offsets.push(i);
        }
    }
    offsets
}

/// Compute bit-level autocorrelation to detect frame/packet boundaries.
///
/// Returns lag values where correlation peaks occur (possible frame lengths).
pub fn bit_autocorrelation(bits: &[u8], max_lag: usize) -> Vec<usize> {
    if bits.len() < 16 {
        return Vec::new();
    }

    let max_lag = max_lag.min(bits.len() / 2);
    let n = bits.len();

    // Convert bits to ±1 for correlation
    let signal: Vec<f32> = bits
        .iter()
        .map(|&b| if b != 0 { 1.0 } else { -1.0 })
        .collect();

    // Compute normalized autocorrelation
    let r0: f32 = signal.iter().map(|s| s * s).sum();
    if r0 < 1e-10 {
        return Vec::new();
    }

    let mut correlations: Vec<(usize, f32)> = Vec::new();
    for lag in 8..max_lag {
        let mut r: f32 = 0.0;
        for i in 0..(n - lag) {
            r += signal[i] * signal[i + lag];
        }
        let normalized = r / r0;
        correlations.push((lag, normalized));
    }

    // Find peaks above threshold
    let threshold = 0.3;
    let mut peaks = Vec::new();

    for i in 1..correlations.len().saturating_sub(1) {
        let (lag, val) = correlations[i];
        if val > threshold && val > correlations[i - 1].1 && val > correlations[i + 1].1 {
            peaks.push(lag);
        }
    }

    // Limit to top 5 peaks
    peaks.truncate(5);
    peaks
}

/// Compute run-length distribution. Returns (max_run_length, distribution).
fn run_length_analysis(bits: &[u8]) -> (usize, Vec<(usize, usize)>) {
    if bits.is_empty() {
        return (0, Vec::new());
    }

    let mut max_run = 1usize;
    let mut current_run = 1usize;
    let mut run_counts: std::collections::BTreeMap<usize, usize> =
        std::collections::BTreeMap::new();

    for i in 1..bits.len() {
        if (bits[i] != 0) == (bits[i - 1] != 0) {
            current_run += 1;
        } else {
            max_run = max_run.max(current_run);
            *run_counts.entry(current_run).or_insert(0) += 1;
            current_run = 1;
        }
    }
    // Final run
    max_run = max_run.max(current_run);
    *run_counts.entry(current_run).or_insert(0) += 1;

    let dist: Vec<(usize, usize)> = run_counts.into_iter().collect();
    (max_run, dist)
}

/// Extract byte-aligned ASCII strings (min 3 printable chars).
fn extract_ascii(bits: &[u8]) -> Vec<AsciiFragment> {
    let mut fragments = Vec::new();
    let num_bytes = bits.len() / 8;

    let mut current_str = String::new();
    let mut start_byte = 0;

    for byte_idx in 0..num_bytes {
        let mut byte_val = 0u8;
        for bit in 0..8 {
            byte_val = (byte_val << 1) | (bits[byte_idx * 8 + bit] & 1);
        }

        if (0x20..=0x7E).contains(&byte_val) {
            if current_str.is_empty() {
                start_byte = byte_idx;
            }
            current_str.push(byte_val as char);
        } else {
            if current_str.len() >= 3 {
                fragments.push(AsciiFragment {
                    byte_offset: start_byte,
                    text: current_str.clone(),
                });
            }
            current_str.clear();
        }
    }

    if current_str.len() >= 3 {
        fragments.push(AsciiFragment {
            byte_offset: start_byte,
            text: current_str,
        });
    }

    fragments
}

/// Shannon entropy per byte of the bitstream.
fn byte_entropy(bits: &[u8]) -> f32 {
    let num_bytes = bits.len() / 8;
    if num_bytes == 0 {
        return 0.0;
    }

    let mut counts = [0u32; 256];
    for byte_idx in 0..num_bytes {
        let mut byte_val = 0u8;
        for bit in 0..8 {
            byte_val = (byte_val << 1) | (bits[byte_idx * 8 + bit] & 1);
        }
        counts[byte_val as usize] += 1;
    }

    let n = num_bytes as f64;
    let mut entropy = 0.0f64;
    for &count in &counts {
        if count > 0 {
            let p = count as f64 / n;
            entropy -= p * p.log2();
        }
    }

    entropy as f32
}

/// Guess encoding based on run-length distribution.
fn guess_encoding(bits: &[u8], run_dist: &[(usize, usize)]) -> Option<String> {
    if bits.is_empty() || run_dist.is_empty() {
        return None;
    }

    // Manchester: runs should cluster around 1T and 2T (where T = symbol period)
    // Look for bimodal distribution at lengths N and 2N
    let dominant_runs: Vec<&(usize, usize)> =
        run_dist.iter().filter(|(_, count)| *count > 5).collect();

    if dominant_runs.len() == 2 {
        let (l1, _) = dominant_runs[0];
        let (l2, _) = dominant_runs[1];
        let ratio = *l2 as f64 / *l1 as f64;
        if (ratio - 2.0).abs() < 0.3 {
            return Some("Manchester".to_string());
        }
    }

    // NRZI: look at transition density
    let transitions = bits
        .windows(2)
        .filter(|w| (w[0] != 0) != (w[1] != 0))
        .count();
    let transition_density = transitions as f64 / (bits.len() - 1).max(1) as f64;

    // NRZI with bit stuffing: transition density ~40-60%
    if (0.35..=0.65).contains(&transition_density) {
        return Some("NRZI (possible)".to_string());
    }

    None
}

/// Convert a bit slice to hex string.
fn bits_to_hex(bits: &[u8]) -> String {
    let num_bytes = bits.len().div_ceil(8);
    let mut hex = String::with_capacity(num_bytes * 2);
    for byte_idx in 0..num_bytes {
        let mut byte_val = 0u8;
        for bit in 0..8 {
            let i = byte_idx * 8 + bit;
            byte_val <<= 1;
            if i < bits.len() && bits[i] != 0 {
                byte_val |= 1;
            }
        }
        hex.push_str(&format!("{byte_val:02X}"));
    }
    hex
}

/// Generate a hex dump of the bitstream (byte-aligned).
fn make_hex_dump(bits: &[u8], max_bytes: usize) -> String {
    let num_bytes = (bits.len() / 8).min(max_bytes);
    let mut dump = String::new();

    for row_start in (0..num_bytes).step_by(16) {
        let row_end = (row_start + 16).min(num_bytes);
        dump.push_str(&format!("{row_start:04X}  "));

        // Hex portion
        for i in row_start..row_end {
            let mut byte_val = 0u8;
            for bit in 0..8 {
                byte_val = (byte_val << 1) | (bits[i * 8 + bit] & 1);
            }
            dump.push_str(&format!("{byte_val:02X} "));
        }
        // Pad if short row
        for _ in row_end..row_start + 16 {
            dump.push_str("   ");
        }

        dump.push_str(" |");
        // ASCII portion
        for i in row_start..row_end {
            let mut byte_val = 0u8;
            for bit in 0..8 {
                byte_val = (byte_val << 1) | (bits[i * 8 + bit] & 1);
            }
            if (0x20..=0x7E).contains(&byte_val) {
                dump.push(byte_val as char);
            } else {
                dump.push('.');
            }
        }
        dump.push_str("|\n");
    }

    dump
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_balance_all_ones() {
        let config = BitstreamConfig {
            bits: vec![1; 100],
            search_patterns: Vec::new(),
        };
        let report = analyze_bitstream(&config);
        assert!((report.ones_fraction - 1.0).abs() < 0.001);
    }

    #[test]
    fn bit_balance_alternating() {
        let bits: Vec<u8> = (0..1000).map(|i| (i % 2) as u8).collect();
        let config = BitstreamConfig {
            bits,
            search_patterns: Vec::new(),
        };
        let report = analyze_bitstream(&config);
        assert!((report.ones_fraction - 0.5).abs() < 0.01);
    }

    #[test]
    fn find_pattern_simple() {
        let bits = vec![0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 1, 0];
        //               0  1  2  3  4  5  6  7  8  9 10 11 12 13
        let pattern = vec![1, 0, 1, 0];
        let offsets = find_pattern(&bits, &pattern);
        assert!(!offsets.is_empty());
        // bits[4..8] = [1, 0, 1, 0] — first match is at offset 4
        assert_eq!(offsets[0], 4);
    }

    #[test]
    fn find_pattern_multiple() {
        // Repeated pattern
        let mut bits = Vec::new();
        for _ in 0..5 {
            bits.extend_from_slice(&[1, 1, 0, 0, 1, 0, 0, 0]);
        }
        let pattern = vec![1, 1, 0, 0];
        let offsets = find_pattern(&bits, &pattern);
        assert_eq!(offsets.len(), 5);
    }

    #[test]
    fn run_length_distribution() {
        // 3 ones, 2 zeros, 3 ones, 2 zeros
        let bits = vec![1, 1, 1, 0, 0, 1, 1, 1, 0, 0];
        let (max_run, dist) = run_length_analysis(&bits);
        assert_eq!(max_run, 3);
        assert!(dist.contains(&(3, 2))); // two runs of length 3
        assert!(dist.contains(&(2, 2))); // two runs of length 2
    }

    #[test]
    fn bit_autocorrelation_periodic() {
        // Periodic signal with period 16 bits
        let period = 16;
        let pattern: Vec<u8> = vec![1, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0];
        let mut bits = Vec::new();
        for _ in 0..50 {
            bits.extend_from_slice(&pattern);
        }
        let peaks = bit_autocorrelation(&bits, 128);
        // Should detect period of 16
        assert!(
            peaks.iter().any(|&lag| lag == period || lag == period * 2),
            "Expected period {period} in peaks: {peaks:?}"
        );
    }

    #[test]
    fn ascii_extraction() {
        // Encode "Hello" as bits
        let text = "Hello";
        let mut bits = Vec::new();
        for &byte in text.as_bytes() {
            for bit in (0..8).rev() {
                bits.push((byte >> bit) & 1);
            }
        }
        // Pad with non-printable
        bits.extend(std::iter::repeat_n(0, 24));
        let frags = extract_ascii(&bits);
        assert_eq!(frags.len(), 1);
        assert_eq!(frags[0].text, "Hello");
    }

    #[test]
    fn hex_dump_format() {
        let bits = vec![1, 0, 1, 0, 1, 0, 1, 0, 0, 0, 0, 0, 1, 1, 1, 1]; // 0xAA, 0x0F
        let dump = make_hex_dump(&bits, 256);
        assert!(dump.contains("AA"), "Hex dump should contain AA: {dump}");
        assert!(dump.contains("0F"), "Hex dump should contain 0F: {dump}");
    }
}
