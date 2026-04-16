//! SigMF (Signal Metadata Format) v1.0.0 writer.
//!
//! Produces a `.sigmf-meta` JSON sidecar and `.sigmf-data` binary file
//! conforming to the [SigMF specification](https://sigmf.org).
//!
//! ## File Pair
//!
//! | File              | Contents                                    |
//! |-------------------|---------------------------------------------|
//! | `name.sigmf-data` | Raw interleaved cf32_le binary samples       |
//! | `name.sigmf-meta` | JSON: global, `captures[]`, `annotations[]`  |
//!
//! ## Usage
//!
//! ```ignore
//! let mut w = SigMfWriter::new("recording", 433.92e6, 2.048e6)?;
//! w.write_samples(&samples)?;
//! w.add_annotation(0, 1024, "POCSAG burst detected");
//! w.finalize()?;
//! ```
//!
//! ## Data Type
//!
//! Always writes `cf32_le` — interleaved little-endian 32-bit float
//! complex pairs. This is the most widely supported SigMF format and
//! matches our internal `Sample` type (`Complex<f32>`).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::WaveError;
use crate::types::Sample;

// ============================================================================
// SigMF metadata types (v1.0.0)
// ============================================================================

/// Top-level SigMF metadata document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigMfMeta {
    /// Global metadata — applies to the entire dataset.
    pub global: SigMfGlobal,
    /// Capture segments — each describes a contiguous block of samples.
    pub captures: Vec<SigMfCapture>,
    /// Annotations — regions of interest within the data.
    #[serde(default)]
    pub annotations: Vec<SigMfAnnotation>,
}

/// Global metadata block.
///
/// Required fields per SigMF v1.0.0:
/// - `core:datatype` (always `cf32_le`)
/// - `core:version` (always `1.0.0`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigMfGlobal {
    /// SigMF data type. Always "cf32_le" for this writer.
    #[serde(rename = "core:datatype")]
    pub datatype: String,

    /// SigMF spec version.
    #[serde(rename = "core:version")]
    pub version: String,

    /// Sample rate in samples/second.
    #[serde(rename = "core:sample_rate", skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<f64>,

    /// Description of the recording.
    #[serde(rename = "core:description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Author name.
    #[serde(rename = "core:author", skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,

    /// Hardware description.
    #[serde(rename = "core:hw", skip_serializing_if = "Option::is_none")]
    pub hw: Option<String>,

    /// Recording application.
    #[serde(rename = "core:recorder", skip_serializing_if = "Option::is_none")]
    pub recorder: Option<String>,

    /// SHA-512 hash of the data file.
    #[serde(rename = "core:sha512", skip_serializing_if = "Option::is_none")]
    pub sha512: Option<String>,

    /// Total number of samples in the data file.
    #[serde(rename = "core:num_channels", skip_serializing_if = "Option::is_none")]
    pub num_channels: Option<u32>,
}

/// Capture segment metadata.
///
/// Each capture describes where in the data file a contiguous block
/// of samples begins and what center frequency was being received.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigMfCapture {
    /// Sample index where this capture starts (0-based).
    #[serde(rename = "core:sample_start")]
    pub sample_start: u64,

    /// Center frequency of the capture in Hz.
    #[serde(rename = "core:frequency", skip_serializing_if = "Option::is_none")]
    pub frequency: Option<f64>,

    /// ISO 8601 date-time string for the start of this capture.
    #[serde(rename = "core:datetime", skip_serializing_if = "Option::is_none")]
    pub datetime: Option<String>,
}

/// Annotation metadata — marks a region of interest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigMfAnnotation {
    /// Sample index where this annotation starts.
    #[serde(rename = "core:sample_start")]
    pub sample_start: u64,

    /// Number of samples this annotation covers.
    #[serde(rename = "core:sample_count")]
    pub sample_count: u64,

    /// Center frequency of the annotated signal in Hz.
    #[serde(
        rename = "core:freq_lower_edge",
        skip_serializing_if = "Option::is_none"
    )]
    pub freq_lower_edge: Option<f64>,

    /// Upper frequency edge in Hz.
    #[serde(
        rename = "core:freq_upper_edge",
        skip_serializing_if = "Option::is_none"
    )]
    pub freq_upper_edge: Option<f64>,

    /// Human-readable label for this annotation.
    #[serde(rename = "core:label", skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Additional description or comment.
    #[serde(rename = "core:description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ============================================================================
