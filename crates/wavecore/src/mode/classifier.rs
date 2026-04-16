//! Rule-based signal classifier using frequency/bandwidth lookup.
//!
//! No ML — just a table of known signal fingerprints matched against
//! detected signal parameters.

/// Classification result for a detected signal.
#[derive(Debug, Clone)]
pub enum SignalClass {
    /// Known protocol with an available decoder.
    KnownProtocol {
        name: String,
        decoder: String,
        confidence: f32,
    },
    /// Recognized signal type but no decoder available.
    Recognized {
        name: String,
        modulation: String,
        confidence: f32,
    },
    /// Unrecognized signal.
    Unknown,
}

/// A classification rule matching frequency/bandwidth ranges to signal types.
pub struct ClassificationRule {
    pub name: &'static str,
    pub freq_range: (f64, f64),
    pub bandwidth_range: (f64, f64),
    pub decoder: Option<&'static str>,
    pub modulation: &'static str,
}

/// Rule-based classifier for RF signals.
pub struct RuleClassifier {
    rules: Vec<ClassificationRule>,
}

impl RuleClassifier {
    /// Create a classifier pre-loaded with built-in signal fingerprints.
    pub fn new() -> Self {
        Self {
            rules: vec![
                ClassificationRule {
                    name: "ADS-B",
                    freq_range: (1_089_000_000.0, 1_091_000_000.0),
                    bandwidth_range: (1_000_000.0, 3_000_000.0),
                    decoder: Some("adsb"),
                    modulation: "PPM",
                },
                ClassificationRule {
                    name: "POCSAG",
                    freq_range: (929_000_000.0, 930_000_000.0),
                    bandwidth_range: (10_000.0, 50_000.0),
                    decoder: Some("pocsag"),
                    modulation: "FSK",
                },
                ClassificationRule {
                    name: "FM Broadcast",
                    freq_range: (87_500_000.0, 108_000_000.0),
                    bandwidth_range: (150_000.0, 250_000.0),
                    decoder: Some("rds"),
                    modulation: "WFM",
                },
                ClassificationRule {
                    name: "Aviation Emergency",
                    freq_range: (121_400_000.0, 121_600_000.0),
                    bandwidth_range: (3_000.0, 10_000.0),
                    decoder: None,
                    modulation: "AM",
                },
                ClassificationRule {
                    name: "Marine VHF Ch16",
                    freq_range: (156_700_000.0, 156_900_000.0),
                    bandwidth_range: (10_000.0, 30_000.0),
                    decoder: None,
                    modulation: "NFM",
                },
                ClassificationRule {
                    name: "FRS/GMRS",
                    freq_range: (462_000_000.0, 468_000_000.0),
                    bandwidth_range: (10_000.0, 25_000.0),
                    decoder: None,
                    modulation: "NFM",
                },
                ClassificationRule {
                    name: "ISM 433",
                    freq_range: (433_000_000.0, 434_800_000.0),
                    bandwidth_range: (1_000.0, 500_000.0),
                    decoder: None,
                    modulation: "Various",
                },
                ClassificationRule {
                    name: "ISM 915",
                    freq_range: (902_000_000.0, 928_000_000.0),
                    bandwidth_range: (1_000.0, 500_000.0),
                    decoder: None,
                    modulation: "Various",
                },
            ],
        }
    }

    /// Classify a signal by its center frequency and bandwidth.
    ///
    /// Returns the best matching rule. Frequency must fall within the rule's
    /// range. If bandwidth is known (> 0), it refines the match. SNR affects
    /// confidence scoring.
    pub fn classify(&self, freq_hz: f64, bandwidth_hz: f64, snr_db: f32) -> SignalClass {
        let mut best: Option<(&ClassificationRule, f32)> = None;

        for rule in &self.rules {
            // Frequency must be within range
            if freq_hz < rule.freq_range.0 || freq_hz > rule.freq_range.1 {
                continue;
            }

            let mut confidence: f32 = 0.5;

            // Frequency match quality — center of range is best
            let range_width = rule.freq_range.1 - rule.freq_range.0;
            let center = (rule.freq_range.0 + rule.freq_range.1) / 2.0;
            let freq_dist = (freq_hz - center).abs() / (range_width / 2.0);
            confidence += 0.2 * (1.0 - freq_dist as f32).max(0.0);

            // Bandwidth match if known
            if bandwidth_hz > 0.0
                && bandwidth_hz >= rule.bandwidth_range.0
                && bandwidth_hz <= rule.bandwidth_range.1
            {
                confidence += 0.2;
            }

            // SNR boost for strong signals
            if snr_db > 15.0 {
                confidence += 0.1;
            }

            confidence = confidence.min(1.0);

            let should_replace = best
                .as_ref()
                .map(|(_, best_confidence)| confidence > *best_confidence)
                .unwrap_or(true);
            if should_replace {
                best = Some((rule, confidence));
            }
        }

        match best {
            Some((rule, confidence)) => {
                if let Some(decoder) = rule.decoder {
                    SignalClass::KnownProtocol {
                        name: rule.name.to_string(),
                        decoder: decoder.to_string(),
                        confidence,
                    }
                } else {
                    SignalClass::Recognized {
                        name: rule.name.to_string(),
                        modulation: rule.modulation.to_string(),
                        confidence,
                    }
                }
            }
            None => SignalClass::Unknown,
        }
    }
}

