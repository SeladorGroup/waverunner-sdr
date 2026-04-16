//! POCSAG pager decoder via external `multimon-ng` tool.
//!
//! Pipes FM-demodulated audio to `multimon-ng` stdin as raw S16LE PCM.
//! Parses text output for pager addresses and messages.
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

/// Baud rate variants for POCSAG.
#[derive(Debug, Clone, Copy)]
pub enum PocsagBaudRate {
    Rate512,
    Rate1200,
    Rate2400,
}

impl PocsagBaudRate {
    fn multimon_args(&self) -> Vec<String> {
        // When a specific baud rate is selected, enable that one.
        // "pocsag" (no baud suffix) enables all three.
        match self {
            Self::Rate512 => vec!["-a".to_string(), "POCSAG512".to_string()],
            Self::Rate1200 => vec!["-a".to_string(), "POCSAG1200".to_string()],
            Self::Rate2400 => vec!["-a".to_string(), "POCSAG2400".to_string()],
        }
    }

    fn default_decoder_name(&self) -> &'static str {
        match self {
            Self::Rate512 => "pocsag-512",
            Self::Rate1200 => "pocsag",
            Self::Rate2400 => "pocsag-2400",
        }
    }
}

pub struct PocsagDecoder {
    decoder_name: String,
    baud_rate: PocsagBaudRate,
    sample_rate: f64,
    bridge: SubprocessBridge,
    config: Option<SubprocessConfig>,
    prev_sample: Sample,
}

impl PocsagDecoder {
    pub fn new(baud_rate: PocsagBaudRate) -> Self {
        Self::named(baud_rate, baud_rate.default_decoder_name())
    }

    pub fn named(baud_rate: PocsagBaudRate, decoder_name: impl Into<String>) -> Self {
        let sample_rate = 22050.0;
        let decoder_name = decoder_name.into();
        let config = Self::make_config(baud_rate, sample_rate, &decoder_name);
        Self {
            decoder_name,
            baud_rate,
            sample_rate,
            bridge: SubprocessBridge::new(),
            config: Some(config),
            prev_sample: Sample::new(0.0, 0.0),
        }
    }

    fn make_config(
        baud_rate: PocsagBaudRate,
        sample_rate: f64,
        decoder_name: &str,
    ) -> SubprocessConfig {
        let mut args = vec![
            "-t".to_string(),
            "raw".to_string(),
            "-q".to_string(), // quiet (no monitor line)
        ];
        args.extend(baud_rate.multimon_args());
        args.push("-".to_string()); // read from stdin

        SubprocessConfig {
            command: tools::resolve_tool_command("multimon-ng")
                .unwrap_or("multimon-ng")
                .to_string(),
            args,
            input_format: InputFormat::S16LeAudio,
            output_parser: Box::new(PocsagParser {
                sample_rate,
                decoder_name: decoder_name.to_string(),
            }),
            thread_name: "multimon-pocsag".to_string(),
        }
    }
}

impl DecoderPlugin for PocsagDecoder {
    fn name(&self) -> &str {
        &self.decoder_name
    }

