//! Shared utility functions for frequency/gain parsing and formatting.
//!
//! These are used across CLI commands and TUI to avoid duplication.

use crate::hardware::GainMode;

/// Parse frequency strings with SI suffixes.
///
/// Supports Hz (bare number), kHz (`k`/`K`), MHz (`m`/`M`), GHz (`g`/`G`).
///
/// # Examples
///
/// ```
/// use wavecore::util::parse_frequency;
/// assert!((parse_frequency("433.92M").unwrap() - 433_920_000.0).abs() < 0.1);
/// assert!((parse_frequency("1.09G").unwrap() - 1_090_000_000.0).abs() < 0.1);
/// assert!((parse_frequency("1000k").unwrap() - 1_000_000.0).abs() < 0.1);
/// assert!((parse_frequency("433920000").unwrap() - 433_920_000.0).abs() < 0.1);
/// ```
pub fn parse_frequency(s: &str) -> Result<f64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty frequency string".to_string());
    }

    let (num_str, multiplier) = match s.as_bytes().last() {
        Some(b'G' | b'g') => (&s[..s.len() - 1], 1e9),
        Some(b'M' | b'm') => (&s[..s.len() - 1], 1e6),
        Some(b'K' | b'k') => (&s[..s.len() - 1], 1e3),
        _ => (s, 1.0),
    };

    num_str
        .parse::<f64>()
        .map(|v| v * multiplier)
        .map_err(|e| format!("invalid frequency '{s}': {e}"))
}

/// Parse gain string: "auto" for AGC, or a dB value for manual gain.
///
/// # Examples
///
/// ```
/// use wavecore::util::parse_gain;
/// use wavecore::hardware::GainMode;
/// assert_eq!(parse_gain("auto").unwrap(), GainMode::Auto);
/// assert_eq!(parse_gain("AUTO").unwrap(), GainMode::Auto);
/// assert_eq!(parse_gain("40.2").unwrap(), GainMode::Manual(40.2));
/// ```
pub fn parse_gain(s: &str) -> Result<GainMode, String> {
    if s.eq_ignore_ascii_case("auto") {
        Ok(GainMode::Auto)
    } else {
        let db: f64 = s
            .parse()
            .map_err(|e| format!("invalid gain '{s}': {e}"))?;
        Ok(GainMode::Manual(db))
    }
}

/// Format frequency for human-readable display.
///
/// Automatically selects GHz, MHz, kHz, or Hz based on magnitude.
///
/// # Examples
///
/// ```
/// use wavecore::util::format_freq;
/// assert_eq!(format_freq(433_920_000.0), "433.920000 MHz");
/// assert_eq!(format_freq(1_090_000_000.0), "1.090000 GHz");
/// ```
pub fn format_freq(hz: f64) -> String {
    if hz >= 1e9 {
        format!("{:.6} GHz", hz / 1e9)
    } else if hz >= 1e6 {
        format!("{:.6} MHz", hz / 1e6)
    } else if hz >= 1e3 {
        format!("{:.3} kHz", hz / 1e3)
    } else {
        format!("{:.1} Hz", hz)
    }
}

/// Format tuning step size for display.
///
/// Uses whole numbers (no decimal places) for compact display.
pub fn format_step(hz: f64) -> String {
    if hz >= 1e6 {
        format!("{:.0} MHz", hz / 1e6)
    } else if hz >= 1e3 {
        format!("{:.0} kHz", hz / 1e3)
    } else {
        format!("{:.0} Hz", hz)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frequency_mhz() {
        assert!((parse_frequency("433.92M").unwrap() - 433_920_000.0).abs() < 0.1);
        assert!((parse_frequency("100m").unwrap() - 100_000_000.0).abs() < 0.1);
    }

    #[test]
    fn parse_frequency_ghz() {
        assert!((parse_frequency("1.09G").unwrap() - 1_090_000_000.0).abs() < 0.1);
    }

    #[test]
    fn parse_frequency_khz() {
        assert!((parse_frequency("1000k").unwrap() - 1_000_000.0).abs() < 0.1);
        assert!((parse_frequency("200K").unwrap() - 200_000.0).abs() < 0.1);
    }

    #[test]
    fn parse_frequency_hz() {
        assert!((parse_frequency("433920000").unwrap() - 433_920_000.0).abs() < 0.1);
    }

    #[test]
    fn parse_frequency_invalid() {
        assert!(parse_frequency("").is_err());
        assert!(parse_frequency("abc").is_err());
        assert!(parse_frequency("M").is_err());
    }

    #[test]
    fn parse_gain_auto() {
        assert_eq!(parse_gain("auto").unwrap(), GainMode::Auto);
        assert_eq!(parse_gain("AUTO").unwrap(), GainMode::Auto);
        assert_eq!(parse_gain("Auto").unwrap(), GainMode::Auto);
    }

    #[test]
    fn parse_gain_manual() {
        assert_eq!(parse_gain("40.2").unwrap(), GainMode::Manual(40.2));
        assert_eq!(parse_gain("0").unwrap(), GainMode::Manual(0.0));
    }

    #[test]
    fn parse_gain_invalid() {
        assert!(parse_gain("xyz").is_err());
    }

    #[test]
    fn format_freq_ranges() {
        assert!(format_freq(1_500_000_000.0).contains("GHz"));
        assert!(format_freq(433_920_000.0).contains("MHz"));
        assert!(format_freq(25_000.0).contains("kHz"));
        assert!(format_freq(100.0).contains("Hz"));
    }

    #[test]
    fn format_step_ranges() {
        assert_eq!(format_step(1_000_000.0), "1 MHz");
        assert_eq!(format_step(25_000.0), "25 kHz");
        assert_eq!(format_step(100.0), "100 Hz");
    }
}