// SigMF Writer
// ============================================================================

/// Streaming SigMF file writer.
///
/// Writes cf32_le samples to `.sigmf-data` and produces the corresponding
/// `.sigmf-meta` JSON on finalization.
pub struct SigMfWriter {
    /// Base path (without extension). Data goes to `{base}.sigmf-data`,
    /// meta goes to `{base}.sigmf-meta`.
    base_path: PathBuf,
    /// Buffered writer for the binary data file.
    data_writer: BufWriter<File>,
    /// Metadata being accumulated.
    meta: SigMfMeta,
    /// Running sample count.
    samples_written: u64,
}

impl SigMfWriter {
    /// Create a new SigMF writer.
    ///
    /// `base_path` is the stem — files will be `{base_path}.sigmf-data`
    /// and `{base_path}.sigmf-meta`.
    ///
    /// A single capture segment is created at sample 0 with the given
    /// center frequency.
    pub fn new(
        base_path: impl AsRef<Path>,
        center_freq: f64,
        sample_rate: f64,
    ) -> Result<Self, WaveError> {
        let base = base_path.as_ref().to_path_buf();
        let data_path = base.with_extension("sigmf-data");
        let data_file = File::create(&data_path)?;

        let meta = SigMfMeta {
            global: SigMfGlobal {
                datatype: "cf32_le".to_string(),
                version: "1.0.0".to_string(),
                sample_rate: Some(sample_rate),
                description: None,
                author: None,
                hw: None,
                recorder: Some("waverunner".to_string()),
                sha512: None,
                num_channels: None,
            },
            captures: vec![SigMfCapture {
                sample_start: 0,
                frequency: Some(center_freq),
                datetime: None,
            }],
            annotations: Vec::new(),
        };

        Ok(Self {
            base_path: base,
            data_writer: BufWriter::new(data_file),
            meta,
            samples_written: 0,
        })
    }

    /// Write IQ samples to the data file.
    pub fn write_samples(&mut self, samples: &[Sample]) -> Result<(), WaveError> {
        for s in samples {
            self.data_writer.write_all(&s.re.to_le_bytes())?;
            self.data_writer.write_all(&s.im.to_le_bytes())?;
        }
        self.samples_written += samples.len() as u64;
        Ok(())
    }

    /// Add a capture segment at the current sample position.
    ///
    /// Useful when the center frequency changes mid-recording
    /// (e.g., during a scan).
    pub fn add_capture(&mut self, frequency: f64) {
        self.meta.captures.push(SigMfCapture {
            sample_start: self.samples_written,
            frequency: Some(frequency),
            datetime: None,
        });
    }

    /// Add an annotation marking a region of interest.
    pub fn add_annotation(&mut self, sample_start: u64, sample_count: u64, label: &str) {
        self.meta.annotations.push(SigMfAnnotation {
            sample_start,
            sample_count,
            freq_lower_edge: None,
            freq_upper_edge: None,
            label: Some(label.to_string()),
            description: None,
        });
    }

    /// Add an annotation with frequency bounds.
    pub fn add_annotation_with_freq(
        &mut self,
        sample_start: u64,
        sample_count: u64,
        label: &str,
        freq_lower: f64,
        freq_upper: f64,
    ) {
        self.meta.annotations.push(SigMfAnnotation {
            sample_start,
            sample_count,
            freq_lower_edge: Some(freq_lower),
            freq_upper_edge: Some(freq_upper),
            label: Some(label.to_string()),
            description: None,
        });
    }

    /// Set the global description field.
    pub fn set_description(&mut self, desc: &str) {
        self.meta.global.description = Some(desc.to_string());
    }

    /// Set the hardware field.
    pub fn set_hw(&mut self, hw: &str) {
        self.meta.global.hw = Some(hw.to_string());
    }

    /// Set the author field.
    pub fn set_author(&mut self, author: &str) {
        self.meta.global.author = Some(author.to_string());
    }

    /// Set the datetime on the first capture segment.
    pub fn set_datetime(&mut self, datetime: &str) {
        if let Some(cap) = self.meta.captures.first_mut() {
            cap.datetime = Some(datetime.to_string());
        }
    }

    /// Get the number of samples written so far.
    pub fn samples_written(&self) -> u64 {
        self.samples_written
    }

