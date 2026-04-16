//! ADS-B decoder via an external `dump1090`-compatible tool.
//!
//! Pipes cu8 IQ samples to a `dump1090`-compatible backend (`dump1090`,
//! `dump1090-fa`, or `readsb`) for Mode S / ADS-B decoding.
//!
//! ## Requirements
//!
//! A `dump1090`-compatible backend must be installed.

use std::collections::BTreeMap;
use std::time::Instant;

use crate::dsp::decoder::{DecoderPlugin, DecoderRequirements};
use crate::session::DecodedMessage;
use crate::types::Sample;

use super::subprocess::{
    InputFormat, OutputParser, SubprocessBridge, SubprocessConfig, samples_to_cu8,
};
use super::tools;

pub const ADSB_SAMPLE_RATE_HZ: f64 = 2_400_000.0;
pub const ADSB_CHANNEL_BANDWIDTH_HZ: f64 = 2_000_000.0;

pub struct AdsbDecoder {
    sample_rate: f64,
    bridge: SubprocessBridge,
    config: Option<SubprocessConfig>,
}

impl Default for AdsbDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl AdsbDecoder {
    pub fn new() -> Self {
        let sample_rate = ADSB_SAMPLE_RATE_HZ;
        let config = Self::make_config();
        Self {
            sample_rate,
            bridge: SubprocessBridge::new(),
            config: Some(config),
        }
    }

    fn make_config() -> SubprocessConfig {
        SubprocessConfig {
            command: tools::resolve_tool_command("dump1090")
                .unwrap_or("dump1090")
                .to_string(),
            args: vec![
                "--ifile".to_string(),
                "-".to_string(),
                "--iformat".to_string(),
                "UC8".to_string(),
                "--raw".to_string(),
            ],
            input_format: InputFormat::Cu8Iq,
            output_parser: Box::new(AdsbParser),
            thread_name: "dump1090-reader".to_string(),
        }
    }
}

impl DecoderPlugin for AdsbDecoder {
    fn name(&self) -> &str {
        "adsb"
    }

    fn requirements(&self) -> DecoderRequirements {
        DecoderRequirements {
            center_frequency: 1_090_000_000.0,
            sample_rate: self.sample_rate,
            bandwidth: ADSB_CHANNEL_BANDWIDTH_HZ,
            wants_iq: true,
        }
    }

