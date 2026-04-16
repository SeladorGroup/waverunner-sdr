//! Session and scan report generation.
//!
//! Structured report types for exporting session summaries, scan results,
//! and analysis data in JSON or CSV format.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::session::timeline::Annotation;

/// Metadata about the session.
#[derive(Debug, Clone, Serialize)]
pub struct SessionMetadata {
    pub start_time: String,
    pub duration_secs: f64,
    pub center_freq: f64,
    pub sample_rate: f64,
    pub gain: String,
    pub fft_size: usize,
}

/// A single detection from a frequency scan.
#[derive(Debug, Clone, Serialize)]
pub struct ScanDetection {
    pub frequency_hz: f64,
    pub power_db: f32,
    pub snr_db: f32,
    pub bandwidth_hz: f64,
}

/// Results from a frequency scan.
#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    pub start_freq: f64,
    pub end_freq: f64,
    pub step_hz: f64,
    pub dwell_ms: u64,
    pub signals_found: u32,
    pub detections: Vec<ScanDetection>,
}

/// A decoded message for report inclusion.
#[derive(Debug, Clone, Serialize)]
pub struct ReportDecodedMessage {
    pub decoder: String,
    pub elapsed_ms: u64,
    pub summary: String,
    pub fields: BTreeMap<String, String>,
}

/// Full session report combining metadata, analysis, and decoded messages.
#[derive(Debug, Clone, Serialize)]
pub struct SessionReport {
    pub metadata: SessionMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan_results: Option<ScanReport>,
    pub decoded_messages: Vec<ReportDecodedMessage>,
    pub annotations: Vec<Annotation>,
}

/// Export a session report to a file.
pub fn export_session_report(
    report: &SessionReport,
    path: &Path,
    format: &str,
) -> Result<String, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {e}"))?;
    }

    match format {
        "json" => {
            let json = serde_json::to_string_pretty(report)
                .map_err(|e| format!("Serialize error: {e}"))?;
            std::fs::write(path, &json).map_err(|e| format!("Write error: {e}"))?;
        }
        "csv" => {
            let mut file =
                std::fs::File::create(path).map_err(|e| format!("Failed to create file: {e}"))?;

            writeln!(file, "section,key,value").map_err(|e| format!("Write error: {e}"))?;

            // Metadata
            writeln!(
                file,
                "metadata,start_time,\"{}\"",
                report.metadata.start_time
            )
            .map_err(|e| format!("Write error: {e}"))?;
            writeln!(
                file,
                "metadata,duration_secs,{:.1}",
                report.metadata.duration_secs
            )
            .map_err(|e| format!("Write error: {e}"))?;
            writeln!(
                file,
                "metadata,center_freq,{:.0}",
                report.metadata.center_freq
            )
            .map_err(|e| format!("Write error: {e}"))?;
            writeln!(
                file,
                "metadata,sample_rate,{:.0}",
                report.metadata.sample_rate
            )
            .map_err(|e| format!("Write error: {e}"))?;
            writeln!(file, "metadata,gain,\"{}\"", report.metadata.gain)
                .map_err(|e| format!("Write error: {e}"))?;
            writeln!(file, "metadata,fft_size,{}", report.metadata.fft_size)
                .map_err(|e| format!("Write error: {e}"))?;

            // Decoded messages
            for msg in &report.decoded_messages {
                let escaped = msg.summary.replace('"', "\"\"");
                writeln!(
                    file,
                    "decoded,{},\"[{}] {}\"",
                    msg.elapsed_ms, msg.decoder, escaped
                )
                .map_err(|e| format!("Write error: {e}"))?;
            }

            // Annotations
            for ann in &report.annotations {
                let escaped = ann.text.replace('"', "\"\"");
                writeln!(file, "annotation,{:.3},\"{}\"", ann.timestamp_s, escaped)
                    .map_err(|e| format!("Write error: {e}"))?;
            }
        }
        _ => return Err(format!("Unsupported format: {format}")),
    }

    Ok(path.display().to_string())
}

