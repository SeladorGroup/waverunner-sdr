//! RDS decoder via external `redsea` tool.
//!
//! Pipes FM-demodulated audio to `redsea` stdin as raw S16LE PCM.
//! Parses JSON output for station name, radio text, program type, etc.
//!
//! ## Requirements
//!
//! `redsea` must be installed: `sudo pacman -S redsea`

use std::collections::BTreeMap;
use std::time::Instant;

use crate::dsp::decoder::{DecoderPlugin, DecoderRequirements};
use crate::dsp::resample::PolyphaseResampler;
use crate::session::DecodedMessage;
use crate::types::Sample;

use super::subprocess::{
    InputFormat, OutputParser, SubprocessBridge, SubprocessConfig, audio_to_s16le, fm_demod,
};
use super::tools;

/// Preferred MPX sample rate for `redsea`.
pub const REDSEA_INPUT_SAMPLE_RATE_HZ: f64 = 171_000.0;
/// Hardware capture rate for broadcast FM before channelization.
pub const RDS_CAPTURE_SAMPLE_RATE_HZ: f64 = 1_024_000.0;
/// Decoder input rate after DDC/channel filtering.
pub const RDS_DECODER_SAMPLE_RATE_HZ: f64 = 256_000.0;
/// Broadcast FM channel bandwidth wide enough for stereo and the 57 kHz RDS subcarrier.
pub const RDS_CHANNEL_BANDWIDTH_HZ: f64 = 180_000.0;

pub struct RdsDecoder {
    input_sample_rate: f64,
    bridge: SubprocessBridge,
    config: Option<SubprocessConfig>,
    prev_sample: Sample,
    audio_resampler: Option<PolyphaseResampler>,
}

impl RdsDecoder {
    pub fn new(input_sample_rate: f64) -> Self {
        Self {
            input_sample_rate,
            bridge: SubprocessBridge::new(),
            config: Some(Self::make_config()),
            prev_sample: Sample::new(0.0, 0.0),
            audio_resampler: make_audio_resampler(input_sample_rate),
        }
    }

    fn make_config() -> SubprocessConfig {
        SubprocessConfig {
            command: tools::resolve_tool_command("redsea")
                .unwrap_or("redsea")
                .to_string(),
            args: vec![
                "-r".to_string(),
                REDSEA_INPUT_SAMPLE_RATE_HZ.to_string(),
                "-p".to_string(),
                "-u".to_string(),
            ],
            input_format: InputFormat::S16LeAudio,
            output_parser: Box::new(RedseaParser),
            thread_name: "redsea-reader".to_string(),
        }
    }
}

impl DecoderPlugin for RdsDecoder {
    fn name(&self) -> &str {
        "rds"
    }

    fn requirements(&self) -> DecoderRequirements {
        DecoderRequirements {
            center_frequency: 0.0, // works at any FM broadcast frequency
            sample_rate: self.input_sample_rate,
            bandwidth: RDS_CHANNEL_BANDWIDTH_HZ,
            wants_iq: true,
        }
    }

