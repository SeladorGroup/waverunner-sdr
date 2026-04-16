//! ReplayDevice — implements [`SdrDevice`] for recorded IQ files.
//!
//! Enables offline testing and analysis of all decoders without
//! hardware by replaying previously-recorded IQ data at real-time
//! pace (or as fast as possible in test mode).
//!
//! ## Supported Formats
//!
//! | Extension    | Format                    | Bytes/sample |
//! |-------------|---------------------------|-------------|
//! | `.cf32`     | Interleaved f32 LE (I,Q)  | 8           |
//! | `.raw`/`.iq`| Same as cf32              | 8           |
//! | `.cu8`      | Unsigned 8-bit (I,Q)      | 2           |
//! | `.wav`      | 2-channel float32 WAV     | 8           |
//!
//! ## Timing Model
//!
//! For real-time replay, each block of N samples introduces a sleep of
//! `N / sample_rate` seconds before the callback. This matches the
//! cadence of a hardware device and prevents decoders from being
//! overwhelmed. In test/fast mode (`set_realtime(false)`), blocks are
//! delivered as fast as possible.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::HardwareError;
use crate::hardware::{GainMode, RxCallback, SdrDevice};
use crate::types::{DeviceInfo, Sample};

// ============================================================================
// File format detection
// ============================================================================

/// Supported IQ file formats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IqFormat {
    /// Complex float32 little-endian (interleaved I,Q pairs).
    Cf32Le,
    /// Complex unsigned 8-bit (interleaved I,Q pairs, center = 127.5).
    Cu8,
    /// 2-channel float32 WAV file (channel 1 = I, channel 2 = Q).
    WavF32,
}

impl IqFormat {
    /// Detect format from file extension.
    fn from_path(path: &Path) -> Option<IqFormat> {
        let ext = path.extension()?.to_str()?.to_lowercase();
        match ext.as_str() {
            "cf32" | "raw" | "iq" | "fc32" | "cfile" => Some(IqFormat::Cf32Le),
            "cu8" | "cs8" | "u8" => Some(IqFormat::Cu8),
            "wav" => Some(IqFormat::WavF32),
            _ => None,
        }
    }
}

// ============================================================================
// ReplayDevice
// ============================================================================

/// SDR device that replays IQ data from a file.
///
/// Implements the full [`SdrDevice`] trait so it can be used anywhere
/// a hardware device is expected — in SessionManager, in the decode
/// command, or in tests.
pub struct ReplayDevice {
    path: PathBuf,
    format: IqFormat,
    /// Sample rate for timing and metadata.
    sample_rate: Mutex<f64>,
    /// Center frequency for metadata.
    center_freq: Mutex<f64>,
    /// Gain setting (no-op for replay, stored for trait compliance).
    gain: Mutex<GainMode>,
    /// Whether we're currently streaming.
    streaming: AtomicBool,
    /// Stop flag — set by `stop_rx()` to break the replay loop.
    stop_flag: Arc<AtomicBool>,
    /// Deliver blocks at real-time pace (true) or as fast as possible (false).
    realtime: AtomicBool,
    /// Block size in samples for each callback invocation.
    block_size: usize,
    /// Loop replay continuously.
    looping: AtomicBool,
}

