//! rtl_433 subprocess integration decoder.
//!
//! Bridges to the external [`rtl_433`](https://github.com/merbanan/rtl_433)
//! tool by piping IQ samples to its stdin and parsing JSON output from
//! stdout. This enables decoding of 200+ device protocols supported by
//! rtl_433 without reimplementing them.
//!
//! ## Signal Flow
//!
//! ```text
//! IQ samples (cf32) → convert to cu8 → rtl_433 stdin
//!                                          │
//!                    JSON lines ← rtl_433 stdout
//!                        │
//!                  parse → DecodedMessage
//! ```
//!
//! ## Requirements
//!
//! `rtl_433` must be installed and available on `$PATH`.
//!
//! ## Registered Variants
//!
//! | Name           | Center Frequency |
//! |----------------|------------------|
//! | `rtl433`       | 433.92 MHz       |
//! | `rtl433-315`   | 315 MHz          |
//! | `rtl433-868`   | 868 MHz          |
//! | `rtl433-915`   | 915 MHz          |

use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};

use crate::dsp::decoder::{DecoderPlugin, DecoderRequirements};
use crate::session::DecodedMessage;
use crate::types::Sample;

/// Decoder that delegates to an external `rtl_433` subprocess.
///
/// Lazily spawns rtl_433 on the first call to [`process()`](DecoderPlugin::process).
/// IQ samples are converted from cf32 to cu8 and written to rtl_433's stdin.
/// A reader thread parses JSON lines from stdout into [`DecodedMessage`]s.
pub struct Rtl433Decoder {
    sample_rate: f64,
    center_freq: f64,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    message_rx: Option<Receiver<DecodedMessage>>,
    reader_handle: Option<std::thread::JoinHandle<()>>,
    init_attempted: bool,
    init_error: Option<String>,
}

impl Rtl433Decoder {
    /// Create a new rtl_433 decoder.
    ///
    /// The subprocess is not started until the first call to `process()`.
    pub fn new(sample_rate: f64, center_freq: f64) -> Self {
        Self {
            sample_rate,
            center_freq,
            child: None,
            stdin: None,
            message_rx: None,
            reader_handle: None,
            init_attempted: false,
            init_error: None,
        }
    }

