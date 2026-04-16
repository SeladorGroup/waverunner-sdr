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
use std::time::Instant;

use crate::dsp::decoder::{DecoderPlugin, DecoderRequirements};
use crate::session::DecodedMessage;
use crate::types::Sample;

use super::subprocess::{
    InputFormat, OutputParser, SubprocessBridge, SubprocessConfig, samples_to_cu8,
};
use super::tools;

/// Decoder that delegates to an external `rtl_433` subprocess.
pub struct Rtl433Decoder {
    decoder_name: String,
    sample_rate: f64,
    center_freq: f64,
    bridge: SubprocessBridge,
    config: Option<SubprocessConfig>,
}

impl Rtl433Decoder {
    pub fn new(sample_rate: f64, center_freq: f64) -> Self {
        Self::named(sample_rate, center_freq, "rtl433")
    }

    pub fn named(sample_rate: f64, center_freq: f64, decoder_name: impl Into<String>) -> Self {
        let decoder_name = decoder_name.into();
        let config = Self::make_config(sample_rate, center_freq, &decoder_name);
        Self {
            decoder_name,
            sample_rate,
            center_freq,
            bridge: SubprocessBridge::new(),
            config: Some(config),
        }
    }

    fn make_config(sample_rate: f64, center_freq: f64, decoder_name: &str) -> SubprocessConfig {
        let rate = sample_rate as u32;
        let freq = center_freq as u64;

        SubprocessConfig {
            command: tools::resolve_tool_command("rtl_433")
                .unwrap_or("rtl_433")
                .to_string(),
            args: vec![
                "-r".to_string(),
                "-".to_string(), // read cu8 IQ from stdin
                "-s".to_string(),
                rate.to_string(), // sample rate
                "-f".to_string(),
                freq.to_string(), // center frequency
                "-F".to_string(),
                "json".to_string(), // JSON output
                "-M".to_string(),
                "utc".to_string(), // UTC timestamps
                "-M".to_string(),
                "level".to_string(), // signal level info
            ],
            input_format: InputFormat::Cu8Iq,
            output_parser: Box::new(Rtl433Parser {
                decoder_name: decoder_name.to_string(),
            }),
            thread_name: "rtl433-reader".to_string(),
        }
    }
}

impl DecoderPlugin for Rtl433Decoder {
    fn name(&self) -> &str {
        &self.decoder_name
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
        if !tools::is_tool_available("rtl_433") {
            if !self.bridge.init_attempted {
                self.bridge.init_attempted = true;
                return vec![DecodedMessage {
                    decoder: "rtl433".to_string(),
                    timestamp: Instant::now(),
                    summary: format!(
                        "rtl_433 not installed. Install with: {}",
                        tools::install_hint("rtl_433")
                    ),
                    fields: BTreeMap::new(),
                    raw_bits: None,
                }];
            }
            return vec![];
        }

        if let Some(ref mut config) = self.config.take() {
            if let Err(err) = self.bridge.ensure_started(config) {
                return vec![DecodedMessage {
                    decoder: "rtl433".to_string(),
                    timestamp: Instant::now(),
                    summary: err,
                    fields: BTreeMap::new(),
                    raw_bits: None,
                }];
            }
        } else if !self.bridge.is_running() {
            return vec![];
        }

        let cu8_data = samples_to_cu8(samples);
        if !self.bridge.write_stdin(&cu8_data) {
            let summary = self
                .bridge
                .take_recent_stderr()
                .map(|stderr| format!("rtl_433 process terminated unexpectedly: {stderr}"))
                .unwrap_or_else(|| "rtl_433 process terminated unexpectedly".to_string());
            return vec![DecodedMessage {
                decoder: "rtl433".to_string(),
                timestamp: Instant::now(),
                summary,
                fields: BTreeMap::new(),
                raw_bits: None,
            }];
        }

        self.bridge.drain_messages()
    }

    fn reset(&mut self) {
        self.bridge.reset();
        self.config = Some(Self::make_config(
            self.sample_rate,
            self.center_freq,
            &self.decoder_name,
        ));
    }
}

// ============================================================================
// rtl_433 JSON output parser
// ============================================================================

struct Rtl433Parser {
    decoder_name: String,
}

impl OutputParser for Rtl433Parser {
    fn parse_line(&mut self, line: &str) -> Option<DecodedMessage> {
        if !line.starts_with('{') {
            return None;
        }
        parse_json_message(line, &self.decoder_name)
    }
}

/// Parse a single rtl_433 JSON line into a [`DecodedMessage`].
fn parse_json_message(json_str: &str, decoder_name: &str) -> Option<DecodedMessage> {
    let obj: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let map = obj.as_object()?;

    let model = map
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

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
        decoder: decoder_name.to_string(),
        timestamp: Instant::now(),
        summary: parts.join(" | "),
        fields,
        raw_bits: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_basic() {
        let json = r#"{"time":"2024-01-01 12:00:00","model":"Acurite-5n1","id":1234,"temperature_C":22.5,"humidity":45}"#;
        let msg = parse_json_message(json, "rtl433").unwrap();
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
        let msg = parse_json_message(json, "rtl433").unwrap();
        assert_eq!(msg.summary, "Generic-Remote");
        assert_eq!(msg.fields.len(), 1);
    }

    #[test]
    fn parse_json_unknown_model() {
        let json = r#"{"data":"0xDEADBEEF"}"#;
        let msg = parse_json_message(json, "rtl433").unwrap();
        assert!(msg.summary.starts_with("unknown"));
    }

    #[test]
    fn parse_json_invalid() {
        assert!(parse_json_message("not json", "rtl433").is_none());
        assert!(parse_json_message("[]", "rtl433").is_none());
        assert!(parse_json_message("", "rtl433").is_none());
    }

    #[test]
    fn parse_json_temperature_f() {
        let json = r#"{"model":"Test","temperature_F":72.3}"#;
        let msg = parse_json_message(json, "rtl433").unwrap();
        assert!(msg.summary.contains("72.3F"));
    }

    #[test]
    fn parse_json_all_value_types() {
        let json = r#"{"model":"Test","active":true,"code":null,"data":[1,2],"count":42}"#;
        let msg = parse_json_message(json, "rtl433").unwrap();
        assert_eq!(msg.fields["active"], "true");
        assert_eq!(msg.fields["code"], "null");
        assert_eq!(msg.fields["data"], "[1,2]");
        assert_eq!(msg.fields["count"], "42");
    }

    #[test]
    fn decoder_new_lazy_init() {
        let decoder = Rtl433Decoder::new(250_000.0, 433.92e6);
        assert!(!decoder.bridge.is_running());
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
    fn named_decoder_preserves_variant_name() {
        let decoder = Rtl433Decoder::named(250_000.0, 868.0e6, "rtl433-868");
        assert_eq!(decoder.name(), "rtl433-868");
    }
}
