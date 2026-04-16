//! Session timeline and annotation system.
//!
//! Provides structured logging of session events and user annotations
//! for workflow support and session review.

use std::io::Write;
use std::path::Path;
use std::time::Instant;

use serde::Serialize;

/// Annotation kind.
#[derive(Debug, Clone, Serialize)]
pub enum AnnotationKind {
    Bookmark,
    Note,
    Tag,
}

/// A user-created annotation on the session timeline.
#[derive(Debug, Clone, Serialize)]
pub struct Annotation {
    pub id: u64,
    pub timestamp_s: f64,
    pub kind: AnnotationKind,
    pub text: String,
    pub frequency_hz: f64,
}

/// An automatically-logged session event.
#[derive(Debug, Clone, Serialize)]
pub enum TimelineEntry {
    FreqChange { timestamp_s: f64, freq_hz: f64 },
    GainChange { timestamp_s: f64, gain: String },
    RecordStart { timestamp_s: f64, path: String },
    RecordStop { timestamp_s: f64, samples: u64 },
    DecoderEnabled { timestamp_s: f64, name: String },
    DecoderDisabled { timestamp_s: f64, name: String },
    Annotation { timestamp_s: f64, id: u64 },
    LoadShedding { timestamp_s: f64, level: u8 },
}

/// Session timeline: collects events and annotations with timestamps.
pub struct SessionTimeline {
    entries: Vec<TimelineEntry>,
    annotations: Vec<Annotation>,
    next_id: u64,
    start: Instant,
}

impl SessionTimeline {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            annotations: Vec::new(),
            next_id: 1,
            start: Instant::now(),
        }
    }

    /// Elapsed seconds since session start.
    pub fn elapsed_s(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    /// Number of timeline entries logged.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Log a timeline event.
    pub fn log_event(&mut self, entry: TimelineEntry) {
        self.entries.push(entry);
    }

    /// Add a user annotation. Returns the annotation id.
    pub fn add_annotation(&mut self, kind: AnnotationKind, text: String, frequency_hz: f64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let timestamp_s = self.elapsed_s();
        self.annotations.push(Annotation {
            id,
            timestamp_s,
            kind,
            text,
            frequency_hz,
        });
        self.entries
            .push(TimelineEntry::Annotation { timestamp_s, id });
        id
    }

    /// Get all annotations.
    pub fn annotations(&self) -> &[Annotation] {
        &self.annotations
    }

    /// Get all timeline entries.
    pub fn entries(&self) -> &[TimelineEntry] {
        &self.entries
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the timeline is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Export timeline to JSON.
    pub fn export_json(&self, path: &Path) -> Result<String, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {e}"))?;
        }

        let doc = serde_json::json!({
            "schema_version": 1,
            "timeline": self.entries,
            "annotations": self.annotations,
        });
        let json =
            serde_json::to_string_pretty(&doc).map_err(|e| format!("Serialize error: {e}"))?;
        std::fs::write(path, &json).map_err(|e| format!("Write error: {e}"))?;
        Ok(path.display().to_string())
    }

    /// Export timeline to CSV.
    pub fn export_csv(&self, path: &Path) -> Result<String, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {e}"))?;
        }

        let mut file =
            std::fs::File::create(path).map_err(|e| format!("Failed to create file: {e}"))?;

        writeln!(file, "timestamp_s,event_type,detail").map_err(|e| format!("Write error: {e}"))?;

        for entry in &self.entries {
            let (ts, etype, detail) = match entry {
                TimelineEntry::FreqChange {
                    timestamp_s,
                    freq_hz,
                } => (*timestamp_s, "freq_change", format!("{freq_hz:.0}")),
                TimelineEntry::GainChange { timestamp_s, gain } => {
                    (*timestamp_s, "gain_change", gain.clone())
                }
                TimelineEntry::RecordStart { timestamp_s, path } => {
                    (*timestamp_s, "record_start", path.clone())
                }
                TimelineEntry::RecordStop {
                    timestamp_s,
                    samples,
                } => (*timestamp_s, "record_stop", format!("{samples}")),
                TimelineEntry::DecoderEnabled { timestamp_s, name } => {
                    (*timestamp_s, "decoder_on", name.clone())
                }
                TimelineEntry::DecoderDisabled { timestamp_s, name } => {
                    (*timestamp_s, "decoder_off", name.clone())
                }
                TimelineEntry::Annotation { timestamp_s, id } => {
                    let text = self
                        .annotations
                        .iter()
                        .find(|a| a.id == *id)
                        .map(|a| a.text.clone())
                        .unwrap_or_default();
                    (*timestamp_s, "annotation", text)
                }
                TimelineEntry::LoadShedding { timestamp_s, level } => {
                    (*timestamp_s, "load_shedding", format!("{level}"))
                }
            };
            let escaped = detail.replace('"', "\"\"");
            writeln!(file, "{ts:.3},{etype},\"{escaped}\"")
                .map_err(|e| format!("Write error: {e}"))?;
        }

        Ok(path.display().to_string())
    }
}