    /// Lazily spawn the rtl_433 subprocess. Returns true if ready.
    fn ensure_started(&mut self) -> bool {
        if self.child.is_some() {
            return true;
        }
        if self.init_attempted {
            return false;
        }
        self.init_attempted = true;

        let rate = self.sample_rate as u32;
        let freq = self.center_freq as u64;

        let result = Command::new("rtl_433")
            .args([
                "-r", "-",                  // read cu8 IQ from stdin
                "-s", &rate.to_string(),    // sample rate
                "-f", &freq.to_string(),    // center frequency
                "-F", "json",               // JSON output on stdout
                "-M", "utc",                // UTC timestamps
                "-M", "level",              // include signal level info
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();

        match result {
            Ok(mut child) => {
                let Some(stdin) = child.stdin.take() else {
                    self.init_error = Some("rtl_433: stdin not available".to_string());
                    child.kill().ok();
                    return false;
                };
                let Some(stdout) = child.stdout.take() else {
                    self.init_error = Some("rtl_433: stdout not available".to_string());
                    child.kill().ok();
                    return false;
                };

                let (tx, rx) = crossbeam_channel::unbounded();
                let handle = match std::thread::Builder::new()
                    .name("rtl433-reader".to_string())
                    .spawn(move || {
                        read_json_lines(stdout, tx);
                    }) {
                    Ok(h) => h,
                    Err(e) => {
                        self.init_error =
                            Some(format!("Failed to spawn rtl_433 reader thread: {e}"));
                        child.kill().ok();
                        return false;
                    }
                };

                self.child = Some(child);
                self.stdin = Some(stdin);
                self.message_rx = Some(rx);
                self.reader_handle = Some(handle);
                true
            }
            Err(e) => {
                self.init_error = Some(format!("Failed to start rtl_433: {e}"));
                false
            }
        }
    }
}

impl DecoderPlugin for Rtl433Decoder {
    fn name(&self) -> &str {
        "rtl433"
    }

    fn requirements(&self) -> DecoderRequirements {
        DecoderRequirements {
            center_frequency: self.center_freq,
            sample_rate: self.sample_rate,
            bandwidth: self.sample_rate,
            wants_iq: true,
        }
    }

    fn process(&mut self, samples: &[Sample]) -> Vec<DecodedMessage> {
        if !self.ensure_started() {
            // Report the init error exactly once
            if let Some(err) = self.init_error.take() {
                return vec![DecodedMessage {
                    decoder: "rtl433".to_string(),
                    timestamp: Instant::now(),
                    summary: err,
                    fields: BTreeMap::new(),
                    raw_bits: None,
                }];
            }
            return vec![];
        }

        // Convert cf32 → cu8 and write to rtl_433 stdin
        let cu8_data = samples_to_cu8(samples);
        if let Some(ref mut stdin) = self.stdin {
            if stdin.write_all(&cu8_data).is_err() {
                // rtl_433 process died — full cleanup to avoid leaking resources
                self.stdin = None;
                if let Some(mut child) = self.child.take() {
                    child.kill().ok();
                    child.wait().ok();
                }
                if let Some(handle) = self.reader_handle.take() {
                    handle.join().ok();
                }
                self.message_rx = None;
                return vec![DecodedMessage {
                    decoder: "rtl433".to_string(),
                    timestamp: Instant::now(),
                    summary: "rtl_433 process terminated unexpectedly".to_string(),
                    fields: BTreeMap::new(),
                    raw_bits: None,
                }];
            }
        }

        // Drain available messages (non-blocking)
        let mut messages = Vec::new();
        if let Some(ref rx) = self.message_rx {
            while let Ok(msg) = rx.try_recv() {
                messages.push(msg);
            }
        }

        messages
    }

    fn reset(&mut self) {
        // Drop stdin first so rtl_433 sees EOF and the reader thread can exit
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            child.kill().ok();
            child.wait().ok();
        }
        if let Some(handle) = self.reader_handle.take() {
            handle.join().ok();
        }
        self.message_rx = None;
        self.init_attempted = false;
        self.init_error = None;
    }
}

impl Drop for Rtl433Decoder {
    fn drop(&mut self) {
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            child.kill().ok();
            child.wait().ok();
        }
        // Reader thread exits when stdout closes — don't block in Drop
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Read JSON lines from rtl_433 stdout and send parsed messages.
fn read_json_lines(stdout: ChildStdout, tx: Sender<DecodedMessage>) {
    use std::io::{BufRead, BufReader};

    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            continue;
        }
        if let Some(msg) = parse_json_message(trimmed) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    }
}

/// Parse a single rtl_433 JSON line into a [`DecodedMessage`].
///
/// rtl_433 emits objects like:
/// ```json
/// {"time":"2024-01-01 12:00:00","model":"Acurite-5n1","id":1234,"temperature_C":22.5}
/// ```
fn parse_json_message(json_str: &str) -> Option<DecodedMessage> {
    let obj: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let map = obj.as_object()?;

    let model = map
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    // Collect all fields
    let mut fields = BTreeMap::new();
    for (key, value) in map {
        let val_str = match value {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => "null".to_string(),
            other => other.to_string(),
        };
        fields.insert(key.clone(), val_str);
    }

    // Build a human-readable summary
    let mut parts = vec![model.to_string()];
    if let Some(id) = map.get("id").and_then(|v| v.as_u64()) {
        parts.push(format!("id:{id}"));
    }
    for key in ["temperature_C", "temperature_F"] {
        if let Some(temp) = map.get(key).and_then(|v| v.as_f64()) {
            let unit = if key.ends_with('C') { "C" } else { "F" };
            parts.push(format!("{temp:.1}{unit}"));
            break;
        }
    }
    if let Some(hum) = map.get("humidity").and_then(|v| v.as_f64()) {
        parts.push(format!("{hum:.0}%RH"));
    }

    Some(DecodedMessage {
        decoder: "rtl433".to_string(),
        timestamp: Instant::now(),
        summary: parts.join(" | "),
        fields,
        raw_bits: None,
    })
}