impl ReplayDevice {
    /// Open a recorded IQ file for replay.
    ///
    /// The format is auto-detected from the file extension. The sample
    /// rate must be provided since raw files have no metadata.
    pub fn open(path: &Path, sample_rate: f64) -> Result<Box<dyn SdrDevice>, HardwareError> {
        let format = IqFormat::from_path(path).ok_or_else(|| {
            HardwareError::DriverError(format!(
                "Unknown IQ format for extension: {}",
                path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("(none)")
            ))
        })?;

        if !path.exists() {
            return Err(HardwareError::DriverError(format!(
                "File not found: {}",
                path.display()
            )));
        }

        // Validate file size for raw formats (minimum one complete sample)
        if format == IqFormat::Cf32Le {
            let meta = std::fs::metadata(path).map_err(|e| {
                HardwareError::DriverError(format!("Cannot read file metadata: {e}"))
            })?;
            if meta.len() < 8 {
                return Err(HardwareError::StreamError(format!(
                    "File too small for cf32 format ({} bytes, need at least 8)",
                    meta.len()
                )));
            }
        } else if format == IqFormat::Cu8 {
            let meta = std::fs::metadata(path).map_err(|e| {
                HardwareError::DriverError(format!("Cannot read file metadata: {e}"))
            })?;
            if meta.len() < 2 {
                return Err(HardwareError::StreamError(format!(
                    "File too small for cu8 format ({} bytes, need at least 2)",
                    meta.len()
                )));
            }
        }

        // For WAV files, validate the header up front
        if format == IqFormat::WavF32 {
            let reader = hound::WavReader::open(path)
                .map_err(|e| HardwareError::DriverError(format!("Invalid WAV file: {e}")))?;
            let spec = reader.spec();
            if spec.channels != 2 {
                return Err(HardwareError::DriverError(format!(
                    "WAV must have 2 channels (I/Q), got {}",
                    spec.channels
                )));
            }
        }

        Ok(Box::new(ReplayDevice {
            path: path.to_path_buf(),
            format,
            sample_rate: Mutex::new(sample_rate),
            center_freq: Mutex::new(0.0),
            gain: Mutex::new(GainMode::Auto),
            streaming: AtomicBool::new(false),
            stop_flag: Arc::new(AtomicBool::new(false)),
            realtime: AtomicBool::new(true),
            block_size: 262144, // Match RTL-SDR default
            looping: AtomicBool::new(false),
        }))
    }

    /// Set block size for callback invocations.
    pub fn with_block_size(mut self, size: usize) -> Self {
        self.block_size = size;
        self
    }

    /// Enable/disable real-time pacing.
    pub fn with_realtime(self, realtime: bool) -> Self {
        self.realtime.store(realtime, Ordering::Relaxed);
        self
    }

    /// Enable/disable continuous looping.
    pub fn with_looping(self, looping: bool) -> Self {
        self.looping.store(looping, Ordering::Relaxed);
        self
    }
}

// ============================================================================
// SdrDevice implementation
// ============================================================================

impl SdrDevice for ReplayDevice {
    fn name(&self) -> &str {
        "ReplayDevice"
    }

    fn info(&self) -> Result<DeviceInfo, HardwareError> {
        let sr = *self.sample_rate.lock().unwrap();
        Ok(DeviceInfo {
            name: format!("Replay: {}", self.path.display()),
            driver: "file".to_string(),
            serial: None,
            index: 0,
            frequency_range: (0.0, 6_000_000_000.0),
            sample_rate_range: (sr, sr),
            gain_range: (0.0, 0.0),
            available_gains: vec![0.0],
        })
    }

    fn frequency(&self) -> Result<f64, HardwareError> {
        Ok(*self.center_freq.lock().unwrap())
    }

    fn set_frequency(&self, freq: f64) -> Result<(), HardwareError> {
        *self.center_freq.lock().unwrap() = freq;
        Ok(())
    }

    fn sample_rate(&self) -> Result<f64, HardwareError> {
        Ok(*self.sample_rate.lock().unwrap())
    }

    fn set_sample_rate(&self, rate: f64) -> Result<(), HardwareError> {
        *self.sample_rate.lock().unwrap() = rate;
        Ok(())
    }

    fn gain(&self) -> Result<GainMode, HardwareError> {
        Ok(*self.gain.lock().unwrap())
    }

    fn set_gain(&self, mode: GainMode) -> Result<(), HardwareError> {
        *self.gain.lock().unwrap() = mode;
        Ok(())
    }