    fn requirements(&self) -> DecoderRequirements {
        DecoderRequirements {
            center_frequency: 0.0,
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
                    decoder: "pocsag".to_string(),
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
                    decoder: "pocsag".to_string(),
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
                decoder: "pocsag".to_string(),
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
        self.config = Some(Self::make_config(
            self.baud_rate,
            self.sample_rate,
            &self.decoder_name,
        ));
    }
}

// ============================================================================
// multimon-ng POCSAG output parser
// ============================================================================

struct PocsagParser {
    #[allow(dead_code)]
    sample_rate: f64,
    decoder_name: String,
}

impl OutputParser for PocsagParser {
    fn parse_line(&mut self, line: &str) -> Option<DecodedMessage> {
        // multimon-ng POCSAG output format:
        // POCSAG1200: Address: 1234567  Function: 0  Alpha:   Hello World
        // POCSAG1200: Address: 1234567  Function: 2  Numeric: 5551234
        if !line.starts_with("POCSAG") {
            return None;
        }

        let mut fields = BTreeMap::new();
        let mut parts = Vec::new();

        // Extract baud rate
        if let Some(colon_pos) = line.find(':') {
            let baud = &line[..colon_pos];
            fields.insert("baud".to_string(), baud.to_string());
            parts.push(baud.to_string());
        }

        // Extract address
        if let Some(addr_start) = line.find("Address:") {
            let rest = &line[addr_start + 8..].trim_start();
            if let Some(addr_end) = rest.find(|c: char| !c.is_ascii_digit()) {
                let addr = &rest[..addr_end].trim();
                if !addr.is_empty() {
                    fields.insert("address".to_string(), addr.to_string());
                    parts.push(format!("Addr:{addr}"));
                }
            }
        }

        // Extract function code
        if let Some(func_start) = line.find("Function:") {
            let rest = &line[func_start + 9..].trim_start();
            if let Some(func) = rest.chars().next() {
                if func.is_ascii_digit() {
                    fields.insert("function".to_string(), func.to_string());
                }
            }
        }

        // Extract message content (Alpha or Numeric)
        if let Some(alpha_start) = line.find("Alpha:") {
            let msg = line[alpha_start + 6..].trim();
            if !msg.is_empty() {
                fields.insert("message".to_string(), msg.to_string());
                fields.insert("type".to_string(), "alpha".to_string());
                parts.push(msg.to_string());
            }
        } else if let Some(num_start) = line.find("Numeric:") {
            let msg = line[num_start + 8..].trim();
            if !msg.is_empty() {
                fields.insert("message".to_string(), msg.to_string());
                fields.insert("type".to_string(), "numeric".to_string());
                parts.push(format!("#{msg}"));
            }
        }

        if parts.is_empty() {
            return None;
        }

        Some(DecodedMessage {
            decoder: self.decoder_name.clone(),
            timestamp: Instant::now(),
            summary: parts.join(" | "),
            fields,
            raw_bits: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pocsag_alpha() {
        let mut parser = PocsagParser {
            sample_rate: 22050.0,
            decoder_name: "pocsag".to_string(),
        };
        let line = "POCSAG1200: Address: 1234567  Function: 0  Alpha:   Hello World";
        let msg = parser.parse_line(line).unwrap();
        assert_eq!(msg.decoder, "pocsag");
        assert!(msg.summary.contains("POCSAG1200"));
        assert!(msg.summary.contains("Addr:1234567"));
        assert!(msg.summary.contains("Hello World"));
        assert_eq!(msg.fields["address"], "1234567");
        assert_eq!(msg.fields["type"], "alpha");
    }

    #[test]
    fn parse_pocsag_numeric() {
        let mut parser = PocsagParser {
            sample_rate: 22050.0,
            decoder_name: "pocsag".to_string(),
        };
        let line = "POCSAG2400: Address: 9876543  Function: 2  Numeric: 5551234";
        let msg = parser.parse_line(line).unwrap();
        assert!(msg.summary.contains("#5551234"));
        assert_eq!(msg.fields["type"], "numeric");
    }

    #[test]
    fn parse_pocsag_skip_non_pocsag() {
        let mut parser = PocsagParser {
            sample_rate: 22050.0,
            decoder_name: "pocsag".to_string(),
        };
        assert!(parser.parse_line("multimon-ng starting").is_none());
        assert!(parser.parse_line("").is_none());
    }

    #[test]
    fn decoder_name_and_requirements() {
        let decoder = PocsagDecoder::new(PocsagBaudRate::Rate1200);
        assert_eq!(decoder.name(), "pocsag");
        let req = decoder.requirements();
        assert!(req.wants_iq);
        assert!((req.sample_rate - 22050.0).abs() < 1.0);
    }

    #[test]
    fn named_decoder_preserves_variant_name() {
        let decoder = PocsagDecoder::named(PocsagBaudRate::Rate1200, "pocsag-1200");
        assert_eq!(decoder.name(), "pocsag-1200");
    }
}
