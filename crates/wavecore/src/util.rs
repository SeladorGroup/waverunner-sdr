//! Shared utility functions for frequency/gain parsing and formatting.
//!
//! These are used across CLI commands and TUI to avoid duplication.

use std::time::SystemTime;

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
        let db: f64 = s.parse().map_err(|e| format!("invalid gain '{s}': {e}"))?;
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

/// Get the WaveRunner config directory (~/.config/waverunner).
///
/// Respects XDG_CONFIG_HOME, falls back to $HOME/.config.
pub fn config_dir() -> Option<std::path::PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        Some(std::path::PathBuf::from(xdg).join("waverunner"))
    } else if let Ok(home) = std::env::var("HOME") {
        Some(
            std::path::PathBuf::from(home)
                .join(".config")
                .join("waverunner"),
        )
    } else {
        None
    }
}

/// Directory used for generated capture artifacts and local workflow state.
pub fn capture_dir() -> Option<std::path::PathBuf> {
    config_dir().map(|d| d.join("captures"))
}

/// Format the current UTC time as ISO 8601 without pulling in chrono.
pub fn utc_timestamp_now() -> String {
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();

    let days = secs / 86_400;
    let time_secs = secs % 86_400;
    let hours = time_secs / 3_600;
    let minutes = (time_secs % 3_600) / 60;
    let seconds = time_secs % 60;

    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Compact UTC timestamp used for filenames.
pub fn utc_timestamp_compact() -> String {
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();

    let days = secs / 86_400;
    let time_secs = secs % 86_400;
    let hours = time_secs / 3_600;
    let minutes = (time_secs % 3_600) / 60;
    let seconds = time_secs % 60;

    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}{month:02}{day:02}T{hours:02}{minutes:02}{seconds:02}Z")
}

/// Convert arbitrary text into a filesystem-safe lowercase slug.
pub fn slugify(input: &str) -> String {
    let mut slug = String::with_capacity(input.len());
    let mut last_was_dash = false;

    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.is_empty() {
        "capture".to_string()
    } else {
        slug
    }
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year, m, d)
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

    #[test]
    fn slugify_compacts_text() {
        assert_eq!(slugify("FM Survey 94.9!"), "fm-survey-94-9");
        assert_eq!(slugify("   "), "capture");
    }

    #[test]
    fn compact_timestamp_shape() {
        let stamp = utc_timestamp_compact();
        assert_eq!(stamp.len(), 16);
        assert!(stamp.ends_with('Z'));
    }
}
