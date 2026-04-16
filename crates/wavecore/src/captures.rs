//! Persistent capture catalog and default capture path helpers.
//!
//! The catalog keeps a small recent-history index for replay workflows in the
//! CLI and GUI without requiring a database or external service.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::recording::RecordingMetadata;
use crate::util::{capture_dir, slugify, utc_timestamp_compact};

const MAX_RECENT_CAPTURES: usize = 100;

fn default_schema_v1() -> u32 {
    1
}

/// Source of a catalog entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum CaptureSource {
    #[default]
    LiveRecord,
    Import,
    ReplayExport,
}

/// Serializable recent-capture entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureRecord {
    #[serde(default = "default_schema_v1")]
    pub schema_version: u32,
    pub id: String,
    pub created_at: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeline_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub center_freq: f64,
    pub sample_rate: f64,
    pub format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub demod_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decoder: Option<String>,
    #[serde(default)]
    pub source: CaptureSource,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CaptureCatalogFile {
    #[serde(default)]
    capture: Vec<CaptureRecord>,
}

/// Persistent recent-capture index.
#[derive(Debug, Default)]
pub struct CaptureCatalog {
    captures: Vec<CaptureRecord>,
}

impl CaptureCatalog {
    pub fn load() -> Self {
        let captures = match catalog_path() {
            Some(path) if path.exists() => match fs::read_to_string(&path) {
                Ok(contents) => serde_json::from_str::<CaptureCatalogFile>(&contents)
                    .map(|file| file.capture)
                    .unwrap_or_default(),
                Err(_) => Vec::new(),
            },
            _ => Vec::new(),
        };
        Self { captures }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = catalog_path().ok_or("Cannot determine capture catalog path")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create capture dir: {e}"))?;
        }

        let file = CaptureCatalogFile {
            capture: self.captures.clone(),
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| format!("Failed to serialize capture catalog: {e}"))?;
        fs::write(&path, json).map_err(|e| format!("Failed to write capture catalog: {e}"))
    }

    pub fn list(&self) -> &[CaptureRecord] {
        &self.captures
    }

    pub fn list_recent(&self, limit: usize) -> Vec<CaptureRecord> {
        self.captures.iter().take(limit).cloned().collect()
    }

    pub fn register(
        &mut self,
        recording_path: &Path,
        metadata: &RecordingMetadata,
        source: CaptureSource,
    ) {
        let catalog_path = catalog_capture_path(recording_path, &metadata.format);
        let metadata_path = recording_path.with_extension("json");
        let size_bytes = capture_size_bytes(recording_path);
        let id = format!(
            "{}-{}",
            crate::util::slugify(
                metadata
                    .label
                    .as_deref()
                    .unwrap_or(metadata.timestamp.as_str())
            ),
            utc_timestamp_compact()
        );

        let record = CaptureRecord {
            schema_version: 1,
            id,
            created_at: metadata.timestamp.clone(),
            path: catalog_path.display().to_string(),
            metadata_path: metadata_path
                .exists()
                .then(|| metadata_path.display().to_string()),
            timeline_path: metadata.timeline_path.clone(),
            report_path: metadata.report_path.clone(),
            label: metadata.label.clone(),
            notes: metadata.notes.clone(),
            tags: metadata.tags.clone(),
            center_freq: metadata.center_freq,
            sample_rate: metadata.sample_rate,
            format: metadata.format.clone(),
            duration_secs: metadata.duration_secs,
            size_bytes,
            demod_mode: metadata.demod_mode.clone(),
            decoder: metadata.decoder.clone(),
            source,
        };

        self.captures.retain(|existing| {
            existing.path != record.path || existing.created_at != record.created_at
        });
        self.captures.insert(0, record);
        self.captures.truncate(MAX_RECENT_CAPTURES);
    }

    pub fn prune_missing(&mut self) -> usize {
        let before = self.captures.len();
        self.captures
            .retain(|capture| Path::new(&capture.path).exists());
        before.saturating_sub(self.captures.len())
    }
}

pub fn catalog_path() -> Option<PathBuf> {
    capture_dir().map(|dir| dir.join("catalog.json"))
}

pub fn default_capture_path(format: &str, label: Option<&str>) -> Result<PathBuf, String> {
    let dir = capture_dir().ok_or("Cannot determine capture directory")?;
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create capture directory: {e}"))?;
    Ok(build_default_capture_path(&dir, format, label))
}

fn build_default_capture_path(dir: &Path, format: &str, label: Option<&str>) -> PathBuf {
    let label = label.filter(|s| !s.trim().is_empty()).unwrap_or("capture");
    let base = format!("{}-{}", slugify(label), utc_timestamp_compact());
    match format {
        "wav" => dir.join(format!("{base}.wav")),
        "sigmf" => dir.join(base),
        _ => dir.join(format!("{base}.cf32")),
    }
}

fn capture_size_bytes(path: &Path) -> Option<u64> {
    if path.exists() {
        fs::metadata(path).ok().map(|m| m.len())
    } else {
        let sigmf_data = path.with_extension("sigmf-data");
        fs::metadata(sigmf_data).ok().map(|m| m.len())
    }
}

fn catalog_capture_path(recording_path: &Path, format: &str) -> PathBuf {
    if format.starts_with("sigmf") {
        recording_path.with_extension("sigmf-data")
    } else {
        recording_path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "waverunner_capture_test_{tag}_{}",
            utc_timestamp_compact()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn default_path_uses_expected_extensions() {
        let dir = unique_temp_dir("home");
        let cf32 = build_default_capture_path(&dir, "raw", Some("FM Survey"));
        let wav = build_default_capture_path(&dir, "wav", Some("FM Survey"));
        let sigmf = build_default_capture_path(&dir, "sigmf", Some("FM Survey"));

        assert!(
            cf32.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .starts_with("fm-survey-")
        );
        assert_eq!(cf32.extension().and_then(|e| e.to_str()), Some("cf32"));
        assert_eq!(wav.extension().and_then(|e| e.to_str()), Some("wav"));
        assert_eq!(sigmf.extension().and_then(|e| e.to_str()), None);
    }

    #[test]
    fn register_keeps_newest_first() {
        let mut catalog = CaptureCatalog::default();
        let dir = unique_temp_dir("register");
        let path = dir.join("capture.cf32");
        fs::write(&path, vec![0_u8; 16]).unwrap();

        let meta = RecordingMetadata {
            schema_version: 1,
            center_freq: 100e6,
            sample_rate: 2.048e6,
            gain: "auto".to_string(),
            format: "cf32".to_string(),
            timestamp: "2026-04-16T12:00:00Z".to_string(),
            duration_secs: Some(5.0),
            device: "rtlsdr".to_string(),
            samples_written: 100,
            label: Some("Test".to_string()),
            notes: None,
            tags: vec!["survey".to_string()],
            demod_mode: None,
            decoder: None,
            timeline_path: None,
            report_path: None,
        };

        catalog.register(&path, &meta, CaptureSource::LiveRecord);
        catalog.register(&path, &meta, CaptureSource::LiveRecord);

        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].label.as_deref(), Some("Test"));
    }
}