/// Convert cf32 IQ samples to unsigned 8-bit IQ (cu8) for rtl_433.
///
/// cf32: `(f32, f32)` with range approximately `[-1, 1]`
/// cu8: `(u8, u8)` with range `[0, 255]`, center at 127.5
fn samples_to_cu8(samples: &[Sample]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        buf.push(((s.re * 127.5) + 127.5).clamp(0.0, 255.0) as u8);
        buf.push(((s.im * 127.5) + 127.5).clamp(0.0, 255.0) as u8);
    }
    buf
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_basic() {
        let json = r#"{"time":"2024-01-01 12:00:00","model":"Acurite-5n1","id":1234,"temperature_C":22.5,"humidity":45}"#;
        let msg = parse_json_message(json).unwrap();
        assert_eq!(msg.decoder, "rtl433");
        assert!(msg.summary.contains("Acurite-5n1"));
        assert!(msg.summary.contains("id:1234"));
        assert!(msg.summary.contains("22.5C"));
        assert!(msg.summary.contains("45%RH"));
        assert_eq!(msg.fields["model"], "Acurite-5n1");
        assert_eq!(msg.fields["temperature_C"], "22.5");
    }

    #[test]
    fn parse_json_minimal() {
        let json = r#"{"model":"Generic-Remote"}"#;
        let msg = parse_json_message(json).unwrap();
        assert_eq!(msg.summary, "Generic-Remote");
        assert_eq!(msg.fields.len(), 1);
    }

    #[test]
    fn parse_json_unknown_model() {
        let json = r#"{"data":"0xDEADBEEF"}"#;
        let msg = parse_json_message(json).unwrap();
        assert!(msg.summary.starts_with("unknown"));
    }

    #[test]
    fn parse_json_invalid() {
        assert!(parse_json_message("not json").is_none());
        assert!(parse_json_message("[]").is_none());
        assert!(parse_json_message("").is_none());
    }

    #[test]
    fn parse_json_temperature_f() {
        let json = r#"{"model":"Test","temperature_F":72.3}"#;
        let msg = parse_json_message(json).unwrap();
        assert!(msg.summary.contains("72.3F"));
    }

    #[test]
    fn parse_json_all_value_types() {
        let json = r#"{"model":"Test","active":true,"code":null,"data":[1,2],"count":42}"#;
        let msg = parse_json_message(json).unwrap();
        assert_eq!(msg.fields["active"], "true");
        assert_eq!(msg.fields["code"], "null");
        assert_eq!(msg.fields["data"], "[1,2]");
        assert_eq!(msg.fields["count"], "42");
    }

    #[test]
    fn samples_to_cu8_center() {
        // Zero IQ should map to ~128
        let samples = vec![Sample::new(0.0, 0.0)];
        let cu8 = samples_to_cu8(&samples);
        assert_eq!(cu8.len(), 2);
        // 0.0 * 127.5 + 127.5 = 127.5 → 127 as u8
        assert_eq!(cu8[0], 127);
        assert_eq!(cu8[1], 127);
    }

    #[test]
    fn samples_to_cu8_extremes() {
        let samples = vec![Sample::new(1.0, -1.0)];
        let cu8 = samples_to_cu8(&samples);
        assert_eq!(cu8[0], 255); // 1.0 * 127.5 + 127.5 = 255
        assert_eq!(cu8[1], 0);   // -1.0 * 127.5 + 127.5 = 0
    }

    #[test]
    fn samples_to_cu8_clamp() {
        // Values beyond [-1, 1] should be clamped
        let samples = vec![Sample::new(2.0, -2.0)];
        let cu8 = samples_to_cu8(&samples);
        assert_eq!(cu8[0], 255);
        assert_eq!(cu8[1], 0);
    }

    #[test]
    fn decoder_new_lazy_init() {
        let decoder = Rtl433Decoder::new(250_000.0, 433.92e6);
        assert!(decoder.child.is_none());
        assert!(!decoder.init_attempted);
    }

    #[test]
    fn decoder_requirements() {
        let decoder = Rtl433Decoder::new(250_000.0, 433.92e6);
        let req = decoder.requirements();
        assert!((req.center_frequency - 433.92e6).abs() < 1.0);
        assert!((req.sample_rate - 250_000.0).abs() < 1.0);
        assert!(req.wants_iq);
    }

    #[test]
    fn decoder_requirements_868() {
        let decoder = Rtl433Decoder::new(250_000.0, 868.0e6);
        let req = decoder.requirements();
        assert!((req.center_frequency - 868.0e6).abs() < 1.0);
    }

    #[test]
    fn decoder_name() {
        let decoder = Rtl433Decoder::new(250_000.0, 433.92e6);
        assert_eq!(decoder.name(), "rtl433");
    }

    #[test]
    fn decoder_reset_clears_state() {
        let mut decoder = Rtl433Decoder::new(250_000.0, 433.92e6);
        // Simulate failed init
        decoder.init_attempted = true;
        decoder.init_error = Some("test error".to_string());

        decoder.reset();

        assert!(!decoder.init_attempted);
        assert!(decoder.init_error.is_none());
        assert!(decoder.child.is_none());
    }
}