    fn process(&mut self, samples: &[Sample]) -> Vec<DecodedMessage> {
        // Check if redsea is available
        if !tools::is_tool_available("redsea") {
            if !self.bridge.init_attempted {
                self.bridge.init_attempted = true;
                return vec![DecodedMessage {
                    decoder: "rds".to_string(),
                    timestamp: Instant::now(),
                    summary: format!(
                        "redsea not installed. Install with: {}",
                        tools::install_hint("redsea")
                    ),
                    fields: BTreeMap::new(),
                    raw_bits: None,
                }];
            }
            return vec![];
        }

        // Start subprocess if needed
        if let Some(ref mut config) = self.config.take() {
            // config is taken once; subsequent calls skip this
            if let Err(err) = self.bridge.ensure_started(config) {
                return vec![DecodedMessage {
                    decoder: "rds".to_string(),
                    timestamp: Instant::now(),
                    summary: err,
                    fields: BTreeMap::new(),
                    raw_bits: None,
                }];
            }
        } else if !self.bridge.is_running() {
            // Already tried and failed
            return vec![];
        }

        // FM demodulate IQ → audio, then convert to S16LE
        let audio = fm_demod(samples, &mut self.prev_sample);
        let audio = if let Some(ref mut resampler) = self.audio_resampler {
            let mono: Vec<Sample> = audio.iter().map(|&s| Sample::new(s, 0.0)).collect();
            resampler.process(&mono).into_iter().map(|s| s.re).collect()
        } else {
            audio
        };
        let s16le = audio_to_s16le(&audio);

        if !self.bridge.write_stdin(&s16le) {
            let summary = self
                .bridge
                .take_recent_stderr()
                .map(|stderr| format!("redsea process terminated unexpectedly: {stderr}"))
                .unwrap_or_else(|| "redsea process terminated unexpectedly".to_string());
            return vec![DecodedMessage {
                decoder: "rds".to_string(),
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
        self.prev_sample = Sample::new(0.0, 0.0);
        self.audio_resampler = make_audio_resampler(self.input_sample_rate);
        self.config = Some(Self::make_config());
    }
}

fn make_audio_resampler(iq_sample_rate: f64) -> Option<PolyphaseResampler> {
    let input_rate = iq_sample_rate.round() as usize;
    let output_rate = REDSEA_INPUT_SAMPLE_RATE_HZ as usize;
    if input_rate == output_rate {
        None
    } else {
        Some(PolyphaseResampler::new(output_rate, input_rate, 128, 0.0))
    }
}

// ============================================================================
// Redsea JSON parser
// ============================================================================

struct RedseaParser;

impl OutputParser for RedseaParser {
    fn parse_line(&mut self, line: &str) -> Option<DecodedMessage> {
        if !line.starts_with('{') {
            return None;
        }
        let obj: serde_json::Value = serde_json::from_str(line).ok()?;
        let map = obj.as_object()?;

        let mut fields = BTreeMap::new();
        let mut parts = Vec::new();

        // PI code (station identifier)
        if let Some(pi) = map.get("pi").and_then(|v| v.as_str()) {
            fields.insert("pi".to_string(), pi.to_string());
            parts.push(format!("PI:{pi}"));
        }

        // Program Service name (station name)
        if let Some(ps) = first_non_empty_str(map, &["ps", "partial_ps"]) {
            fields.insert("ps".to_string(), ps.to_string());
            parts.push(ps.to_string());
        }

        // Radio Text
        if let Some(rt) = first_non_empty_str(map, &["radiotext", "partial_radiotext"]) {
            fields.insert("radiotext".to_string(), rt.to_string());
            parts.push(format!("RT: {rt}"));
        }

        // Program Type
        if let Some(pty) = first_non_empty_str(map, &["prog_type"]) {
            fields.insert("prog_type".to_string(), pty.to_string());
            parts.push(pty.to_string());
        }

        // Callsign (RBDS)
        if let Some(call) = first_non_empty_str(map, &["callsign"]) {
            fields.insert("callsign".to_string(), call.to_string());
            parts.push(call.to_string());
        }

        // Traffic info
        if let Some(tp) = map.get("tp").and_then(|v| v.as_bool()) {
            if tp {
                fields.insert("tp".to_string(), "true".to_string());
                parts.push("TP".to_string());
            }
        }
        if let Some(ta) = map.get("ta").and_then(|v| v.as_bool()) {
            if ta {
                fields.insert("ta".to_string(), "true".to_string());
                parts.push("TA".to_string());
            }
        }

        // Store all raw fields too
        for (key, value) in map {
            if !fields.contains_key(key) {
                let val_str = match value {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    other => other.to_string(),
                };
                fields.insert(key.clone(), val_str);
            }
        }

        if parts.is_empty() {
            parts.push("RDS group".to_string());
        }

        Some(DecodedMessage {
            decoder: "rds".to_string(),
            timestamp: Instant::now(),
            summary: parts.join(" | "),
            fields,
            raw_bits: None,
        })
    }
}

fn first_non_empty_str<'a>(
    map: &'a serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<&'a str> {
    keys.iter()
        .filter_map(|key| map.get(*key).and_then(|value| value.as_str()))
        .map(str::trim)
        .find(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rds_full() {
        let mut parser = RedseaParser;
        let json = r#"{"pi":"0x54A5","ps":"KQED-FM","radiotext":"Morning Edition","prog_type":"News","callsign":"KQED"}"#;
        let msg = parser.parse_line(json).unwrap();
        assert_eq!(msg.decoder, "rds");
        assert!(msg.summary.contains("KQED-FM"));
        assert!(msg.summary.contains("Morning Edition"));
        assert_eq!(msg.fields["callsign"], "KQED");
    }

    #[test]
    fn parse_rds_minimal() {
        let mut parser = RedseaParser;
        let json = r#"{"pi":"0x1234"}"#;
        let msg = parser.parse_line(json).unwrap();
        assert!(msg.summary.contains("PI:0x1234"));
    }

    #[test]
    fn parse_rds_partial_fields() {
        let mut parser = RedseaParser;
        let json = r#"{"pi":"0xD206","partial_ps":"RADIO S","partial_radiotext":"Now playing","prog_type":"","callsign":""}"#;
        let msg = parser.parse_line(json).unwrap();
        assert!(msg.summary.contains("RADIO S"));
        assert!(msg.summary.contains("Now playing"));
        assert!(!msg.summary.ends_with(" | "));
    }

    #[test]
    fn parse_rds_skip_non_json() {
        let mut parser = RedseaParser;
        assert!(parser.parse_line("not json").is_none());
        assert!(parser.parse_line("").is_none());
    }

    #[test]
    fn decoder_requirements() {
        let decoder = RdsDecoder::new(RDS_DECODER_SAMPLE_RATE_HZ);
        let req = decoder.requirements();
        assert!(req.wants_iq);
        assert!((req.sample_rate - RDS_DECODER_SAMPLE_RATE_HZ).abs() < 1.0);
    }

    #[test]
    fn decoder_name() {
        let decoder = RdsDecoder::new(RDS_DECODER_SAMPLE_RATE_HZ);
        assert_eq!(decoder.name(), "rds");
    }
}
