//! Additional protocol decoders via `multimon-ng`.
//!
//! Covers DTMF, EAS (Emergency Alert System), and FLEX pager protocols.
//! These are external-tool-only decoders with no built-in fallback.
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

/// Which multimon-ng protocol to decode.
#[derive(Debug, Clone, Copy)]
pub enum MultimonProtocol {
    Dtmf,
    Eas,
    Flex,
}

impl MultimonProtocol {
    fn multimon_flag(&self) -> &str {
        match self {
            Self::Dtmf => "DTMF",
            Self::Eas => "EAS",
            Self::Flex => "FLEX",
        }
    }

    fn decoder_name(&self) -> &str {
        match self {
            Self::Dtmf => "dtmf",
            Self::Eas => "eas",
            Self::Flex => "flex",
        }
    }
}

pub struct MultimonDecoder {
    protocol: MultimonProtocol,
    sample_rate: f64,
    bridge: SubprocessBridge,
    config: Option<SubprocessConfig>,
    prev_sample: Sample,
}

impl MultimonDecoder {
    pub fn new(protocol: MultimonProtocol) -> Self {
        let sample_rate = 22050.0;
        let config = Self::make_config(protocol, sample_rate);
        Self {
            protocol,
            sample_rate,
            bridge: SubprocessBridge::new(),
            config: Some(config),
            prev_sample: Sample::new(0.0, 0.0),
        }
    }

    fn make_config(protocol: MultimonProtocol, _sample_rate: f64) -> SubprocessConfig {
        SubprocessConfig {
            command: tools::resolve_tool_command("multimon-ng")
                .unwrap_or("multimon-ng")
                .to_string(),
            args: vec![
                "-t".to_string(),
                "raw".to_string(),
                "-q".to_string(),
                "-a".to_string(),
                protocol.multimon_flag().to_string(),
                "-".to_string(),
            ],
            input_format: InputFormat::S16LeAudio,
            output_parser: Box::new(MultimonParser { protocol }),
            thread_name: format!("multimon-{}", protocol.decoder_name()),
        }
    }
}

impl DecoderPlugin for MultimonDecoder {
    fn name(&self) -> &str {
        self.protocol.decoder_name()
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
        let decoder_name = self.protocol.decoder_name().to_string();

        if !tools::is_tool_available("multimon-ng") {
            if !self.bridge.init_attempted {
                self.bridge.init_attempted = true;
                return vec![DecodedMessage {
                    decoder: decoder_name,
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
                    decoder: decoder_name,
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
                decoder: decoder_name,
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
        self.config = Some(Self::make_config(self.protocol, self.sample_rate));
    }
}

// ============================================================================
// Generic multimon-ng output parser
// ============================================================================

struct MultimonParser {
    protocol: MultimonProtocol,
}

impl OutputParser for MultimonParser {
    fn parse_line(&mut self, line: &str) -> Option<DecodedMessage> {
        let decoder_name = self.protocol.decoder_name();

        match self.protocol {
            MultimonProtocol::Dtmf => {
                // DTMF: X
                if !line.starts_with("DTMF:") {
                    return None;
                }
                let digit = line["DTMF:".len()..].trim();
                let mut fields = BTreeMap::new();
                fields.insert("digit".to_string(), digit.to_string());
                Some(DecodedMessage {
                    decoder: decoder_name.to_string(),
                    timestamp: Instant::now(),
                    summary: format!("DTMF: {digit}"),
                    fields,
                    raw_bits: None,
                })
            }
            MultimonProtocol::Eas => {
                // EAS: ZCZC-ORG-EEE-PSSCCC+TTTT-JJJHHMM-LLLLLLLL-
                if !line.starts_with("EAS:") {
                    return None;
                }
                let msg = line["EAS:".len()..].trim();
                let mut fields = BTreeMap::new();
                fields.insert("message".to_string(), msg.to_string());
                Some(DecodedMessage {
                    decoder: decoder_name.to_string(),
                    timestamp: Instant::now(),
                    summary: format!("EAS: {msg}"),
                    fields,
                    raw_bits: None,
                })
            }
            MultimonProtocol::Flex => {
                // FLEX output varies, just capture the line
                if !line.starts_with("FLEX") {
                    return None;
                }
                let mut fields = BTreeMap::new();
                fields.insert("raw".to_string(), line.to_string());
                Some(DecodedMessage {
                    decoder: decoder_name.to_string(),
                    timestamp: Instant::now(),
                    summary: line.to_string(),
                    fields,
                    raw_bits: None,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dtmf() {
        let mut parser = MultimonParser {
            protocol: MultimonProtocol::Dtmf,
        };
        let msg = parser.parse_line("DTMF: 5").unwrap();
        assert_eq!(msg.decoder, "dtmf");
        assert!(msg.summary.contains("5"));
    }

    #[test]
    fn parse_eas() {
        let mut parser = MultimonParser {
            protocol: MultimonProtocol::Eas,
        };
        let msg = parser
            .parse_line("EAS: ZCZC-WXR-TOR-029165+0030-1261848-NWS/DLIL-")
            .unwrap();
        assert_eq!(msg.decoder, "eas");
        assert!(msg.summary.contains("ZCZC"));
    }

    #[test]
    fn parse_flex() {
        let mut parser = MultimonParser {
            protocol: MultimonProtocol::Flex,
        };
        let msg = parser
            .parse_line("FLEX: 2024-01-01 12:00:00 1600/2 [1234567] ALN Test message")
            .unwrap();
        assert_eq!(msg.decoder, "flex");
    }

    #[test]
    fn parse_skip_wrong_protocol() {
        let mut parser = MultimonParser {
            protocol: MultimonProtocol::Dtmf,
        };
        assert!(parser.parse_line("POCSAG1200: something").is_none());
        assert!(parser.parse_line("").is_none());
    }

    #[test]
    fn decoder_names() {
        assert_eq!(MultimonDecoder::new(MultimonProtocol::Dtmf).name(), "dtmf");
        assert_eq!(MultimonDecoder::new(MultimonProtocol::Eas).name(), "eas");
        assert_eq!(MultimonDecoder::new(MultimonProtocol::Flex).name(), "flex");
    }
}
