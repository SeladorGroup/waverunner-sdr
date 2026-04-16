//! APRS decoder via external `multimon-ng` tool.
//!
//! Pipes FM-demodulated audio to `multimon-ng` stdin as raw S16LE PCM
//! with AFSK1200 decoding enabled. Parses AX.25/APRS packet output.
//!
//! ## Requirements
//!
//! `multimon-ng` must be installed: `sudo pacman -S multimon-ng`

use std::collections::BTreeMap;
use std::time::Instant;

use crate::dsp::decoder::{DecoderPlugin, DecoderRequirements};
use crate::session::DecodedMessage;
use crate::types::Sample;

use super::subprocess::{
    InputFormat, OutputParser, SubprocessBridge, SubprocessConfig, audio_to_s16le, fm_demod,
};
use super::tools;

pub struct AprsDecoder {
    sample_rate: f64,
    bridge: SubprocessBridge,
    config: Option<SubprocessConfig>,
    prev_sample: Sample,
}

impl AprsDecoder {
    pub fn new(sample_rate: f64) -> Self {
        let config = Self::make_config(sample_rate);
        Self {
            sample_rate,
            bridge: SubprocessBridge::new(),
            config: Some(config),
            prev_sample: Sample::new(0.0, 0.0),
        }
    }

    fn make_config(sample_rate: f64) -> SubprocessConfig {
        SubprocessConfig {
            command: tools::resolve_tool_command("multimon-ng")
                .unwrap_or("multimon-ng")
                .to_string(),
            args: vec![
                "-t".to_string(),
                "raw".to_string(),
                "-q".to_string(),
                "-a".to_string(),
                "AFSK1200".to_string(),
                "-".to_string(),
            ],
            input_format: InputFormat::S16LeAudio,
            output_parser: Box::new(AprsParser { sample_rate }),
            thread_name: "multimon-aprs".to_string(),
        }
    }
}

impl DecoderPlugin for AprsDecoder {
    fn name(&self) -> &str {
        "aprs"
    }

    fn requirements(&self) -> DecoderRequirements {
        DecoderRequirements {
            center_frequency: 144.39e6,
            sample_rate: self.sample_rate,
            bandwidth: 25_000.0,
            wants_iq: true,
        }
    }

    fn process(&mut self, samples: &[Sample]) -> Vec<DecodedMessage> {
        if !tools::is_tool_available("multimon-ng") {
            if !self.bridge.init_attempted {
                self.bridge.init_attempted = true;
                return vec![DecodedMessage {
                    decoder: "aprs".to_string(),
                    timestamp: Instant::now(),
                    summary: format!(
                        "multimon-ng not installed. Install with: {}",
                        tools::install_hint("multimon-ng")
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
                    decoder: "aprs".to_string(),
                    timestamp: Instant::now(),
                    summary: err,
                    fields: BTreeMap::new(),
                    raw_bits: None,
                }];
            }
        } else if !self.bridge.is_running() {
            return vec![];
        }

        let audio = fm_demod(samples, &mut self.prev_sample);
        let s16le = audio_to_s16le(&audio);

        if !self.bridge.write_stdin(&s16le) {
            let summary = self
                .bridge
                .take_recent_stderr()
                .map(|stderr| format!("multimon-ng process terminated unexpectedly: {stderr}"))
                .unwrap_or_else(|| "multimon-ng process terminated unexpectedly".to_string());
            return vec![DecodedMessage {
                decoder: "aprs".to_string(),
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
        self.config = Some(Self::make_config(self.sample_rate));
    }
}

// ============================================================================
// multimon-ng AFSK1200/APRS output parser
// ============================================================================

struct AprsParser {
    #[allow(dead_code)]
    sample_rate: f64,
}

impl OutputParser for AprsParser {
    fn parse_line(&mut self, line: &str) -> Option<DecodedMessage> {
        // multimon-ng AFSK1200 output format:
        // AFSK1200: fm CALL-1 to CALL-2 via CALL-3 <UI pid=F0 Len=42 > :message content
        if !line.starts_with("AFSK1200:") {
            return None;
        }

        let payload = line["AFSK1200:".len()..].trim();
        let mut fields = BTreeMap::new();

        // Extract source callsign
        if let Some(rest) = payload.strip_prefix("fm ") {
            if let Some(space_pos) = rest.find(' ') {
                let source = &rest[..space_pos];
                fields.insert("source".to_string(), source.to_string());
            }
        }

        // Extract destination
        if let Some(to_pos) = payload.find(" to ") {
            let rest = &payload[to_pos + 4..];
            if let Some(space_pos) = rest.find(' ') {
                let dest = &rest[..space_pos];
                fields.insert("destination".to_string(), dest.to_string());
            }
        }

        // Extract via path
        if let Some(via_pos) = payload.find(" via ") {
            let rest = &payload[via_pos + 5..];
            if let Some(angle) = rest.find('<') {
                let path = rest[..angle].trim();
                if !path.is_empty() {
                    fields.insert("path".to_string(), path.to_string());
                }
            }
        }

        // Extract info field (after the > :)
        if let Some(info_marker) = payload.find("> :") {
            let info = payload[info_marker + 3..].trim();
            if !info.is_empty() {
                fields.insert("info".to_string(), info.to_string());
            }
        } else if let Some(info_marker) = payload.find(">:") {
            let info = payload[info_marker + 2..].trim();
            if !info.is_empty() {
                fields.insert("info".to_string(), info.to_string());
            }
        }

        // Build summary
        let source = fields.get("source").map_or("?", |s| s.as_str());
        let info = fields.get("info").map_or("", |s| s.as_str());
        let summary = if info.is_empty() {
            format!("APRS {source}")
        } else {
            format!("APRS {source}: {info}")
        };

        fields.insert("raw".to_string(), payload.to_string());

        Some(DecodedMessage {
            decoder: "aprs".to_string(),
            timestamp: Instant::now(),
            summary,
            fields,
            raw_bits: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_aprs_packet() {
        let mut parser = AprsParser {
            sample_rate: 22050.0,
        };
        let line = "AFSK1200: fm W6ABC-1 to APRS via WIDE1-1 <UI pid=F0 Len=42 > :!3740.00N/12200.00W-PHG2360";
        let msg = parser.parse_line(line).unwrap();
        assert_eq!(msg.decoder, "aprs");
        assert!(msg.summary.contains("W6ABC-1"));
        assert_eq!(msg.fields["source"], "W6ABC-1");
        assert_eq!(msg.fields["destination"], "APRS");
    }

    #[test]
    fn parse_aprs_skip_non_afsk() {
        let mut parser = AprsParser {
            sample_rate: 22050.0,
        };
        assert!(parser.parse_line("POCSAG1200: something").is_none());
        assert!(parser.parse_line("").is_none());
    }

    #[test]
    fn decoder_name_and_requirements() {
        let decoder = AprsDecoder::new(22050.0);
        assert_eq!(decoder.name(), "aprs");
        let req = decoder.requirements();
        assert!(req.wants_iq);
        assert!((req.center_frequency - 144.39e6).abs() < 1.0);
    }
}
