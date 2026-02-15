//! Session management types for the WaveRunner SDR engine.
//!
//! The session layer provides a unified command/event protocol that
//! decouples frontends (CLI, TUI) from the SDR processing pipeline.
//!
//! ```text
//! Frontend ──Command──→ SessionManager ──Event──→ Frontend
//!                            │
//!                    ┌───────┴───────┐
//!                    │               │
//!               HW Thread      Processing Thread
//!                    │               │
//!              start_rx()    DC→FFT→CFAR→Stats
//!                                    │
//!                              Decoder Threads
//!                              (bounded channels)
//! ```

pub mod manager;
pub mod replay;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use crate::dsp::detection::Detection;
use crate::dsp::statistics::SignalStats;
use crate::hardware::GainMode;

/// Commands sent from frontend to SessionManager.
#[derive(Debug)]
pub enum Command {
    /// Change center frequency.
    Tune(f64),
    /// Change gain mode.
    SetGain(GainMode),
    /// Change sample rate.
    SetSampleRate(f64),
    /// Start recording IQ to file.
    StartRecord {
        path: PathBuf,
        format: RecordFormat,
    },
    /// Stop recording.
    StopRecord,
    /// Enable a named decoder (spawns decoder thread).
    EnableDecoder(String),
    /// Disable a named decoder (stops decoder thread).
    DisableDecoder(String),
    /// Start audio demodulation.
    StartDemod(DemodConfig),
    /// Stop audio demodulation.
    StopDemod,
    /// Graceful shutdown.
    Shutdown,
}

/// Recording format.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecordFormat {
    /// Raw interleaved f32 IQ.
    RawCf32,
    /// 2-channel float WAV.
    Wav,
    /// SigMF (cf32_le data + JSON meta).
    SigMf,
}

/// Events emitted from SessionManager to frontend.
#[derive(Debug)]
pub enum Event {
    /// New spectrum data available.
    SpectrumReady(SpectrumFrame),
    /// CFAR detections from latest block.
    Detections(Vec<Detection>),
    /// Session statistics update.
    Stats(SessionStats),
    /// Decoded protocol message from a decoder plugin.
    DecodedMessage(DecodedMessage),
    /// Status update (informational).
    Status(StatusUpdate),
    /// Visualization data from demodulator chain.
    DemodVis(DemodVisData),
    /// Error from processing or hardware.
    Error(String),
}

/// Bundled spectrum and signal analysis for one processing block.
#[derive(Debug, Clone)]
pub struct SpectrumFrame {
    /// Power spectrum in dBFS, FFT-shifted (DC centered).
    pub spectrum_db: Vec<f32>,
    /// Estimated noise floor in dB.
    pub noise_floor_db: f32,
    /// RMS power in dBFS.
    pub rms_dbfs: f32,
    /// M2M4 SNR estimate in dB.
    pub snr_db: f32,
    /// Spectral flatness (Wiener entropy), 0.0 = tone, 1.0 = noise.
    pub spectral_flatness: f32,
    /// Full signal statistics (kurtosis, crest factor, etc).
    pub signal_stats: SignalStats,
    /// AGC gain in dB (when demod active).
    pub agc_gain_db: f32,
    /// Cumulative block count.
    pub block_count: u64,
}

/// Session performance and status statistics.
#[derive(Debug, Clone)]
pub struct SessionStats {
    /// Total sample blocks processed.
    pub blocks_processed: u64,
    /// Blocks dropped due to overflow.
    pub blocks_dropped: u64,
    /// Processing time for last block in microseconds.
    pub processing_time_us: u64,
    /// Moving average throughput in MS/s.
    pub throughput_msps: f64,
    /// CPU load estimate: processing_time / block_duration.
    pub cpu_load_percent: f32,
}

/// A decoded protocol message from a decoder plugin.
#[derive(Debug, Clone)]
pub struct DecodedMessage {
    /// Name of the decoder that produced this message.
    pub decoder: String,
    /// Timestamp when the message was decoded.
    pub timestamp: Instant,
    /// One-line human-readable summary.
    pub summary: String,
    /// Structured key-value fields for display/logging.
    pub fields: BTreeMap<String, String>,
    /// Optional raw bit payload.
    pub raw_bits: Option<Vec<u8>>,
}

/// Visualization data from the demodulation chain.
#[derive(Debug, Clone)]
pub struct DemodVisData {
    /// Recent IQ constellation points (post-AGC, pre-demod).
    /// Decimated to ~256 points per block.
    pub constellation: Vec<(f32, f32)>,
    /// PLL phase error in radians.
    pub pll_phase_error: f32,
    /// PLL frequency estimate in Hz.
    pub pll_frequency_hz: f64,
    /// Whether PLL/carrier is locked.
    pub pll_locked: bool,
    /// AGC gain in dB.
    pub agc_gain_db: f32,
    /// Demod mode name.
    pub mode: String,
}

