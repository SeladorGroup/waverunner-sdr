//! Three-stage signal identification.
//!
//! 1. **Instant**: FrequencyDb lookup → band name + expected modulation
//! 2. **Instant**: RuleClassifier → confidence-scored classification
//! 3. **Slow**: Decoder trial — briefly enable candidate decoders, count messages
//!
//! Stages 1 and 2 run instantly on any frequency. Stage 3 requires a running
//! SessionManager and takes several seconds.

use serde::Serialize;

use crate::frequency_db::FrequencyDb;
use crate::mode::classifier::{RuleClassifier, SignalClass};

/// Result of signal identification.
#[derive(Debug, Clone, Serialize)]
pub struct IdentifyResult {
    /// Frequency in Hz.
    pub frequency_hz: f64,
    /// Band name from frequency database, if any.
    pub band_name: Option<String>,
    /// Band modulation from database.
    pub modulation_estimate: Option<String>,
    /// Classifier match result.
    pub classifier_match: Option<ClassifierMatch>,
    /// Overall confidence 0.0–1.0.
    pub confidence: f32,
    /// Recommended demod mode.
    pub recommended_mode: Option<String>,
    /// Recommended decoder.
    pub recommended_decoder: Option<String>,
    /// Decoder trial results, if stage 3 was run.
    pub decoder_trials: Vec<DecoderTrialResult>,
}

/// Classifier match information.
#[derive(Debug, Clone, Serialize)]
pub struct ClassifierMatch {
    pub name: String,
    pub confidence: f32,
    pub modulation: Option<String>,
    pub decoder: Option<String>,
}

/// Result of running a decoder briefly against a signal.
#[derive(Debug, Clone, Serialize)]
pub struct DecoderTrialResult {
    pub decoder: String,
    pub messages_decoded: usize,
    pub trial_duration_ms: u64,
}

/// Run stages 1 and 2 of signal identification (instant, no hardware needed).
pub fn identify_instant(freq_hz: f64, db: &FrequencyDb) -> IdentifyResult {
    // Stage 1: FrequencyDb lookup
    let band = db.lookup(freq_hz);
    let band_name = band.map(|b| b.label.to_string());
    let modulation_estimate = band.map(|b| b.modulation.to_string());
    let recommended_decoder = band.and_then(|b| b.decoder.map(|d| d.to_string()));
    let recommended_mode = db.demod_mode(freq_hz).map(|s| s.to_string());

    // Stage 2: RuleClassifier
    let classifier = RuleClassifier::new();
    let classification = classifier.classify(freq_hz, 0.0, 0.0);

    let classifier_match = match &classification {
        SignalClass::KnownProtocol {
            name,
            decoder,
            confidence,
        } => Some(ClassifierMatch {
            name: name.clone(),
            confidence: *confidence,
            modulation: None,
            decoder: Some(decoder.clone()),
        }),
        SignalClass::Recognized {
            name,
            modulation,
            confidence,
        } => Some(ClassifierMatch {
            name: name.clone(),
            confidence: *confidence,
            modulation: Some(modulation.clone()),
            decoder: None,
        }),
        SignalClass::Unknown => None,
    };

    // Compute overall confidence
    let confidence = match (&band_name, &classifier_match) {
        (Some(_), Some(cm)) => (0.5 + cm.confidence) / 2.0, // Both agree
        (Some(_), None) => 0.4,                             // DB hit only
        (None, Some(cm)) => cm.confidence * 0.8,            // Classifier only
        (None, None) => 0.0,                                // Nothing
    };

    // Prefer classifier decoder over DB decoder
    let best_decoder = classifier_match
        .as_ref()
        .and_then(|cm| cm.decoder.clone())
        .or(recommended_decoder);

    IdentifyResult {
        frequency_hz: freq_hz,
        band_name,
        modulation_estimate,
        classifier_match,
        confidence,
        recommended_mode,
        recommended_decoder: best_decoder,
        decoder_trials: Vec::new(),
    }
}