impl Default for RuleClassifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_adsb() {
        let c = RuleClassifier::new();
        match c.classify(1_090_000_000.0, 2_000_000.0, 20.0) {
            SignalClass::KnownProtocol { name, decoder, .. } => {
                assert_eq!(name, "ADS-B");
                assert_eq!(decoder, "adsb");
            }
            other => panic!("Expected KnownProtocol, got {other:?}"),
        }
    }

    #[test]
    fn classify_pocsag() {
        let c = RuleClassifier::new();
        match c.classify(929_612_500.0, 25_000.0, 15.0) {
            SignalClass::KnownProtocol { name, decoder, .. } => {
                assert_eq!(name, "POCSAG");
                assert_eq!(decoder, "pocsag");
            }
            other => panic!("Expected KnownProtocol, got {other:?}"),
        }
    }

    #[test]
    fn classify_fm_broadcast() {
        let c = RuleClassifier::new();
        match c.classify(100_100_000.0, 200_000.0, 30.0) {
            SignalClass::KnownProtocol { name, decoder, .. } => {
                assert_eq!(name, "FM Broadcast");
                assert_eq!(decoder, "rds");
            }
            other => panic!("Expected KnownProtocol, got {other:?}"),
        }
    }

    #[test]
    fn classify_aviation_emergency() {
        let c = RuleClassifier::new();
        match c.classify(121_500_000.0, 5_000.0, 10.0) {
            SignalClass::Recognized {
                name, modulation, ..
            } => {
                assert_eq!(name, "Aviation Emergency");
                assert_eq!(modulation, "AM");
            }
            other => panic!("Expected Recognized, got {other:?}"),
        }
    }

    #[test]
    fn classify_unknown() {
        let c = RuleClassifier::new();
        assert!(matches!(
            c.classify(300_000_000.0, 50_000.0, 10.0),
            SignalClass::Unknown
        ));
    }

    #[test]
    fn no_rule_conflicts() {
        let c = RuleClassifier::new();
        // Each known frequency should classify to exactly one type
        let results = vec![
            c.classify(1_090_000_000.0, 2_000_000.0, 20.0),
            c.classify(929_500_000.0, 20_000.0, 15.0),
            c.classify(100_000_000.0, 200_000.0, 25.0),
        ];
        for r in &results {
            assert!(!matches!(r, SignalClass::Unknown));
        }
    }

    #[test]
    fn frequency_at_boundary() {
        let c = RuleClassifier::new();
        // At exact lower boundary of FM broadcast
        match c.classify(87_500_000.0, 200_000.0, 15.0) {
            SignalClass::KnownProtocol { name, .. } => {
                assert_eq!(name, "FM Broadcast");
            }
            other => panic!("Expected KnownProtocol at boundary, got {other:?}"),
        }
        // At exact upper boundary of FM broadcast
        match c.classify(108_000_000.0, 200_000.0, 15.0) {
            SignalClass::KnownProtocol { name, .. } => {
                assert_eq!(name, "FM Broadcast");
            }
            other => panic!("Expected KnownProtocol at boundary, got {other:?}"),
        }
    }

    #[test]
    fn empty_classifier_returns_unknown() {
        let c = RuleClassifier { rules: Vec::new() };
        assert!(matches!(
            c.classify(100_000_000.0, 200_000.0, 20.0),
            SignalClass::Unknown
        ));
    }
}