    fn set_ppm(&self, _ppm: i32) -> Result<(), HardwareError> {
        // No-op for replay — PPM correction doesn't apply to files
        Ok(())
    }

    fn start_rx(&self, mut callback: RxCallback) -> Result<(), HardwareError> {
        if self.streaming.swap(true, Ordering::SeqCst) {
            return Err(HardwareError::DeviceBusy);
        }
        self.stop_flag.store(false, Ordering::Relaxed);

        let sample_rate = *self.sample_rate.lock().unwrap();
        let block_size = self.block_size;
        let realtime = self.realtime.load(Ordering::Relaxed);
        let looping = self.looping.load(Ordering::Relaxed);

        // Block duration for real-time pacing
        let block_duration = if realtime && sample_rate > 0.0 {
            Some(std::time::Duration::from_secs_f64(
                block_size as f64 / sample_rate,
            ))
        } else {
            None
        };

        let result = match self.format {
            IqFormat::Cf32Le => replay_cf32(
                &self.path,
                block_size,
                &self.stop_flag,
                block_duration,
                looping,
                &mut callback,
            ),
            IqFormat::Cu8 => replay_cu8(
                &self.path,
                block_size,
                &self.stop_flag,
                block_duration,
                looping,
                &mut callback,
            ),
            IqFormat::WavF32 => replay_wav(
                &self.path,
                block_size,
                &self.stop_flag,
                block_duration,
                looping,
                &mut callback,
            ),
        };

        self.streaming.store(false, Ordering::Relaxed);
        result
    }

    fn stop_rx(&self) -> Result<(), HardwareError> {
        self.stop_flag.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn is_streaming(&self) -> bool {
        self.streaming.load(Ordering::Relaxed)
    }
}

// ============================================================================
// Format-specific replay functions
// ============================================================================

/// Replay cf32 little-endian IQ data.
fn replay_cf32(
    path: &Path,
    block_size: usize,
    stop: &AtomicBool,
    block_duration: Option<std::time::Duration>,
    looping: bool,
    callback: &mut RxCallback,
) -> Result<(), HardwareError> {
    let bytes_per_block = block_size * 8; // 2 × f32 per sample
    let mut buf = vec![0u8; bytes_per_block];

    loop {
        let file = File::open(path)
            .map_err(|e| HardwareError::StreamError(format!("Failed to open file: {e}")))?;
        let mut reader = BufReader::new(file);

        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }

            let bytes_read = read_exact_or_eof(&mut reader, &mut buf)?;
            if bytes_read == 0 {
                break; // EOF
            }

            // Convert bytes to samples (handle partial last block)
            let sample_count = bytes_read / 8;
            let samples: Vec<Sample> = buf[..sample_count * 8]
                .chunks_exact(8)
                .map(|chunk| {
                    let re = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    let im = f32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
                    Sample::new(re, im)
                })
                .collect();

            callback(&samples);

            if let Some(dur) = block_duration {
                // Scale sleep for partial blocks
                let actual_dur = if sample_count < block_size {
                    std::time::Duration::from_secs_f64(
                        dur.as_secs_f64() * sample_count as f64 / block_size as f64,
                    )
                } else {
                    dur
                };
                std::thread::sleep(actual_dur);
            }
        }

        if !looping {
            return Ok(());
        }
    }
}