/// Informational status update.
#[derive(Debug, Clone)]
pub enum StatusUpdate {
    /// Hardware connected and streaming.
    Streaming,
    /// Recording started.
    RecordingStarted(PathBuf),
    /// Recording stopped with total samples written.
    RecordingStopped(u64),
    /// Decoder enabled.
    DecoderEnabled(String),
    /// Decoder disabled.
    DecoderDisabled(String),
    /// Frequency changed.
    FrequencyChanged(f64),
    /// Gain changed.
    GainChanged(GainMode),
}

/// Configuration for starting audio demodulation.
#[derive(Debug, Clone)]
pub struct DemodConfig {
    /// Demod mode: "am", "am-sync", "fm", "wfm", "wfm-stereo", "usb", "lsb", "cw"
    pub mode: String,
    /// Desired audio output sample rate in Hz.
    pub audio_rate: u32,
    /// Channel bandwidth override in Hz.
    pub bandwidth: Option<f64>,
    /// BFO offset in Hz (for SSB/CW).
    pub bfo: Option<f64>,
    /// Squelch threshold in dBFS.
    pub squelch: Option<f64>,
    /// De-emphasis time constant in μs.
    pub deemph_us: Option<f64>,
    /// Output WAV file for recording demodulated audio.
    pub output_wav: Option<PathBuf>,
}

/// Configuration for creating a SessionManager.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Hardware device index.
    pub device_index: u32,
    /// Initial center frequency in Hz.
    pub frequency: f64,
    /// Sample rate in S/s.
    pub sample_rate: f64,
    /// Gain mode.
    pub gain: GainMode,
    /// PPM frequency correction.
    pub ppm: i32,
    /// FFT size for spectrum analysis.
    pub fft_size: usize,
    /// CFAR false alarm probability.
    pub pfa: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_message_fields() {
        let msg = DecodedMessage {
            decoder: "test".to_string(),
            timestamp: Instant::now(),
            summary: "Test message".to_string(),
            fields: BTreeMap::from([
                ("key1".to_string(), "value1".to_string()),
                ("key2".to_string(), "value2".to_string()),
            ]),
            raw_bits: None,
        };
        assert_eq!(msg.decoder, "test");
        assert_eq!(msg.fields.len(), 2);
        assert_eq!(msg.fields["key1"], "value1");
    }

    #[test]
    fn session_config_defaults() {
        let config = SessionConfig {
            device_index: 0,
            frequency: 100e6,
            sample_rate: 2.048e6,
            gain: GainMode::Auto,
            ppm: 0,
            fft_size: 2048,
            pfa: 1e-4,
        };
        assert_eq!(config.fft_size, 2048);
        assert_eq!(config.gain, GainMode::Auto);
    }

    #[test]
    fn record_format_variants() {
        assert_ne!(RecordFormat::RawCf32, RecordFormat::Wav);
        assert_ne!(RecordFormat::Wav, RecordFormat::SigMf);
    }

    #[test]
    fn demod_vis_data_construction() {
        let vis = DemodVisData {
            constellation: vec![(0.1, 0.2), (-0.3, 0.4)],
            pll_phase_error: 0.05,
            pll_frequency_hz: 19000.0,
            pll_locked: true,
            agc_gain_db: -12.5,
            mode: "WFM Stereo".to_string(),
        };
        assert_eq!(vis.constellation.len(), 2);
        assert!(vis.pll_locked);
        assert_eq!(vis.mode, "WFM Stereo");
    }

    #[test]
    fn demod_vis_event_variant() {
        let vis = DemodVisData {
            constellation: vec![],
            pll_phase_error: 0.0,
            pll_frequency_hz: 0.0,
            pll_locked: false,
            agc_gain_db: 0.0,
            mode: "AM".to_string(),
        };
        let event = Event::DemodVis(vis);
        assert!(matches!(event, Event::DemodVis(_)));
    }

    #[test]
    fn demod_vis_channel_round_trip() {
        use crossbeam_channel;
        let (tx, rx) = crossbeam_channel::bounded::<Event>(16);
        let vis = DemodVisData {
            constellation: vec![(1.0, -1.0); 256],
            pll_phase_error: 0.3,
            pll_frequency_hz: 500.0,
            pll_locked: true,
            agc_gain_db: -20.0,
            mode: "AM-Sync".to_string(),
        };
        tx.send(Event::DemodVis(vis)).unwrap();
        let received = rx.recv().unwrap();
        if let Event::DemodVis(v) = received {
            assert_eq!(v.constellation.len(), 256);
            assert!(v.pll_locked);
            assert_eq!(v.mode, "AM-Sync");
        } else {
            panic!("Expected DemodVis event");
        }
    }
}
