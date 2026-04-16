//! Three-stage signal identification.
//!
//! 1. **Instant**: FrequencyDb lookup → band name + expected modulation
//! 2. **Instant**: RuleClassifier → confidence-scored classification
//! 3. **Slow**: Decoder trial — briefly enable candidate decoders, count messages
//!
//! Stages 1 and 2 run instantly on any frequency. Stage 3 requires a running
//! SessionManager and takes several seconds.

use serde::Serialize;

use crate::analysis::{
    burst::BurstReport,
    measurement::MeasurementReport,
    modulation::{ModulationReport, ModulationType},
};
use crate::frequency_db::{FrequencyDb, KNOWN_FREQUENCY_TOLERANCE_HZ};
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
    /// Optional investigation details from a short capture and replay analysis.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub investigation: Option<SignalInvestigation>,
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

/// Optional deeper investigation produced from a short live capture.
#[derive(Debug, Clone, Serialize)]
pub struct SignalInvestigation {
    pub capture_path: String,
    pub capture_duration_secs: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurement: Option<MeasurementReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub burst: Option<BurstReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modulation: Option<ModulationReport>,
}

/// Run stages 1 and 2 of signal identification (instant, no hardware needed).
pub fn identify_instant(freq_hz: f64, db: &FrequencyDb) -> IdentifyResult {
    // Stage 1: FrequencyDb lookup (prefer known exact-ish channels over broad bands)
    let known_hit = db
        .lookup_known_frequency(freq_hz, KNOWN_FREQUENCY_TOLERANCE_HZ)
        .is_some();
    let band_name = db.band_name(freq_hz).map(|label| label.to_string());
    let modulation_estimate = db.modulation(freq_hz).map(|mode| mode.to_string());
    let recommended_decoder = db.decoder(freq_hz).map(|decoder| decoder.to_string());
    let recommended_mode = db.demod_mode(freq_hz).map(|mode| mode.to_string());

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
    let confidence = match (known_hit, &band_name, &classifier_match) {
        (true, Some(_), Some(cm)) => (0.65 + cm.confidence) / 2.0,
        (true, Some(_), None) => 0.65,
        (false, Some(_), Some(cm)) => (0.5 + cm.confidence) / 2.0,
        (false, Some(_), None) => 0.4,
        (false, None, Some(cm)) => cm.confidence * 0.8,
        (false, None, None) => 0.0,
        (true, None, Some(cm)) => (0.55 + cm.confidence) / 2.0,
        (true, None, None) => 0.55,
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
        investigation: None,
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

/// Attach investigation results captured from a short recording.
pub fn add_investigation(result: &mut IdentifyResult, investigation: SignalInvestigation) {
    let mut investigation = investigation;
    if let Some(mode) = result.recommended_mode.as_deref() {
        let should_suppress = investigation
            .modulation
            .as_ref()
            .is_some_and(|report| result.confidence >= 0.5 && !mode_matches_report(mode, report));
        if should_suppress {
            investigation.modulation = None;
        }
    }

    if investigation.modulation.is_some() || investigation.measurement.is_some() {
        result.confidence = (result.confidence + 0.15).min(1.0);
    }
    result.investigation = Some(investigation);
}

fn mode_matches_report(mode: &str, report: &ModulationReport) -> bool {
    matches!(
        (mode, &report.modulation_type),
        ("am", ModulationType::AM)
            | ("fm", ModulationType::FM)
            | ("wfm", ModulationType::FM)
            | ("wfm-stereo", ModulationType::FM)
            | ("cw", ModulationType::CW)
    )
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

    if let Some(ref investigation) = result.investigation {
        lines.push(format!(
            "Capture: {} ({:.1}s)",
            investigation.capture_path, investigation.capture_duration_secs
        ));
        if let Some(ref modulation) = investigation.modulation {
            lines.push(format!(
                "Modulation: {} ({:.0}% confidence)",
                modulation.modulation_type,
                modulation.confidence * 100.0,
            ));
        }
        if let Some(ref measurement) = investigation.measurement {
            lines.push(format!(
                "Occupied bandwidth: {:.1} kHz",
                measurement.occupied_bw_hz / 1e3
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
        assert_eq!(result.modulation_estimate.as_deref(), Some("ppm"));
        assert!(result.recommended_mode.is_none());
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
    fn identify_known_frequency_prefers_specific_channel_label() {
        let db = FrequencyDb::new(Region::NA);
        let result = identify_instant(162_550_000.0, &db);

        assert_eq!(result.band_name.as_deref(), Some("NOAA Weather 7"));
        assert_eq!(result.recommended_mode.as_deref(), Some("fm"));
        assert!(result.confidence >= 0.65);
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
    fn investigation_boosts_confidence() {
        let db = FrequencyDb::new(Region::NA);
        let mut result = identify_instant(98_300_000.0, &db);
        let initial_confidence = result.confidence;
        add_investigation(
            &mut result,
            SignalInvestigation {
                capture_path: "/tmp/test.cf32".to_string(),
                capture_duration_secs: 5.0,
                measurement: None,
                burst: None,
                modulation: Some(ModulationReport {
                    modulation_type: crate::analysis::modulation::ModulationType::FM,
                    confidence: 0.9,
                    symbol_rate_hz: None,
                    am_depth: None,
                    fm_deviation_hz: Some(75_000.0),
                    amplitude_levels: None,
                    phase_states: None,
                }),
            },
        );
        assert!(result.confidence > initial_confidence);
        assert!(result.investigation.is_some());
    }

    #[test]
    fn conflicting_investigation_modulation_is_suppressed_for_strong_known_signal() {
        let db = FrequencyDb::new(Region::NA);
        let mut result = identify_instant(98_300_000.0, &db);

        add_investigation(
            &mut result,
            SignalInvestigation {
                capture_path: "/tmp/test.cf32".to_string(),
                capture_duration_secs: 5.0,
                measurement: Some(MeasurementReport {
                    bandwidth_3db_hz: 180_000.0,
                    bandwidth_6db_hz: 220_000.0,
                    occupied_bw_hz: 200_000.0,
                    obw_percent: 99.0,
                    channel_power_dbfs: -20.0,
                    acpr_lower_dbc: -30.0,
                    acpr_upper_dbc: -30.0,
                    papr_db: 6.0,
                    freq_offset_hz: 0.0,
                }),
                burst: None,
                modulation: Some(ModulationReport {
                    modulation_type: ModulationType::AM,
                    confidence: 0.6,
                    symbol_rate_hz: None,
                    am_depth: Some(0.8),
                    fm_deviation_hz: None,
                    amplitude_levels: None,
                    phase_states: None,
                }),
            },
        );

        let investigation = result
            .investigation
            .expect("investigation should be attached");
        assert!(investigation.modulation.is_none());
        assert!(investigation.measurement.is_some());
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
