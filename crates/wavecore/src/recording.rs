use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use hound::{SampleFormat, WavSpec, WavWriter};
use serde::{Deserialize, Serialize};

use crate::error::WaveError;
use crate::types::{Sample, SampleRate};

fn default_schema_v1() -> u32 {
    1
}

/// IQ recording metadata sidecar (written as JSON alongside recordings).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingMetadata {
    /// Schema version for forward compatibility.
    #[serde(default = "default_schema_v1")]
    pub schema_version: u32,
    pub center_freq: f64,
    pub sample_rate: f64,
    pub gain: String,
    /// Sample format: "cf32" (complex float32), "cu8" (complex uint8)
    pub format: String,
    /// ISO 8601 timestamp
    pub timestamp: String,
    pub duration_secs: Option<f64>,
    pub device: String,
    pub samples_written: u64,
}

impl RecordingMetadata {
    /// Write metadata as a JSON sidecar file next to the recording.
    pub fn write_sidecar(&self, recording_path: &Path) -> Result<(), WaveError> {
        let sidecar_path = recording_path.with_extension("json");
        let file = File::create(sidecar_path)?;
        serde_json::to_writer_pretty(file, self)
            .map_err(|e| WaveError::Config(format!("failed to write metadata: {e}")))?;
        Ok(())
    }
}

/// Raw IQ file writer — interleaved f32 pairs, no header.
///
/// This is the standard SDR recording format (.raw / .iq / .cf32).
/// Compatible with GNU Radio, inspectrum, and other tools.
pub struct RawIqWriter {
    writer: BufWriter<File>,
    samples_written: u64,
}

impl RawIqWriter {
    pub fn new(path: &Path) -> Result<Self, WaveError> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            samples_written: 0,
        })
    }

    /// Write a buffer of IQ samples.
    pub fn write_samples(&mut self, samples: &[Sample]) -> Result<(), WaveError> {
        for s in samples {
            self.writer.write_all(&s.re.to_le_bytes())?;
            self.writer.write_all(&s.im.to_le_bytes())?;
        }
        self.samples_written += samples.len() as u64;
        Ok(())
    }

    /// Flush and finalize the writer.
    pub fn finish(mut self) -> Result<u64, WaveError> {
        self.writer.flush()?;
        Ok(self.samples_written)
    }

    pub fn samples_written(&self) -> u64 {
        self.samples_written
    }
}

/// WAV IQ file writer — 2-channel float32 WAV.
///
/// Compatible with Audacity, GNU Radio, SDR#, and other tools.
/// Channel 1 = I (in-phase), Channel 2 = Q (quadrature).
pub struct WavIqWriter {
    writer: WavWriter<BufWriter<File>>,
    samples_written: u64,
}

impl WavIqWriter {
    pub fn new(path: &Path, sample_rate: SampleRate) -> Result<Self, WaveError> {
        let spec = WavSpec {
            channels: 2,
            sample_rate: sample_rate as u32,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };

        let file = File::create(path)?;
        let writer = WavWriter::new(BufWriter::new(file), spec)
            .map_err(|e| WaveError::Config(format!("failed to create WAV writer: {e}")))?;

        Ok(Self {
            writer,
            samples_written: 0,
        })
    }

    /// Write a buffer of IQ samples as interleaved I/Q channels.
    pub fn write_samples(&mut self, samples: &[Sample]) -> Result<(), WaveError> {
        for s in samples {
            self.writer
                .write_sample(s.re)
                .map_err(|e| WaveError::Config(format!("WAV write error: {e}")))?;
            self.writer
                .write_sample(s.im)
                .map_err(|e| WaveError::Config(format!("WAV write error: {e}")))?;
        }
        self.samples_written += samples.len() as u64;
        Ok(())
    }

    /// Finalize the WAV file (writes header with correct length).
    pub fn finish(self) -> Result<u64, WaveError> {
        let count = self.samples_written;
        self.writer
            .finalize()
            .map_err(|e| WaveError::Config(format!("WAV finalize error: {e}")))?;
        Ok(count)
    }

    pub fn samples_written(&self) -> u64 {
        self.samples_written
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn raw_iq_roundtrip() {
        let dir = std::env::temp_dir().join("waverunner_test_raw");
        let path = dir.with_extension("raw");

        let samples = vec![
            Sample::new(0.5, -0.5),
            Sample::new(1.0, 0.0),
            Sample::new(-1.0, 1.0),
        ];

        // Write
        let mut writer = RawIqWriter::new(&path).unwrap();
        writer.write_samples(&samples).unwrap();
        assert_eq!(writer.samples_written(), 3);
        writer.finish().unwrap();

        // Read back
        let mut file = File::open(&path).unwrap();
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).unwrap();
        assert_eq!(buf.len(), 3 * 2 * 4); // 3 samples * 2 floats * 4 bytes

        // Parse back
        let mut read_samples = Vec::new();
        for chunk in buf.chunks_exact(8) {
            let re = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            let im = f32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
            read_samples.push(Sample::new(re, im));
        }
        assert_eq!(read_samples.len(), 3);
        assert!((read_samples[0].re - 0.5).abs() < 1e-6);
        assert!((read_samples[0].im - (-0.5)).abs() < 1e-6);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn wav_iq_write_and_verify() {
        let dir = std::env::temp_dir().join("waverunner_test_wav");
        let path = dir.with_extension("wav");

        let samples = vec![Sample::new(0.25, -0.25), Sample::new(0.5, 0.5)];

        let mut writer = WavIqWriter::new(&path, 2_048_000.0).unwrap();
        writer.write_samples(&samples).unwrap();
        assert_eq!(writer.samples_written(), 2);
        writer.finish().unwrap();

        // Verify with hound reader
        let reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, 2_048_000);
        assert_eq!(spec.bits_per_sample, 32);
        assert_eq!(spec.sample_format, SampleFormat::Float);

        let wav_samples: Vec<f32> = reader.into_samples::<f32>().map(|s| s.unwrap()).collect();
        // 2 IQ samples = 4 WAV samples (I0, Q0, I1, Q1)
        assert_eq!(wav_samples.len(), 4);
        assert!((wav_samples[0] - 0.25).abs() < 1e-6);
        assert!((wav_samples[1] - (-0.25)).abs() < 1e-6);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn metadata_sidecar() {
        let dir = std::env::temp_dir().join("waverunner_test_meta");
        let path = dir.with_extension("raw");

        let meta = RecordingMetadata {
            schema_version: 1,
            center_freq: 433.92e6,
            sample_rate: 2.048e6,
            gain: "auto".to_string(),
            format: "cf32".to_string(),
            timestamp: "2026-02-14T12:00:00Z".to_string(),
            duration_secs: Some(10.0),
            device: "RTL-SDR".to_string(),
            samples_written: 1000,
        };

        // Create the recording file first so the sidecar path works
        File::create(&path).unwrap();
        meta.write_sidecar(&path).unwrap();

        let sidecar_path = path.with_extension("json");
        let content = std::fs::read_to_string(&sidecar_path).unwrap();
        assert!(content.contains("433920000"));

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&sidecar_path).ok();
    }
}