impl Default for SessionTimeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("waverunner_test_timeline");
        std::fs::create_dir_all(&dir).ok();
        dir.join(name)
    }

    #[test]
    fn timeline_creation() {
        let tl = SessionTimeline::new();
        assert!(tl.is_empty());
        assert_eq!(tl.len(), 0);
        assert!(tl.annotations().is_empty());
    }

    #[test]
    fn log_events() {
        let mut tl = SessionTimeline::new();
        tl.log_event(TimelineEntry::FreqChange {
            timestamp_s: 0.0,
            freq_hz: 100e6,
        });
        tl.log_event(TimelineEntry::GainChange {
            timestamp_s: 0.1,
            gain: "Auto".to_string(),
        });
        assert_eq!(tl.len(), 2);
    }

    #[test]
    fn add_annotation() {
        let mut tl = SessionTimeline::new();
        let id1 = tl.add_annotation(
            AnnotationKind::Bookmark,
            "First bookmark".to_string(),
            100e6,
        );
        let id2 = tl.add_annotation(AnnotationKind::Note, "A note".to_string(), 200e6);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(tl.annotations().len(), 2);
        // Each annotation also adds a timeline entry
        assert_eq!(tl.len(), 2);
    }

    #[test]
    fn annotation_fields() {
        let mut tl = SessionTimeline::new();
        let id = tl.add_annotation(AnnotationKind::Tag, "Tagged signal".to_string(), 433.92e6);
        let ann = &tl.annotations()[0];
        assert_eq!(ann.id, id);
        assert_eq!(ann.text, "Tagged signal");
        assert!((ann.frequency_hz - 433.92e6).abs() < 1.0);
        assert!(ann.timestamp_s >= 0.0);
    }

    #[test]
    fn export_json_roundtrip() {
        let mut tl = SessionTimeline::new();
        tl.log_event(TimelineEntry::FreqChange {
            timestamp_s: 0.0,
            freq_hz: 100e6,
        });
        tl.add_annotation(AnnotationKind::Bookmark, "Test".to_string(), 100e6);

        let path = temp_path("test_timeline.json");
        let result = tl.export_json(&path);
        assert!(result.is_ok());

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert!(parsed["timeline"].is_array());
        assert!(parsed["annotations"].is_array());
        assert_eq!(parsed["annotations"].as_array().unwrap().len(), 1);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn export_csv_format() {
        let mut tl = SessionTimeline::new();
        tl.log_event(TimelineEntry::RecordStart {
            timestamp_s: 1.5,
            path: "/tmp/test.cf32".to_string(),
        });
        tl.log_event(TimelineEntry::RecordStop {
            timestamp_s: 5.0,
            samples: 10000,
        });
        tl.add_annotation(
            AnnotationKind::Note,
            "Signal with \"quotes\"".to_string(),
            100e6,
        );

        let path = temp_path("test_timeline.csv");
        let result = tl.export_csv(&path);
        assert!(result.is_ok());

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with("timestamp_s,event_type,detail\n"));
        assert!(contents.contains("record_start"));
        assert!(contents.contains("record_stop"));
        assert!(contents.contains("annotation"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn mixed_events_ordering() {
        let mut tl = SessionTimeline::new();
        tl.log_event(TimelineEntry::FreqChange {
            timestamp_s: 0.0,
            freq_hz: 100e6,
        });
        tl.add_annotation(AnnotationKind::Bookmark, "BM1".to_string(), 100e6);
        tl.log_event(TimelineEntry::DecoderEnabled {
            timestamp_s: 1.0,
            name: "pocsag".to_string(),
        });
        tl.log_event(TimelineEntry::LoadShedding {
            timestamp_s: 2.0,
            level: 1,
        });
        tl.add_annotation(AnnotationKind::Note, "Note1".to_string(), 200e6);

        assert_eq!(tl.len(), 5);
        assert_eq!(tl.annotations().len(), 2);
    }

    #[test]
    fn elapsed_s_increases() {
        let tl = SessionTimeline::new();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(tl.elapsed_s() >= 0.009);
    }

    #[test]
    fn export_empty_timeline() {
        let tl = SessionTimeline::new();
        let path = temp_path("test_empty_timeline.json");
        let result = tl.export_json(&path);
        assert!(result.is_ok());

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["timeline"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["annotations"].as_array().unwrap().len(), 0);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn default_trait() {
        let tl = SessionTimeline::default();
        assert!(tl.is_empty());
    }
}