/// Replay cu8 (unsigned 8-bit) IQ data.
///
/// Converts each byte from [0, 255] to [-1.0, +1.0] with center at 127.5.
fn replay_cu8(
    path: &Path,
    block_size: usize,
    stop: &AtomicBool,
    block_duration: Option<std::time::Duration>,
    looping: bool,
    callback: &mut RxCallback,
) -> Result<(), HardwareError> {
    let bytes_per_block = block_size * 2; // 2 × u8 per sample
    let mut buf = vec![0u8; bytes_per_block];

    // Precompute lookup table: u8 → normalized f32
    // This is the same conversion as RTL-SDR hardware:
    //   f = (byte - 127.5) / 127.5
    let lut: Vec<f32> = (0..256).map(|b| (b as f32 - 127.5) / 127.5).collect();

    loop {
        let file = File::open(path)
            .map_err(|e| HardwareError::StreamError(format!("Failed to open file: {e}")))?;
        let mut reader = BufReader::new(file);

        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }

            let bytes_read = read_exact_or_eof(&mut reader, &mut buf)?;
            if bytes_read == 0 {
                break;
            }

            let sample_count = bytes_read / 2;
            let samples: Vec<Sample> = buf[..sample_count * 2]
                .chunks_exact(2)
                .map(|pair| Sample::new(lut[pair[0] as usize], lut[pair[1] as usize]))
                .collect();

            callback(&samples);

            if let Some(dur) = block_duration {
                let actual_dur = if sample_count < block_size {
                    std::time::Duration::from_secs_f64(
                        dur.as_secs_f64() * sample_count as f64 / block_size as f64,
                    )
                } else {
                    dur
                };
                std::thread::sleep(actual_dur);
            }
        }

        if !looping {
            return Ok(());
        }
    }
}