    fn process(&mut self, samples: &[Sample]) -> Vec<DecodedMessage> {
        if !tools::is_tool_available("dump1090") {
            if !self.bridge.init_attempted {
                self.bridge.init_attempted = true;
                return vec![DecodedMessage {
                    decoder: "adsb".to_string(),
                    timestamp: Instant::now(),
                    summary: format!(
                        "No dump1090-compatible backend found. Install with: {}",
                        tools::install_hint("dump1090")
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
                    decoder: "adsb".to_string(),
                    timestamp: Instant::now(),
                    summary: err,
                    fields: BTreeMap::new(),
                    raw_bits: None,
                }];
            }
        } else if !self.bridge.is_running() {
            return vec![];
        }

        let cu8 = samples_to_cu8(samples);

        if !self.bridge.write_stdin(&cu8) {
            let summary = self
                .bridge
                .take_recent_stderr()
                .map(|stderr| format!("dump1090 process terminated unexpectedly: {stderr}"))
                .unwrap_or_else(|| "dump1090 process terminated unexpectedly".to_string());
            return vec![DecodedMessage {
                decoder: "adsb".to_string(),
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
        self.config = Some(Self::make_config());
    }
}

// ============================================================================
// dump1090 raw output parser
// ============================================================================

struct AdsbParser;

impl OutputParser for AdsbParser {
    fn parse_line(&mut self, line: &str) -> Option<DecodedMessage> {
        // dump1090 --raw outputs Mode S messages as hex:
        // *8D4840D6202CC371C32CE0576098;
        // Lines starting with * are Mode S messages, ending with ;
        let trimmed = line.trim();
        if !trimmed.starts_with('*') || !trimmed.ends_with(';') {
            return None;
        }

        let hex = &trimmed[1..trimmed.len() - 1];
        if hex.len() < 14 {
            return None; // Too short for a valid Mode S message
        }

        let mut fields = BTreeMap::new();
        fields.insert("hex".to_string(), hex.to_string());

        // Parse downlink format (first 5 bits of first byte)
        if let Ok(first_byte) = u8::from_str_radix(&hex[0..2], 16) {
            let df = first_byte >> 3;
            fields.insert("df".to_string(), df.to_string());

            // Parse ICAO address (bytes 1-3 for DF17 extended squitter)
            if df == 17 && hex.len() >= 14 {
                let icao = &hex[2..8];
                fields.insert("icao".to_string(), icao.to_uppercase());

                // Type code (first 5 bits of ME field, byte 4)
                if let Ok(me_byte) = u8::from_str_radix(&hex[8..10], 16) {
                    let tc = me_byte >> 3;
                    fields.insert("typecode".to_string(), tc.to_string());

                    let tc_desc = match tc {
                        1..=4 => "Aircraft ID",
                        5..=8 => "Surface Position",
                        9..=18 => "Airborne Position (Baro)",
                        19 => "Airborne Velocity",
                        20..=22 => "Airborne Position (GNSS)",
                        23 => "Test Message",
                        28 => "Aircraft Status",
                        29 => "Target State",
                        31 => "Operational Status",
                        _ => "Unknown",
                    };
                    fields.insert("type_desc".to_string(), tc_desc.to_string());
                }

                let summary = format!(
                    "ADS-B DF{df} ICAO:{icao} {}",
                    fields.get("type_desc").map_or("", |s| s.as_str())
                );
                return Some(DecodedMessage {
                    decoder: "adsb".to_string(),
                    timestamp: Instant::now(),
                    summary,
                    fields,
                    raw_bits: Some(hex.as_bytes().to_vec()),
                });
            }

            let summary = format!("Mode-S DF{df} [{hex}]");
            return Some(DecodedMessage {
                decoder: "adsb".to_string(),
                timestamp: Instant::now(),
                summary,
                fields,
                raw_bits: Some(hex.as_bytes().to_vec()),
            });
        }

        Some(DecodedMessage {
            decoder: "adsb".to_string(),
            timestamp: Instant::now(),
            summary: format!("Mode-S [{hex}]"),
            fields,
            raw_bits: Some(hex.as_bytes().to_vec()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_adsb_df17() {
        let mut parser = AdsbParser;
        let line = "*8D4840D6202CC371C32CE0576098;";
        let msg = parser.parse_line(line).unwrap();
        assert_eq!(msg.decoder, "adsb");
        assert!(msg.summary.contains("DF17"));
        assert!(msg.summary.contains("ICAO:4840D6"));
        assert_eq!(msg.fields["icao"], "4840D6");
        assert_eq!(msg.fields["df"], "17");
    }

    #[test]
    fn parse_mode_s_short() {
        let mut parser = AdsbParser;
        let line = "*5D4840D699C2F8;";
        let msg = parser.parse_line(line).unwrap();
        assert!(msg.summary.contains("Mode-S"));
    }

    #[test]
    fn parse_skip_non_mode_s() {
        let mut parser = AdsbParser;
        assert!(parser.parse_line("some other output").is_none());
        assert!(parser.parse_line("").is_none());
        assert!(parser.parse_line("*;").is_none()); // too short
    }

    #[test]
    fn decoder_name_and_requirements() {
        let decoder = AdsbDecoder::new();
        assert_eq!(decoder.name(), "adsb");
        let req = decoder.requirements();
        assert!(req.wants_iq);
        assert!((req.center_frequency - 1.09e9).abs() < 1.0);
        assert!((req.sample_rate - ADSB_SAMPLE_RATE_HZ).abs() < 1.0);
    }
}