/// Export scan results to a file.
pub fn export_scan_report(
    report: &ScanReport,
    path: &Path,
    format: &str,
) -> Result<String, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {e}"))?;
    }

    match format {
        "json" => {
            let json = serde_json::to_string_pretty(report)
                .map_err(|e| format!("Serialize error: {e}"))?;
            std::fs::write(path, &json).map_err(|e| format!("Write error: {e}"))?;
        }
        "csv" => {
            let mut file =
                std::fs::File::create(path).map_err(|e| format!("Failed to create file: {e}"))?;

            writeln!(file, "frequency_hz,power_db,snr_db,bandwidth_hz")
                .map_err(|e| format!("Write error: {e}"))?;

            for det in &report.detections {
                writeln!(
                    file,
                    "{:.1},{:.1},{:.1},{:.1}",
                    det.frequency_hz, det.power_db, det.snr_db, det.bandwidth_hz
                )
                .map_err(|e| format!("Write error: {e}"))?;
            }
        }
        _ => return Err(format!("Unsupported format: {format}")),
    }

    Ok(path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("waverunner_test_report");
        std::fs::create_dir_all(&dir).ok();
        dir.join(name)
    }

    fn sample_report() -> SessionReport {
        SessionReport {
            metadata: SessionMetadata {
                start_time: "2026-02-15T12:00:00Z".to_string(),
                duration_secs: 120.0,
                center_freq: 100e6,
                sample_rate: 2.048e6,
                gain: "Auto".to_string(),
                fft_size: 2048,
            },
            scan_results: None,
            decoded_messages: vec![ReportDecodedMessage {
                decoder: "pocsag".to_string(),
                elapsed_ms: 5000,
                summary: "Test message".to_string(),
                fields: BTreeMap::new(),
            }],
            annotations: vec![],
        }
    }

    #[test]
    fn session_report_json_roundtrip() {
        let report = sample_report();
        let path = temp_path("test_session_report.json");
        let result = export_session_report(&report, &path, "json");
        assert!(result.is_ok());

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["metadata"]["fft_size"], 2048);
        assert_eq!(parsed["decoded_messages"].as_array().unwrap().len(), 1);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn session_report_csv() {
        let report = sample_report();
        let path = temp_path("test_session_report.csv");
        let result = export_session_report(&report, &path, "csv");
        assert!(result.is_ok());

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with("section,key,value\n"));
        assert!(contents.contains("metadata,fft_size,2048"));
        assert!(contents.contains("decoded"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn scan_report_json() {
        let report = ScanReport {
            start_freq: 88e6,
            end_freq: 108e6,
            step_hz: 1e6,
            dwell_ms: 100,
            signals_found: 2,
            detections: vec![
                ScanDetection {
                    frequency_hz: 91.5e6,
                    power_db: -30.0,
                    snr_db: 20.0,
                    bandwidth_hz: 200e3,
                },
                ScanDetection {
                    frequency_hz: 101.1e6,
                    power_db: -25.0,
                    snr_db: 25.0,
                    bandwidth_hz: 150e3,
                },
            ],
        };
        let path = temp_path("test_scan_report.json");
        let result = export_scan_report(&report, &path, "json");
        assert!(result.is_ok());

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["signals_found"], 2);
        assert_eq!(parsed["detections"].as_array().unwrap().len(), 2);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn scan_report_csv() {
        let report = ScanReport {
            start_freq: 88e6,
            end_freq: 108e6,
            step_hz: 1e6,
            dwell_ms: 100,
            signals_found: 1,
            detections: vec![ScanDetection {
                frequency_hz: 95.5e6,
                power_db: -35.0,
                snr_db: 15.0,
                bandwidth_hz: 100e3,
            }],
        };
        let path = temp_path("test_scan_report.csv");
        let result = export_scan_report(&report, &path, "csv");
        assert!(result.is_ok());

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with("frequency_hz,power_db,snr_db,bandwidth_hz\n"));
        assert!(contents.lines().count() == 2); // header + 1 detection

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn empty_report() {
        let report = SessionReport {
            metadata: SessionMetadata {
                start_time: "2026-02-15T12:00:00Z".to_string(),
                duration_secs: 0.0,
                center_freq: 100e6,
                sample_rate: 2.048e6,
                gain: "Auto".to_string(),
                fft_size: 2048,
            },
            scan_results: None,
            decoded_messages: vec![],
            annotations: vec![],
        };
        let path = temp_path("test_empty_report.json");
        let result = export_session_report(&report, &path, "json");
        assert!(result.is_ok());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn unsupported_format() {
        let report = sample_report();
        let path = temp_path("test_bad_format.xyz");
        let result = export_session_report(&report, &path, "xyz");
        assert!(result.is_err());
    }
}