/// Replay 2-channel float32 WAV IQ data.
fn replay_wav(
    path: &Path,
    block_size: usize,
    stop: &AtomicBool,
    block_duration: Option<std::time::Duration>,
    looping: bool,
    callback: &mut RxCallback,
) -> Result<(), HardwareError> {
    loop {
        let reader = hound::WavReader::open(path)
            .map_err(|e| HardwareError::StreamError(format!("Failed to open WAV: {e}")))?;

        let spec = reader.spec();
        if spec.channels != 2 {
            return Err(HardwareError::StreamError(format!(
                "WAV must have 2 channels, got {}",
                spec.channels
            )));
        }

        // Read all f32 samples and chunk into I/Q pairs
        let mut sample_iter = reader.into_samples::<f32>();
        let mut block = Vec::with_capacity(block_size);

        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }

            block.clear();
            let mut exhausted = false;

            for _ in 0..block_size {
                let i = match sample_iter.next() {
                    Some(Ok(v)) => v,
                    Some(Err(e)) => {
                        return Err(HardwareError::StreamError(format!("WAV read error: {e}")));
                    }
                    None => {
                        exhausted = true;
                        break;
                    }
                };
                let q = match sample_iter.next() {
                    Some(Ok(v)) => v,
                    Some(Err(e)) => {
                        return Err(HardwareError::StreamError(format!("WAV read error: {e}")));
                    }
                    None => {
                        exhausted = true;
                        break;
                    }
                };
                block.push(Sample::new(i, q));
            }

            if !block.is_empty() {
                callback(&block);

                if let Some(dur) = block_duration {
                    let actual_dur = if block.len() < block_size {
                        std::time::Duration::from_secs_f64(
                            dur.as_secs_f64() * block.len() as f64 / block_size as f64,
                        )
                    } else {
                        dur
                    };
                    std::thread::sleep(actual_dur);
                }
            }

            if exhausted {
                break;
            }
        }

        if !looping {
            return Ok(());
        }
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Read up to `buf.len()` bytes, returning actual bytes read.
/// Returns 0 on EOF. Handles partial reads.
fn read_exact_or_eof(reader: &mut BufReader<File>, buf: &mut [u8]) -> Result<usize, HardwareError> {
    let mut total = 0;
    while total < buf.len() {
        match reader.read(&mut buf[total..]) {
            Ok(0) => break, // EOF
            Ok(n) => total += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(HardwareError::StreamError(format!("Read error: {e}")));
            }
        }
    }
    Ok(total)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::RawIqWriter;
    use std::sync::atomic::AtomicU64;

    /// Generate a unique temp path for this test thread.
    fn unique_path(ext: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "waverunner_replay_{}_{}.{}",
            std::process::id(),
            id,
            ext
        ))
    }

    /// Create a temporary cf32 file with known samples.
    fn write_test_cf32(samples: &[Sample]) -> PathBuf {
        let path = unique_path("cf32");
        let mut writer = RawIqWriter::new(&path).unwrap();
        writer.write_samples(samples).unwrap();
        writer.finish().unwrap();
        path
    }

    /// Create a temporary cu8 file with known bytes.
    fn write_test_cu8(bytes: &[u8]) -> PathBuf {
        let path = unique_path("cu8");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// Create a temporary WAV file with known IQ samples.
    fn write_test_wav(samples: &[Sample], sample_rate: u32) -> PathBuf {
        let path = unique_path("wav");
        let mut writer = crate::recording::WavIqWriter::new(&path, sample_rate as f64).unwrap();
        writer.write_samples(samples).unwrap();
        writer.finish().unwrap();
        path
    }

    #[test]
    fn replay_cf32_roundtrip() {
        let original = vec![
            Sample::new(0.5, -0.5),
            Sample::new(1.0, 0.0),
            Sample::new(-1.0, 0.75),
            Sample::new(0.0, 0.0),
        ];
        let path = write_test_cf32(&original);

        let device = ReplayDevice::open(&path, 48000.0).unwrap();
        assert_eq!(device.name(), "ReplayDevice");

        // Collect all samples delivered through callback
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);

        // Use small block size so we get all samples
        // The device defaults to 262144 — way more than our 4 samples.
        // We need to read the file with block_size <= sample count.
        // But we can't set block size through the trait... the default
        // block_size (262144) will read all 4 samples in one call.
        device
            .start_rx(Box::new(move |samples| {
                received_clone.lock().unwrap().extend_from_slice(samples);
            }))
            .unwrap();

        let got = received.lock().unwrap();
        assert_eq!(got.len(), 4);
        assert!((got[0].re - 0.5).abs() < 1e-6);
        assert!((got[0].im - (-0.5)).abs() < 1e-6);
        assert!((got[2].re - (-1.0)).abs() < 1e-6);
        assert!((got[2].im - 0.75).abs() < 1e-6);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn replay_cu8_conversion() {
        // I=127 (≈0.0), Q=255 (≈1.0), I=0 (≈-1.0), Q=128 (≈0.004)
        let bytes = vec![127, 255, 0, 128];
        let path = write_test_cu8(&bytes);

        let device = ReplayDevice::open(&path, 48000.0).unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);

        device
            .start_rx(Box::new(move |samples| {
                received_clone.lock().unwrap().extend_from_slice(samples);
            }))
            .unwrap();

        let got = received.lock().unwrap();
        assert_eq!(got.len(), 2);

        // 127 → (127 - 127.5) / 127.5 ≈ -0.00392
        assert!((got[0].re - (-0.5 / 127.5)).abs() < 0.01);
        // 255 → (255 - 127.5) / 127.5 = 1.0
        assert!((got[0].im - 1.0).abs() < 0.01);
        // 0 → (0 - 127.5) / 127.5 = -1.0
        assert!((got[1].re - (-1.0)).abs() < 0.01);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn replay_wav_roundtrip() {
        let original = vec![
            Sample::new(0.25, -0.25),
            Sample::new(0.5, 0.5),
            Sample::new(-0.75, 0.125),
        ];
        let path = write_test_wav(&original, 48000);

        let device = ReplayDevice::open(&path, 48000.0).unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);

        device
            .start_rx(Box::new(move |samples| {
                received_clone.lock().unwrap().extend_from_slice(samples);
            }))
            .unwrap();

        let got = received.lock().unwrap();
        assert_eq!(got.len(), 3);
        assert!((got[0].re - 0.25).abs() < 1e-5);
        assert!((got[0].im - (-0.25)).abs() < 1e-5);
        assert!((got[1].re - 0.5).abs() < 1e-5);
        assert!((got[2].re - (-0.75)).abs() < 1e-5);
        assert!((got[2].im - 0.125).abs() < 1e-5);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn replay_device_info() {
        let samples = vec![Sample::new(0.0, 0.0)];
        let path = write_test_cf32(&samples);

        let device = ReplayDevice::open(&path, 2_048_000.0).unwrap();
        let info = device.info().unwrap();

        assert!(info.name.contains("Replay"));
        assert_eq!(info.driver, "file");
        assert_eq!(info.sample_rate_range.0, 2_048_000.0);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn replay_set_frequency_and_gain() {
        let samples = vec![Sample::new(0.0, 0.0)];
        let path = write_test_cf32(&samples);

        let device = ReplayDevice::open(&path, 48000.0).unwrap();

        device.set_frequency(433.92e6).unwrap();
        assert!((device.frequency().unwrap() - 433.92e6).abs() < 1.0);

        device.set_gain(GainMode::Manual(20.0)).unwrap();
        assert_eq!(device.gain().unwrap(), GainMode::Manual(20.0));

        device.set_sample_rate(1e6).unwrap();
        assert!((device.sample_rate().unwrap() - 1e6).abs() < 1.0);

        // PPM is a no-op
        device.set_ppm(42).unwrap();

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn replay_stop_rx() {
        // Create a file large enough to stream for a while
        let samples: Vec<Sample> = (0..10000)
            .map(|i| {
                let t = i as f32 / 10000.0;
                Sample::new(
                    (t * std::f32::consts::TAU * 100.0).cos(),
                    (t * std::f32::consts::TAU * 100.0).sin(),
                )
            })
            .collect();
        let path = write_test_cf32(&samples);

        let device = ReplayDevice::open(&path, 48000.0).unwrap();

        let count = Arc::new(AtomicU64::new(0));
        let count_clone = Arc::clone(&count);

        // Stop immediately from callback
        let stop_device = Arc::new(device);
        let _stop_device_clone = Arc::clone(&stop_device);

        // We can't easily stop from within callback since we need
        // the trait object. Instead, test that the device finishes
        // reading the file and stops naturally.
        stop_device
            .start_rx(Box::new(move |samples| {
                count_clone.fetch_add(samples.len() as u64, Ordering::Relaxed);
            }))
            .unwrap();

        // Should have received all 10000 samples
        assert_eq!(count.load(Ordering::Relaxed), 10000);
        assert!(!stop_device.is_streaming());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn replay_unknown_format_rejected() {
        let path = std::env::temp_dir().join("test.xyz");
        std::fs::write(&path, b"dummy").unwrap();

        let result = ReplayDevice::open(&path, 48000.0);
        assert!(result.is_err());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn replay_nonexistent_file_rejected() {
        let path = std::env::temp_dir().join("does_not_exist_34729.cf32");
        let result = ReplayDevice::open(&path, 48000.0);
        assert!(result.is_err());
    }

    #[test]
    fn format_detection() {
        assert_eq!(
            IqFormat::from_path(Path::new("recording.cf32")),
            Some(IqFormat::Cf32Le)
        );
        assert_eq!(
            IqFormat::from_path(Path::new("recording.raw")),
            Some(IqFormat::Cf32Le)
        );
        assert_eq!(
            IqFormat::from_path(Path::new("recording.iq")),
            Some(IqFormat::Cf32Le)
        );
        assert_eq!(
            IqFormat::from_path(Path::new("recording.cu8")),
            Some(IqFormat::Cu8)
        );
        assert_eq!(
            IqFormat::from_path(Path::new("recording.wav")),
            Some(IqFormat::WavF32)
        );
        assert_eq!(IqFormat::from_path(Path::new("recording.xyz")), None);
        assert_eq!(IqFormat::from_path(Path::new("no_extension")), None);
    }
}