    /// Get a reference to the metadata (for inspection before finalize).
    pub fn meta(&self) -> &SigMfMeta {
        &self.meta
    }

    /// Finalize the recording: flush data, write metadata JSON.
    ///
    /// Returns the total number of samples written.
    pub fn finalize(mut self) -> Result<u64, WaveError> {
        // Flush binary data
        self.data_writer.flush()?;

        // Write metadata JSON
        let meta_path = self.base_path.with_extension("sigmf-meta");
        let meta_file = File::create(&meta_path)?;
        serde_json::to_writer_pretty(meta_file, &self.meta)
            .map_err(|e| WaveError::Config(format!("Failed to write SigMF meta: {e}")))?;

        Ok(self.samples_written)
    }
}

// ============================================================================
// SigMF Reader (for verification and ReplayDevice)
// ============================================================================

/// Read and parse a SigMF metadata file.
pub fn read_sigmf_meta(base_path: impl AsRef<Path>) -> Result<SigMfMeta, WaveError> {
    let meta_path = base_path.as_ref().with_extension("sigmf-meta");
    let file = File::open(&meta_path)?;
    let meta: SigMfMeta = serde_json::from_reader(file)
        .map_err(|e| WaveError::Config(format!("Invalid SigMF metadata: {e}")))?;
    Ok(meta)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn temp_base(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "waverunner_sigmf_{}_{}_{}",
            std::process::id(),
            id,
            name
        ))
    }

    fn cleanup(base: &Path) {
        std::fs::remove_file(base.with_extension("sigmf-data")).ok();
        std::fs::remove_file(base.with_extension("sigmf-meta")).ok();
    }

    #[test]
    fn sigmf_write_and_read_meta() {
        let base = temp_base("basic");
        let samples = vec![
            Sample::new(0.5, -0.5),
            Sample::new(1.0, 0.0),
            Sample::new(-0.25, 0.75),
        ];

        let mut writer = SigMfWriter::new(&base, 433.92e6, 2.048e6).unwrap();
        writer.set_description("Test recording");
        writer.set_hw("RTL-SDR v3");
        writer.write_samples(&samples).unwrap();
        assert_eq!(writer.samples_written(), 3);
        writer.finalize().unwrap();

        // Read back metadata
        let meta = read_sigmf_meta(&base).unwrap();
        assert_eq!(meta.global.datatype, "cf32_le");
        assert_eq!(meta.global.version, "1.0.0");
        assert_eq!(meta.global.sample_rate, Some(2.048e6));
        assert_eq!(meta.global.description.as_deref(), Some("Test recording"));
        assert_eq!(meta.global.hw.as_deref(), Some("RTL-SDR v3"));
        assert_eq!(meta.global.recorder.as_deref(), Some("waverunner"));
        assert_eq!(meta.captures.len(), 1);
        assert_eq!(meta.captures[0].sample_start, 0);
        assert_eq!(meta.captures[0].frequency, Some(433.92e6));

        // Read back binary data
        let data_path = base.with_extension("sigmf-data");
        let mut data = Vec::new();
        File::open(&data_path)
            .unwrap()
            .read_to_end(&mut data)
            .unwrap();
        assert_eq!(data.len(), 3 * 8); // 3 samples × 2 floats × 4 bytes

        // Verify sample values
        let re0 = f32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let im0 = f32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        assert!((re0 - 0.5).abs() < 1e-6);
        assert!((im0 - (-0.5)).abs() < 1e-6);

        cleanup(&base);
    }

    #[test]
    fn sigmf_annotations() {
        let base = temp_base("annot");
        let samples = vec![Sample::new(0.0, 0.0); 1000];

        let mut writer = SigMfWriter::new(&base, 929.6125e6, 2.048e6).unwrap();
        writer.write_samples(&samples).unwrap();
        writer.add_annotation(100, 200, "POCSAG burst");
        writer.add_annotation_with_freq(500, 100, "Signal", 929.0e6, 930.0e6);
        writer.finalize().unwrap();

        let meta = read_sigmf_meta(&base).unwrap();
        assert_eq!(meta.annotations.len(), 2);
        assert_eq!(meta.annotations[0].sample_start, 100);
        assert_eq!(meta.annotations[0].sample_count, 200);
        assert_eq!(meta.annotations[0].label.as_deref(), Some("POCSAG burst"));
        assert_eq!(meta.annotations[1].freq_lower_edge, Some(929.0e6));
        assert_eq!(meta.annotations[1].freq_upper_edge, Some(930.0e6));

        cleanup(&base);
    }

    #[test]
    fn sigmf_multiple_captures() {
        let base = temp_base("multicap");
        let samples = vec![Sample::new(0.0, 0.0); 500];

        let mut writer = SigMfWriter::new(&base, 100.0e6, 2.048e6).unwrap();
        writer.write_samples(&samples).unwrap();
        // Frequency change mid-recording (e.g., during scan)
        writer.add_capture(200.0e6);
        writer.write_samples(&samples).unwrap();
        writer.add_capture(300.0e6);
        writer.write_samples(&samples).unwrap();
        writer.finalize().unwrap();

        let meta = read_sigmf_meta(&base).unwrap();
        assert_eq!(meta.captures.len(), 3);
        assert_eq!(meta.captures[0].sample_start, 0);
        assert_eq!(meta.captures[0].frequency, Some(100.0e6));
        assert_eq!(meta.captures[1].sample_start, 500);
        assert_eq!(meta.captures[1].frequency, Some(200.0e6));
        assert_eq!(meta.captures[2].sample_start, 1000);
        assert_eq!(meta.captures[2].frequency, Some(300.0e6));

        cleanup(&base);
    }

    #[test]
    fn sigmf_datetime() {
        let base = temp_base("datetime");
        let mut writer = SigMfWriter::new(&base, 100e6, 1e6).unwrap();
        writer.set_datetime("2026-02-14T12:00:00Z");
        writer.write_samples(&[Sample::new(0.0, 0.0)]).unwrap();
        writer.finalize().unwrap();

        let meta = read_sigmf_meta(&base).unwrap();
        assert_eq!(
            meta.captures[0].datetime.as_deref(),
            Some("2026-02-14T12:00:00Z")
        );

        cleanup(&base);
    }

    #[test]
    fn sigmf_meta_roundtrip_json() {
        // Verify that SigMfMeta serializes and deserializes correctly
        let meta = SigMfMeta {
            global: SigMfGlobal {
                datatype: "cf32_le".to_string(),
                version: "1.0.0".to_string(),
                sample_rate: Some(2.048e6),
                description: Some("Test".to_string()),
                author: Some("WaveRunner".to_string()),
                hw: None,
                recorder: None,
                sha512: None,
                num_channels: None,
            },
            captures: vec![SigMfCapture {
                sample_start: 0,
                frequency: Some(433.92e6),
                datetime: Some("2026-01-01T00:00:00Z".to_string()),
            }],
            annotations: vec![SigMfAnnotation {
                sample_start: 10,
                sample_count: 100,
                freq_lower_edge: Some(433.0e6),
                freq_upper_edge: Some(434.0e6),
                label: Some("Signal".to_string()),
                description: Some("A signal".to_string()),
            }],
        };

        let json = serde_json::to_string_pretty(&meta).unwrap();
        let parsed: SigMfMeta = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.global.datatype, "cf32_le");
        assert_eq!(parsed.global.version, "1.0.0");
        assert_eq!(parsed.global.sample_rate, Some(2.048e6));
        assert_eq!(parsed.global.author.as_deref(), Some("WaveRunner"));
        assert_eq!(parsed.captures.len(), 1);
        assert_eq!(parsed.captures[0].frequency, Some(433.92e6));
        assert_eq!(parsed.annotations.len(), 1);
        assert_eq!(parsed.annotations[0].label.as_deref(), Some("Signal"));
        assert_eq!(
            parsed.annotations[0].description.as_deref(),
            Some("A signal")
        );

        // Verify None fields are not serialized
        assert!(!json.contains("core:hw"));
        assert!(!json.contains("core:sha512"));
    }

    #[test]
    fn sigmf_empty_recording() {
        let base = temp_base("empty");
        let writer = SigMfWriter::new(&base, 100e6, 1e6).unwrap();
        assert_eq!(writer.samples_written(), 0);
        let count = writer.finalize().unwrap();
        assert_eq!(count, 0);

        // Data file should exist but be empty
        let data_path = base.with_extension("sigmf-data");
        let data = std::fs::read(&data_path).unwrap();
        assert_eq!(data.len(), 0);

        // Meta should still be valid
        let meta = read_sigmf_meta(&base).unwrap();
        assert_eq!(meta.global.datatype, "cf32_le");

        cleanup(&base);
    }
}
