//! Persistent capture catalog and default capture path helpers.
//!
//! The catalog keeps a small recent-history index for replay workflows in the
//! CLI and GUI without requiring a database or external service.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::recording::RecordingMetadata;
use crate::sigmf::read_sigmf_meta;
use crate::util::{capture_dir, slugify, utc_timestamp_compact, utc_timestamp_now};

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMetadataSource {
    RecordingSidecar,
    SigMf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureOpenInfo {
    pub data_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_source: Option<CaptureMetadataSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub center_freq: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct CaptureImportOptions {
    pub sample_rate: Option<f64>,
    pub center_freq: Option<f64>,
    pub label: Option<String>,
    pub notes: Option<String>,
    pub tags: Vec<String>,
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

    pub fn latest(&self) -> Option<&CaptureRecord> {
        self.captures.first()
    }

    pub fn latest_mut(&mut self) -> Option<&mut CaptureRecord> {
        self.captures.first_mut()
    }

    pub fn select(&self, selector: &str) -> Option<&CaptureRecord> {
        if selector == "latest" {
            return self.latest();
        }

        self.captures
            .iter()
            .find(|record| record_matches_selector(record, selector))
    }

    pub fn select_mut(&mut self, selector: &str) -> Option<&mut CaptureRecord> {
        if selector == "latest" {
            return self.latest_mut();
        }

        self.captures
            .iter_mut()
            .find(|record| record_matches_selector(record, selector))
    }

    pub fn upsert(&mut self, record: CaptureRecord) {
        self.captures.retain(|existing| existing.id != record.id);
        self.captures.insert(0, record);
        self.captures.truncate(MAX_RECENT_CAPTURES);
    }

    pub fn remove_selected(&mut self, selector: &str) -> Option<CaptureRecord> {
        let idx = if selector == "latest" {
            (!self.captures.is_empty()).then_some(0)
        } else {
            self.captures
                .iter()
                .position(|record| record_matches_selector(record, selector))
        }?;
        Some(self.captures.remove(idx))
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

pub fn latest_capture() -> Result<CaptureRecord, String> {
    let mut catalog = CaptureCatalog::load();
    if catalog.prune_missing() > 0 {
        catalog.save()?;
    }
    catalog
        .latest()
        .cloned()
        .ok_or_else(|| "No recent captures are indexed yet.".to_string())
}

pub fn find_capture(selector: &str) -> Result<CaptureRecord, String> {
    let mut catalog = CaptureCatalog::load();
    if catalog.prune_missing() > 0 {
        catalog.save()?;
    }
    catalog
        .select(selector)
        .cloned()
        .ok_or_else(|| format!("No capture matches selector '{selector}'."))
}

pub fn import_capture(
    input: &Path,
    options: CaptureImportOptions,
) -> Result<CaptureRecord, String> {
    let info = inspect_capture_input(input)?;
    let data_path = PathBuf::from(&info.data_path);
    let sample_rate = options
        .sample_rate
        .or(info.sample_rate)
        .ok_or_else(|| {
            format!(
                "Sample rate is unknown for {}. Pass --sample-rate when importing raw captures without metadata.",
                input.display()
            )
        })?;
    let center_freq = options.center_freq.or(info.center_freq).unwrap_or(0.0);

    let mut metadata = info
        .metadata_path
        .as_deref()
        .and_then(load_recording_metadata)
        .unwrap_or_else(|| RecordingMetadata {
            schema_version: 1,
            center_freq,
            sample_rate,
            gain: "unknown".to_string(),
            format: detect_capture_format(&data_path),
            timestamp: utc_timestamp_now(),
            duration_secs: estimate_duration_secs(&data_path, sample_rate),
            device: "import".to_string(),
            samples_written: estimate_samples_written(&data_path, sample_rate),
            label: None,
            notes: None,
            tags: Vec::new(),
            demod_mode: None,
            decoder: None,
            timeline_path: None,
            report_path: None,
        });

    metadata.center_freq = center_freq;
    metadata.sample_rate = sample_rate;
    if let Some(label) = options.label {
        metadata.label = Some(label);
    }
    if let Some(notes) = options.notes {
        metadata.notes = Some(notes);
    }
    if !options.tags.is_empty() {
        metadata.tags = options.tags;
    }
    if metadata.duration_secs.is_none() {
        metadata.duration_secs = estimate_duration_secs(&data_path, sample_rate);
    }
    if metadata.samples_written == 0 {
        metadata.samples_written = estimate_samples_written(&data_path, sample_rate);
    }
    if metadata.format.is_empty() {
        metadata.format = detect_capture_format(&data_path);
    }

    let metadata_path = match info.metadata_path {
        Some(path) => Some(PathBuf::from(path)),
        None if !metadata.format.starts_with("sigmf") => {
            let path = data_path.with_extension("json");
            metadata
                .write_to_path(&path)
                .map_err(|e| format!("Failed to write import metadata: {e}"))?;
            Some(path)
        }
        _ => None,
    };

    Ok(build_capture_record(
        &data_path,
        metadata_path.as_deref(),
        &metadata,
        CaptureSource::Import,
    ))
}

pub fn sync_catalog_metadata(record: &CaptureRecord) -> Result<(), String> {
    let Some(path) = record.metadata_path.as_deref() else {
        return Ok(());
    };
    let metadata_path = Path::new(path);
    if metadata_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_none_or(|ext| !ext.eq_ignore_ascii_case("json"))
    {
        return Ok(());
    }

    let mut metadata = load_recording_metadata(path).unwrap_or_else(|| RecordingMetadata {
        schema_version: 1,
        center_freq: record.center_freq,
        sample_rate: record.sample_rate,
        gain: "unknown".to_string(),
        format: record.format.clone(),
        timestamp: record.created_at.clone(),
        duration_secs: record.duration_secs,
        device: "catalog".to_string(),
        samples_written: 0,
        label: None,
        notes: None,
        tags: Vec::new(),
        demod_mode: record.demod_mode.clone(),
        decoder: record.decoder.clone(),
        timeline_path: record.timeline_path.clone(),
        report_path: record.report_path.clone(),
    });

    metadata.center_freq = record.center_freq;
    metadata.sample_rate = record.sample_rate;
    metadata.format = record.format.clone();
    metadata.duration_secs = record.duration_secs;
    metadata.label = record.label.clone();
    metadata.notes = record.notes.clone();
    metadata.tags = record.tags.clone();
    metadata.demod_mode = record.demod_mode.clone();
    metadata.decoder = record.decoder.clone();
    metadata.timeline_path = record.timeline_path.clone();
    metadata.report_path = record.report_path.clone();

    metadata
        .write_to_path(metadata_path)
        .map_err(|e| format!("Failed to sync capture metadata: {e}"))
}

pub fn delete_capture_artifacts(record: &CaptureRecord) -> Result<(), String> {
    for path in [
        Some(record.path.as_str()),
        record.metadata_path.as_deref(),
        record.timeline_path.as_deref(),
        record.report_path.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let candidate = Path::new(path);
        if candidate.exists() {
            fs::remove_file(candidate)
                .map_err(|e| format!("Failed to delete {}: {e}", candidate.display()))?;
        }
    }
    Ok(())
}

pub fn inspect_capture_input(input: &Path) -> Result<CaptureOpenInfo, String> {
    if let Some(info) = inspect_sigmf_input(input) {
        return Ok(info);
    }

    if input
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
        if let Some(info) = inspect_recording_sidecar_input(input) {
            return Ok(info);
        }

        return Err(format!(
            "Could not resolve recording data from metadata sidecar {}",
            input.display()
        ));
    }

    if !input.exists() {
        return Err(format!("Capture file not found: {}", input.display()));
    }

    let metadata_path = input.with_extension("json");
    let metadata = RecordingMetadata::read_sidecar(input).ok();

    Ok(CaptureOpenInfo {
        data_path: input.display().to_string(),
        metadata_path: metadata_path
            .exists()
            .then(|| metadata_path.display().to_string()),
        metadata_source: metadata
            .as_ref()
            .map(|_| CaptureMetadataSource::RecordingSidecar),
        sample_rate: metadata.as_ref().map(|meta| meta.sample_rate),
        center_freq: metadata.as_ref().map(|meta| meta.center_freq),
    })
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

fn build_capture_record(
    data_path: &Path,
    metadata_path: Option<&Path>,
    metadata: &RecordingMetadata,
    source: CaptureSource,
) -> CaptureRecord {
    let size_bytes = capture_size_bytes(data_path);
    let id = format!(
        "{}-{}",
        slugify(
            metadata
                .label
                .as_deref()
                .unwrap_or(metadata.timestamp.as_str())
        ),
        utc_timestamp_compact()
    );

    CaptureRecord {
        schema_version: 1,
        id,
        created_at: metadata.timestamp.clone(),
        path: catalog_capture_path(data_path, &metadata.format)
            .display()
            .to_string(),
        metadata_path: metadata_path.map(|path| path.display().to_string()),
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

fn estimate_samples_written(path: &Path, _sample_rate: f64) -> u64 {
    let format = detect_capture_format(path);
    match format.as_str() {
        "cu8" => fs::metadata(path).ok().map(|m| m.len() / 2).unwrap_or(0),
        "cf32-wav" => hound::WavReader::open(path)
            .ok()
            .map(|reader| u64::from(reader.duration()))
            .unwrap_or(0),
        _ => fs::metadata(path).ok().map(|m| m.len() / 8).unwrap_or(0),
    }
}

fn estimate_duration_secs(path: &Path, sample_rate: f64) -> Option<f64> {
    if sample_rate <= 0.0 {
        return None;
    }

    let samples = estimate_samples_written(path, sample_rate);
    (samples > 0).then_some(samples as f64 / sample_rate)
}

fn detect_capture_format(path: &Path) -> String {
    let lower_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if lower_name.ends_with(".sigmf-data") {
        return "sigmf-cf32_le".to_string();
    }

    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("wav") => "cf32-wav".to_string(),
        Some(ext)
            if ext.eq_ignore_ascii_case("cu8")
                || ext.eq_ignore_ascii_case("cs8")
                || ext.eq_ignore_ascii_case("u8") =>
        {
            "cu8".to_string()
        }
        _ => "cf32".to_string(),
    }
}

fn load_recording_metadata(path: &str) -> Option<RecordingMetadata> {
    let metadata_path = Path::new(path);
    if metadata_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_none_or(|ext| !ext.eq_ignore_ascii_case("json"))
    {
        return None;
    }

    RecordingMetadata::read_from_path(metadata_path).ok()
}

fn record_matches_selector(record: &CaptureRecord, selector: &str) -> bool {
    record.id == selector
        || record.path == selector
        || record.metadata_path.as_deref() == Some(selector)
}

fn catalog_capture_path(recording_path: &Path, format: &str) -> PathBuf {
    if format.starts_with("sigmf") {
        recording_path.with_extension("sigmf-data")
    } else {
        recording_path.to_path_buf()
    }
}

fn inspect_sigmf_input(input: &Path) -> Option<CaptureOpenInfo> {
    let (data_path, meta_path) = resolve_sigmf_paths(input)?;
    if !data_path.exists() {
        return None;
    }

    let (sample_rate, center_freq, metadata_path) = match meta_path.as_ref() {
        Some(path) if path.exists() => match read_sigmf_meta(path.with_extension("")) {
            Ok(meta) => (
                meta.global.sample_rate,
                meta.captures.iter().find_map(|capture| capture.frequency),
                Some(path.display().to_string()),
            ),
            Err(_) => (None, None, Some(path.display().to_string())),
        },
        Some(path) => (None, None, Some(path.display().to_string())),
        None => (None, None, None),
    };

    Some(CaptureOpenInfo {
        data_path: data_path.display().to_string(),
        metadata_path,
        metadata_source: Some(CaptureMetadataSource::SigMf),
        sample_rate,
        center_freq,
    })
}

fn inspect_recording_sidecar_input(input: &Path) -> Option<CaptureOpenInfo> {
    let metadata = RecordingMetadata::read_from_path(input).ok()?;
    let data_path = recording_data_candidates(input, &metadata.format)
        .into_iter()
        .find(|path| path.exists())?;

    Some(CaptureOpenInfo {
        data_path: data_path.display().to_string(),
        metadata_path: Some(input.display().to_string()),
        metadata_source: Some(CaptureMetadataSource::RecordingSidecar),
        sample_rate: Some(metadata.sample_rate),
        center_freq: Some(metadata.center_freq),
    })
}

fn recording_data_candidates(sidecar_path: &Path, format: &str) -> Vec<PathBuf> {
    if format.starts_with("sigmf") {
        return vec![sidecar_path.with_extension("sigmf-data")];
    }

    if format == "cf32-wav" {
        return vec![sidecar_path.with_extension("wav")];
    }

    vec![
        sidecar_path.with_extension("cf32"),
        sidecar_path.with_extension("raw"),
        sidecar_path.with_extension("iq"),
        sidecar_path.with_extension("fc32"),
        sidecar_path.with_extension("cfile"),
    ]
}

fn resolve_sigmf_paths(input: &Path) -> Option<(PathBuf, Option<PathBuf>)> {
    let input_name = input.file_name()?.to_str()?.to_ascii_lowercase();
    if input_name.ends_with(".sigmf-meta") {
        return Some((
            input.with_extension("sigmf-data"),
            Some(input.to_path_buf()),
        ));
    }

    if input_name.ends_with(".sigmf-data") {
        return Some((
            input.to_path_buf(),
            Some(input.with_extension("sigmf-meta")),
        ));
    }

    if input.extension().is_none() {
        let data_path = input.with_extension("sigmf-data");
        if data_path.exists() {
            return Some((data_path, Some(input.with_extension("sigmf-meta"))));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sigmf::SigMfWriter;
    use std::io::Write;

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

    #[test]
    fn inspect_regular_capture_reads_sidecar() {
        let dir = unique_temp_dir("inspect-sidecar");
        let path = dir.join("capture.cf32");
        fs::write(&path, vec![0_u8; 16]).unwrap();

        let meta = RecordingMetadata {
            schema_version: 1,
            center_freq: 162.55e6,
            sample_rate: 2.048e6,
            gain: "auto".to_string(),
            format: "cf32".to_string(),
            timestamp: "2026-04-16T12:00:00Z".to_string(),
            duration_secs: Some(3.0),
            device: "rtlsdr".to_string(),
            samples_written: 2,
            label: Some("WX".to_string()),
            notes: None,
            tags: Vec::new(),
            demod_mode: None,
            decoder: None,
            timeline_path: None,
            report_path: None,
        };
        meta.write_sidecar(&path).unwrap();

        let info = inspect_capture_input(&path).unwrap();
        assert_eq!(info.data_path, path.display().to_string());
        assert_eq!(
            info.metadata_source,
            Some(CaptureMetadataSource::RecordingSidecar)
        );
        assert_eq!(info.sample_rate, Some(2.048e6));
        assert_eq!(info.center_freq, Some(162.55e6));
    }

    #[test]
    fn inspect_sigmf_base_path_reads_sigmf_meta() {
        let base = unique_temp_dir("inspect-sigmf").join("apt-pass");
        let mut writer = SigMfWriter::new(&base, 137.1e6, 1.024e6).unwrap();
        writer
            .write_samples(&[crate::types::Sample::new(0.0, 0.0)])
            .unwrap();
        writer.finalize().unwrap();

        let info = inspect_capture_input(&base).unwrap();
        assert_eq!(
            info.data_path,
            base.with_extension("sigmf-data").display().to_string()
        );
        assert_eq!(info.metadata_source, Some(CaptureMetadataSource::SigMf));
        assert_eq!(info.sample_rate, Some(1.024e6));
        assert_eq!(info.center_freq, Some(137.1e6));

        fs::remove_file(base.with_extension("sigmf-data")).ok();
        fs::remove_file(base.with_extension("sigmf-meta")).ok();
    }

    #[test]
    fn inspect_sidecar_path_resolves_wav_capture() {
        let dir = unique_temp_dir("inspect-sidecar-json");
        let wav_path = dir.join("broadcast.wav");
        let mut file = fs::File::create(&wav_path).unwrap();
        file.write_all(b"RIFF").unwrap();

        let meta_path = dir.join("broadcast.json");
        let meta = RecordingMetadata {
            schema_version: 1,
            center_freq: 99.9e6,
            sample_rate: 1.024e6,
            gain: "auto".to_string(),
            format: "cf32-wav".to_string(),
            timestamp: "2026-04-16T12:00:00Z".to_string(),
            duration_secs: Some(8.0),
            device: "rtlsdr".to_string(),
            samples_written: 2,
            label: None,
            notes: None,
            tags: Vec::new(),
            demod_mode: None,
            decoder: None,
            timeline_path: None,
            report_path: None,
        };
        fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();

        let info = inspect_capture_input(&meta_path).unwrap();
        assert_eq!(info.data_path, wav_path.display().to_string());
        assert_eq!(info.sample_rate, Some(1.024e6));
        assert_eq!(info.center_freq, Some(99.9e6));
    }

    #[test]
    fn inspect_missing_capture_returns_error() {
        let missing = unique_temp_dir("inspect-missing").join("missing.cf32");
        let err = inspect_capture_input(&missing).unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn latest_capture_returns_most_recent_entry() {
        let mut catalog = CaptureCatalog::default();
        let dir = unique_temp_dir("latest");
        let path = dir.join("latest.cf32");
        fs::write(&path, vec![0_u8; 16]).unwrap();

        let meta = RecordingMetadata {
            schema_version: 1,
            center_freq: 162.55e6,
            sample_rate: 1.024e6,
            gain: "auto".to_string(),
            format: "cf32".to_string(),
            timestamp: "2026-04-17T12:00:00Z".to_string(),
            duration_secs: Some(1.0),
            device: "test".to_string(),
            samples_written: 2,
            label: Some("Latest".to_string()),
            notes: None,
            tags: Vec::new(),
            demod_mode: None,
            decoder: None,
            timeline_path: None,
            report_path: None,
        };

        catalog.register(&path, &meta, CaptureSource::LiveRecord);
        assert_eq!(catalog.latest().unwrap().path, path.display().to_string());
    }

    #[test]
    fn import_capture_writes_sidecar_for_raw_input_without_metadata() {
        let dir = unique_temp_dir("import-raw");
        let path = dir.join("import.cf32");
        fs::write(&path, vec![0_u8; 32]).unwrap();

        let record = import_capture(
            &path,
            CaptureImportOptions {
                sample_rate: Some(2.048e6),
                center_freq: Some(433.92e6),
                label: Some("Imported".to_string()),
                notes: Some("from disk".to_string()),
                tags: vec!["test".to_string()],
            },
        )
        .unwrap();

        assert_eq!(record.label.as_deref(), Some("Imported"));
        assert!(Path::new(record.metadata_path.as_deref().unwrap()).exists());
        let meta =
            RecordingMetadata::read_from_path(Path::new(record.metadata_path.as_deref().unwrap()))
                .unwrap();
        assert_eq!(meta.center_freq, 433.92e6);
        assert_eq!(meta.sample_rate, 2.048e6);
        assert_eq!(meta.tags, vec!["test".to_string()]);
    }

    #[test]
    fn catalog_remove_selected_matches_id() {
        let mut catalog = CaptureCatalog::default();
        let dir = unique_temp_dir("remove-selected");
        let path = dir.join("capture.cf32");
        fs::write(&path, vec![0_u8; 16]).unwrap();

        let meta = RecordingMetadata {
            schema_version: 1,
            center_freq: 100e6,
            sample_rate: 2.048e6,
            gain: "auto".to_string(),
            format: "cf32".to_string(),
            timestamp: "2026-04-17T12:00:00Z".to_string(),
            duration_secs: Some(1.0),
            device: "test".to_string(),
            samples_written: 2,
            label: Some("Remove".to_string()),
            notes: None,
            tags: Vec::new(),
            demod_mode: None,
            decoder: None,
            timeline_path: None,
            report_path: None,
        };
        catalog.register(&path, &meta, CaptureSource::LiveRecord);

        let id = catalog.latest().unwrap().id.clone();
        let removed = catalog.remove_selected(&id).unwrap();
        assert_eq!(removed.id, id);
        assert!(catalog.list().is_empty());
    }

    #[test]
    fn sync_catalog_metadata_updates_json_sidecar() {
        let dir = unique_temp_dir("sync-sidecar");
        let path = dir.join("capture.cf32");
        fs::write(&path, vec![0_u8; 16]).unwrap();

        let meta = RecordingMetadata {
            schema_version: 1,
            center_freq: 162.55e6,
            sample_rate: 2.048e6,
            gain: "auto".to_string(),
            format: "cf32".to_string(),
            timestamp: "2026-04-17T12:00:00Z".to_string(),
            duration_secs: Some(3.0),
            device: "test".to_string(),
            samples_written: 2,
            label: Some("Old".to_string()),
            notes: None,
            tags: Vec::new(),
            demod_mode: None,
            decoder: None,
            timeline_path: None,
            report_path: None,
        };
        meta.write_sidecar(&path).unwrap();

        let record = CaptureRecord {
            schema_version: 1,
            id: "sync".to_string(),
            created_at: meta.timestamp.clone(),
            path: path.display().to_string(),
            metadata_path: Some(path.with_extension("json").display().to_string()),
            timeline_path: None,
            report_path: None,
            label: Some("New".to_string()),
            notes: Some("Updated".to_string()),
            tags: vec!["one".to_string(), "two".to_string()],
            center_freq: meta.center_freq,
            sample_rate: meta.sample_rate,
            format: meta.format.clone(),
            duration_secs: meta.duration_secs,
            size_bytes: Some(16),
            demod_mode: None,
            decoder: None,
            source: CaptureSource::Import,
        };

        sync_catalog_metadata(&record).unwrap();
        let updated = RecordingMetadata::read_from_path(&path.with_extension("json")).unwrap();
        assert_eq!(updated.label.as_deref(), Some("New"));
        assert_eq!(updated.notes.as_deref(), Some("Updated"));
        assert_eq!(updated.tags, vec!["one".to_string(), "two".to_string()]);
    }
}