/// Update an IdentifyResult with decoder trial results from stage 3.
pub fn add_trial_results(result: &mut IdentifyResult, trials: Vec<DecoderTrialResult>) {
    // Boost confidence if any decoder decoded messages
    let best_trial = trials.iter().max_by_key(|t| t.messages_decoded);
    if let Some(trial) = best_trial {
        if trial.messages_decoded > 0 {
            result.confidence = (result.confidence + 0.3).min(1.0);
            result.recommended_decoder = Some(trial.decoder.clone());
        }
    }
    result.decoder_trials = trials;
}

/// Format an IdentifyResult for human-readable display.
pub fn format_result(result: &IdentifyResult) -> String {
    let mut lines = Vec::new();

    let freq_mhz = result.frequency_hz / 1e6;
    lines.push(format!("Frequency: {freq_mhz:.6} MHz"));
    lines.push(format!("Confidence: {:.0}%", result.confidence * 100.0));

    if let Some(ref band) = result.band_name {
        lines.push(format!("Band: {band}"));
    }

    if let Some(ref modulation) = result.modulation_estimate {
        lines.push(format!(
            "Expected modulation: {}",
            modulation.to_uppercase()
        ));
    }

    if let Some(ref cm) = result.classifier_match {
        lines.push(format!(
            "Classifier: {} ({:.0}%)",
            cm.name,
            cm.confidence * 100.0,
        ));
    }

    if let Some(ref mode) = result.recommended_mode {
        lines.push(format!("Recommended mode: {}", mode.to_uppercase()));
    }

    if let Some(ref decoder) = result.recommended_decoder {
        lines.push(format!("Recommended decoder: {decoder}"));
    }

    if !result.decoder_trials.is_empty() {
        lines.push("Decoder trials:".to_string());
        for trial in &result.decoder_trials {
            lines.push(format!(
                "  {}: {} messages in {}ms",
                trial.decoder, trial.messages_decoded, trial.trial_duration_ms,
            ));
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frequency_db::Region;

    #[test]
    fn identify_fm_broadcast() {
        let db = FrequencyDb::new(Region::NA);
        let result = identify_instant(98_300_000.0, &db);

        assert!(result.band_name.is_some());
        assert!(result.band_name.as_ref().unwrap().contains("FM"));
        assert_eq!(result.recommended_mode.as_deref(), Some("wfm"));
        assert!(result.confidence > 0.3);
    }

    #[test]
    fn identify_adsb() {
        let db = FrequencyDb::new(Region::NA);
        let result = identify_instant(1_090_000_000.0, &db);

        assert!(result.classifier_match.is_some());
        let cm = result.classifier_match.as_ref().unwrap();
        assert_eq!(cm.name, "ADS-B");
        assert_eq!(result.recommended_decoder.as_deref(), Some("adsb"));
    }

    #[test]
    fn identify_unknown_freq() {
        let db = FrequencyDb::new(Region::NA);
        // Use a very low frequency outside all defined bands
        let result = identify_instant(50_000.0, &db);

        assert_eq!(result.confidence, 0.0);
        assert!(result.band_name.is_none());
        assert!(result.classifier_match.is_none());
    }

    #[test]
    fn trial_results_boost_confidence() {
        let db = FrequencyDb::new(Region::NA);
        let mut result = identify_instant(98_300_000.0, &db);
        let initial_confidence = result.confidence;

        add_trial_results(
            &mut result,
            vec![DecoderTrialResult {
                decoder: "rds".to_string(),
                messages_decoded: 5,
                trial_duration_ms: 3000,
            }],
        );

        assert!(result.confidence > initial_confidence);
        assert_eq!(result.recommended_decoder.as_deref(), Some("rds"));
    }

    #[test]
    fn format_result_readable() {
        let db = FrequencyDb::new(Region::NA);
        let result = identify_instant(98_300_000.0, &db);
        let formatted = format_result(&result);

        assert!(formatted.contains("98.300000 MHz"));
        assert!(formatted.contains("Confidence"));
    }
}
