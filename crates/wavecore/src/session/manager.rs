//! SessionManager — unified SDR processing engine.
//!
//! Wraps the existing sample pipeline, hardware driver, and DSP chain
//! into a single command/event-driven interface. Replaces duplicated
//! pipeline logic across CLI commands and TUI.
//!
//! ## Architecture
//!
//! ```text
//! Frontend ──Command──→ SessionManager ──Event──→ Frontend
//!                            │
//!                    ┌───────┴───────┐
//!                    │               │
//!               HW Thread      Processing Thread
//!               (start_rx)    (select! on samples + cmds)
//!                                    │
//!                    ┌───────────────┼───────────────┐
//!                    │               │               │
//!              Decoder Threads   Recording       Demod Chain
//!              (bounded chans)   (IQ→file)    (DDC→AGC→Demod→Audio)
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};

use crate::analysis;
use crate::analysis::tracking::SignalTracker;
use crate::buffer::{PipelineConfig, SampleConsumer, sample_pipeline};
use crate::dsp::decoder::{DecoderHandle, DecoderRegistry};
use crate::dsp::detection::{
    CfarConfig, CfarMethod, cfar_detect, db_to_linear, noise_floor_sigma_clip, spectral_flatness,
};
use crate::dsp::estimation::snr_m2m4;
use crate::dsp::fft::SpectrumAnalyzer;
use crate::dsp::power::rms_power_dbfs;
use crate::dsp::preprocess::DcRemover;
use crate::dsp::statistics::signal_statistics;
use crate::hardware::rtlsdr::RtlSdrDevice;
use crate::hardware::{DeviceEnumerator, SdrDevice};
use crate::recording::{RawIqWriter, WavIqWriter};
use crate::session::checkpoint::{self, CHECKPOINT_SCHEMA_VERSION, SessionCheckpoint};
use crate::session::timeline::{AnnotationKind, SessionTimeline, TimelineEntry};
use crate::session::{
    Command, DecodedMessage, Event, HealthStatus, LatencyBreakdown, RecordFormat, SessionConfig,
    SessionStats, SpectrumFrame, StatusUpdate, TimelineExportFormat,
};
#[cfg(feature = "audio")]
use crate::session::{DemodConfig, DemodVisData};
use crate::sigmf::SigMfWriter;
use crate::types::{Sample, SampleBlock};

// DDC is used unconditionally for decoder sample-rate routing
use crate::dsp::ddc::Ddc;

// Demod chain imports (audio feature only)
#[cfg(feature = "audio")]
use crate::audio::AudioSink;
#[cfg(feature = "audio")]
use crate::dsp::agc::Agc;
#[cfg(feature = "audio")]
use crate::dsp::demod::am::{AmDemod, AmMode};
#[cfg(feature = "audio")]
use crate::dsp::demod::cw::CwDemod;
#[cfg(feature = "audio")]
use crate::dsp::demod::fm::{FmDemod, FmMode};
#[cfg(feature = "audio")]
use crate::dsp::demod::ssb::{Sideband, SsbDemod};
#[cfg(feature = "audio")]
use crate::dsp::demod::{Demodulator, mode_defaults};
#[cfg(feature = "audio")]
use crate::dsp::resample::PolyphaseResampler;

// ============================================================================
// Load shedding
// ============================================================================

/// Adaptive load shedder that monitors pipeline buffer occupancy and reduces
/// DSP work under sustained load. Three levels:
///
/// - Level 0 (normal): All processing every block.
/// - Level 1 (light, ≥25% fill): Spectrum/CFAR every 2nd block, decoders every block.
/// - Level 2 (heavy, ≥50% fill): Spectrum/CFAR every 4th block, decoders every 2nd block.
struct LoadShedder {
    capacity: usize,
    /// Current shedding level (0=normal, 1=light, 2=heavy). Read by health computation.
    level: u8,
    prev_level: u8,
}

impl LoadShedder {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            level: 0,
            prev_level: 0,
        }
    }

    /// Update shedding level based on current buffer occupancy.
    /// Returns `Some(level)` if the level changed (for status event emission).
    fn update(&mut self, buffer_len: usize) -> Option<u8> {
        let fill = buffer_len as f32 / self.capacity as f32;
        self.level = if fill >= 0.50 {
            2
        } else if fill >= 0.25 {
            1
        } else {
            0
        };

        if self.level != self.prev_level {
            let changed = self.level;
            self.prev_level = self.level;
            if self.level > 0 {
                tracing::warn!(
                    level = self.level,
                    fill_pct = fill * 100.0,
                    "Load shedding activated"
                );
            } else {
                tracing::info!("Load shedding deactivated — pipeline caught up");
            }
            Some(changed)
        } else {
            None
        }
    }

    /// Whether to run expensive DSP (FFT, CFAR, flatness, SNR, stats) this block.
    fn run_spectrum(&self, block_count: u64) -> bool {
        match self.level {
            0 => true,
            1 => block_count % 2 == 0,
            _ => block_count % 4 == 0,
        }
    }

    /// Whether to feed decoders this block.
    fn feed_decoders(&self, block_count: u64) -> bool {
        match self.level {
            0 | 1 => true,
            _ => block_count % 2 == 0,
        }
    }
}

/// Try-send an event, incrementing a drop counter on failure.
fn try_send_event(tx: &Sender<Event>, event: Event, drops: &AtomicU64) {
    if tx.try_send(event).is_err() {
        drops.fetch_add(1, Ordering::Relaxed);
    }
}

// ============================================================================
// Recording types
// ============================================================================

/// Recording writer abstraction over raw, WAV, and SigMF formats.
enum RecWriter {
    Raw(RawIqWriter),
    Wav(WavIqWriter),
    SigMf(Box<SigMfWriter>),
}

impl RecWriter {
    fn is_raw(&self) -> bool {
        matches!(self, RecWriter::Raw(_))
    }

    fn write_samples(&mut self, samples: &[Sample]) -> Result<(), String> {
        match self {
            RecWriter::Raw(w) => w.write_samples(samples).map_err(|e| e.to_string()),
            RecWriter::Wav(w) => w.write_samples(samples).map_err(|e| e.to_string()),
            RecWriter::SigMf(w) => w.write_samples(samples).map_err(|e| e.to_string()),
        }
    }

    fn finish(self) -> Result<u64, String> {
        match self {
            RecWriter::Raw(w) => w.finish().map_err(|e| e.to_string()),
            RecWriter::Wav(w) => w.finish().map_err(|e| e.to_string()),
            RecWriter::SigMf(w) => w.finalize().map_err(|e| e.to_string()),
        }
    }
}

/// Best-effort write of a .sigmf-meta sidecar for raw cf32 recordings.
fn write_sigmf_sidecar(path: &std::path::Path, center_freq: f64, sample_rate: f64) {
    let meta = crate::sigmf::SigMfMeta {
        global: crate::sigmf::SigMfGlobal {
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
        captures: vec![crate::sigmf::SigMfCapture {
            sample_start: 0,
            frequency: Some(center_freq),
            datetime: None,
        }],
        annotations: Vec::new(),
    };

    let meta_path = path.with_extension("sigmf-meta");
    match std::fs::File::create(&meta_path) {
        Ok(file) => {
            if let Err(e) = serde_json::to_writer_pretty(file, &meta) {
                tracing::warn!("Failed to write SigMF sidecar: {e}");
            }
        }
        Err(e) => {
            tracing::warn!(
                "Failed to create SigMF sidecar {}: {e}",
                meta_path.display()
            );
        }
    }
}

/// Active recording state in the processing loop.
struct RecordingState {
    writer: RecWriter,
    samples_written: u64,
    rec_path: PathBuf,
}

impl RecordingState {
    fn path(&self) -> &std::path::Path {
        &self.rec_path
    }
}

// ============================================================================
// Demod types (requires audio feature)
// ============================================================================

#[cfg(feature = "audio")]
enum AudioResampler {
    Mono(PolyphaseResampler),
    Stereo {
        left: PolyphaseResampler,
        right: PolyphaseResampler,
    },
}

#[cfg(feature = "audio")]
struct DemodState {
    ddc: Ddc,
    agc: Agc,
    demod: Box<dyn Demodulator>,
    resampler: Option<AudioResampler>,
    audio_sink: AudioSink,
    wav_writer: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>,
    total_audio_samples: u64,
}

#[cfg(feature = "audio")]
impl AudioResampler {
    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        match self {
            AudioResampler::Mono(resampler) => {
                let iq: Vec<Sample> = input.iter().map(|&s| Sample::new(s, 0.0)).collect();
                let resampled = resampler.process(&iq);
                resampled.into_iter().map(|s| s.re).collect()
            }
            AudioResampler::Stereo { left, right } => {
                let frames = input.len() / 2;
                let mut left_in = Vec::with_capacity(frames);
                let mut right_in = Vec::with_capacity(frames);

                for frame in input.chunks_exact(2) {
                    left_in.push(Sample::new(frame[0], 0.0));
                    right_in.push(Sample::new(frame[1], 0.0));
                }

                let left_out = left.process(&left_in);
                let right_out = right.process(&right_in);
                let frames_out = left_out.len().min(right_out.len());
                let mut interleaved = Vec::with_capacity(frames_out * 2);

                for idx in 0..frames_out {
                    interleaved.push(left_out[idx].re);
                    interleaved.push(right_out[idx].re);
                }

                interleaved
            }
        }
    }
}

/// Build a demodulation chain from configuration.
///
/// Creates DDC → AGC → Demodulator → Resampler → AudioSink.
#[cfg(feature = "audio")]
fn build_demod_state(config: &DemodConfig, sample_rate: f64) -> Result<DemodState, String> {
    let defaults = mode_defaults(&config.mode)
        .ok_or_else(|| format!("Unknown demod mode: {}", config.mode))?;

    let bw = config.bandwidth.unwrap_or(defaults.channel_bw);
    let deemph = config.deemph_us.unwrap_or(defaults.deemph_us);
    let ddc_rate = defaults.ddc_output_rate;

    let ddc = Ddc::new(0.0, sample_rate, ddc_rate, bw);
    let agc = Agc::new(-20.0, 0.001, 0.1, ddc_rate);

    let mut demod: Box<dyn Demodulator> = match config.mode.as_str() {
        "am" => Box::new(AmDemod::new(AmMode::Envelope, ddc_rate, bw / 2.0)),
        "am-sync" => Box::new(AmDemod::new(AmMode::Synchronous, ddc_rate, bw / 2.0)),
        "fm" => Box::new(FmDemod::new(FmMode::Narrow, ddc_rate, deemph)),
        "wfm" => Box::new(FmDemod::new(FmMode::Wide, ddc_rate, deemph)),
        "wfm-stereo" => Box::new(FmDemod::new(FmMode::WideStereo, ddc_rate, deemph)),
        "usb" => {
            let bfo = config.bfo.unwrap_or(1500.0);
            Box::new(SsbDemod::new(Sideband::Upper, ddc_rate, bfo, bw))
        }
        "lsb" => {
            let bfo = config.bfo.unwrap_or(1500.0);
            Box::new(SsbDemod::new(Sideband::Lower, ddc_rate, bfo, bw))
        }
        "cw" => {
            let bfo = config.bfo.unwrap_or(700.0);
            let cw_bw = config.bandwidth.unwrap_or(200.0);
            Box::new(CwDemod::new(ddc_rate, bfo, cw_bw))
        }
        _ => return Err(format!("Unknown demod mode: {}", config.mode)),
    };

    if let Some(sq) = config.squelch {
        demod.set_parameter("squelch", sq).ok();
    }

    let source_channels = if config.mode == "wfm-stereo" { 2 } else { 1 };
    let audio_sink = AudioSink::new(config.audio_rate, source_channels)
        .map_err(|e| format!("Failed to create audio sink: {e}"))?;

    let demod_out_rate = demod.sample_rate_out();
    let sink_rate = audio_sink.sample_rate() as f64;
    let resampler = if (demod_out_rate - sink_rate).abs() > 1.0 {
        let out_rate = audio_sink.sample_rate() as usize;
        let in_rate = demod_out_rate as usize;
        Some(if source_channels == 2 {
            AudioResampler::Stereo {
                left: PolyphaseResampler::new(out_rate, in_rate, 128, 0.0),
                right: PolyphaseResampler::new(out_rate, in_rate, 128, 0.0),
            }
        } else {
            AudioResampler::Mono(PolyphaseResampler::new(out_rate, in_rate, 128, 0.0))
        })
    } else {
        None
    };

    let wav_writer = if let Some(ref path) = config.output_wav {
        let spec = hound::WavSpec {
            channels: source_channels as u16,
            sample_rate: audio_sink.sample_rate(),
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        Some(
            hound::WavWriter::create(path, spec)
                .map_err(|e| format!("Failed to create WAV: {e}"))?,
        )
    } else {
        None
    };

    Ok(DemodState {
        ddc,
        agc,
        demod,
        resampler,
        audio_sink,
        wav_writer,
        total_audio_samples: 0,
    })
}

// ============================================================================
// SessionManager
// ============================================================================

/// Unified SDR processing engine.
///
/// Owns the hardware device, sample pipeline, and DSP processing thread.
/// Frontends interact exclusively through commands and events.
pub struct SessionManager {
    /// Send commands to the processing thread.
    cmd_tx: Sender<Command>,
    /// Running flag shared with all threads.
    running: Arc<AtomicBool>,
    /// Thread handles for cleanup.
    hw_handle: Option<JoinHandle<()>>,
    proc_handle: Option<JoinHandle<()>>,
    /// Hardware device (for direct control like set_frequency).
    device: Arc<Box<dyn SdrDevice>>,
}

impl SessionManager {
    /// Create a new SessionManager with the default RTL-SDR device.
    ///
    /// Returns the manager and a receiver for events. The caller reads
    /// events from the receiver in their main loop.
    pub fn new(
        config: SessionConfig,
        decoder_registry: DecoderRegistry,
    ) -> Result<(Self, Receiver<Event>), String> {
        let device = RtlSdrDevice::open(config.device_index)
            .map_err(|e| format!("Failed to open device: {e}"))?;
        Self::new_with_device(config, device, decoder_registry)
    }

    /// Create a new SessionManager with a caller-provided device.
    ///
    /// This enables replay mode (via `ReplayDevice`) and testing
    /// without hardware. The device is configured with the session's
    /// frequency, sample rate, gain, and PPM settings before streaming
    /// begins.
    pub fn new_with_device(
        config: SessionConfig,
        device: Box<dyn SdrDevice>,
        decoder_registry: DecoderRegistry,
    ) -> Result<(Self, Receiver<Event>), String> {
        let device = Arc::new(device);

        device
            .set_frequency(config.frequency)
            .map_err(|e| format!("Failed to set frequency: {e}"))?;
        device
            .set_sample_rate(config.sample_rate)
            .map_err(|e| format!("Failed to set sample rate: {e}"))?;
        device
            .set_gain(config.gain)
            .map_err(|e| format!("Failed to set gain: {e}"))?;
        if config.ppm != 0 {
            device
                .set_ppm(config.ppm)
                .map_err(|e| format!("Failed to set PPM: {e}"))?;
        }

        // Channels
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<Command>(64);
        let (evt_tx, evt_rx) = crossbeam_channel::bounded::<Event>(256);
        let (producer, consumer) = sample_pipeline(PipelineConfig::default());
        let dropped_counter = producer.dropped_counter();
        let running = Arc::new(AtomicBool::new(true));

        // Shared atomics for HW callback to read current freq/rate (lock-free)
        let shared_freq = Arc::new(AtomicU64::new(config.frequency.to_bits()));
        let shared_rate = Arc::new(AtomicU64::new(config.sample_rate.to_bits()));
        let hw_freq = Arc::clone(&shared_freq);
        let hw_rate = Arc::clone(&shared_rate);

        // Hardware reader thread
        let device_clone = Arc::clone(&device);
        let running_hw = Arc::clone(&running);
        let hw_handle = std::thread::Builder::new()
            .name("session-hw".to_string())
            .spawn(move || {
                let mut sequence = 0u64;
                let start = Instant::now();
                let _ = device_clone.start_rx(Box::new(move |samples| {
                    if !running_hw.load(Ordering::Relaxed) {
                        return;
                    }
                    let block = SampleBlock {
                        samples: samples.to_vec(),
                        center_freq: f64::from_bits(hw_freq.load(Ordering::Relaxed)),
                        sample_rate: f64::from_bits(hw_rate.load(Ordering::Relaxed)),
                        sequence,
                        timestamp_ns: start.elapsed().as_nanos() as u64,
                    };
                    let _ = producer.send(block);
                    sequence += 1;
                }));
            })
            .map_err(|e| format!("Failed to spawn HW thread: {e}"))?;

        // Processing thread
        let running_proc = Arc::clone(&running);
        let device_proc = Arc::clone(&device);
        let proc_handle = std::thread::Builder::new()
            .name("session-proc".to_string())
            .spawn(move || {
                run_processing_loop(
                    consumer,
                    cmd_rx,
                    evt_tx,
                    device_proc,
                    config,
                    decoder_registry,
                    running_proc,
                    dropped_counter,
                    shared_freq,
                    shared_rate,
                );
            })
            .map_err(|e| format!("Failed to spawn processing thread: {e}"))?;

        Ok((
            Self {
                cmd_tx,
                running,
                hw_handle: Some(hw_handle),
                proc_handle: Some(proc_handle),
                device,
            },
            evt_rx,
        ))
    }

    /// Send a command to the processing thread.
    pub fn send(&self, cmd: Command) -> Result<(), String> {
        self.cmd_tx
            .send(cmd)
            .map_err(|e| format!("Command channel closed: {e}"))
    }

    /// Check if the session is still running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Get a reference to the running flag for Ctrl+C handlers.
    pub fn running_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.running)
    }

    /// Get a cloned command sender for use in signal handlers.
    pub fn cmd_sender(&self) -> Sender<Command> {
        self.cmd_tx.clone()
    }

    /// Get a reference to the device for direct queries.
    pub fn device(&self) -> &dyn SdrDevice {
        self.device.as_ref().as_ref()
    }

    /// Graceful shutdown.
    pub fn shutdown(mut self) {
        self.running.store(false, Ordering::Relaxed);
        // Send shutdown command (best effort)
        self.cmd_tx.send(Command::Shutdown).ok();
        // Stop hardware
        self.device.stop_rx().ok();
        // Wait for threads
        if let Some(h) = self.hw_handle.take() {
            h.join().ok();
        }
        if let Some(h) = self.proc_handle.take() {
            h.join().ok();
        }
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self.device.stop_rx().ok();
    }
}

// ============================================================================
// Processing loop
// ============================================================================

/// Main processing loop — runs in the processing thread.
///
/// Factored-out DSP chain handling spectrum analysis, CFAR detection,
/// recording, demodulation, and protocol decoding. Commands are processed
/// non-blocking between sample blocks.
#[allow(clippy::too_many_arguments)]
fn run_processing_loop(
    consumer: SampleConsumer,
    cmd_rx: Receiver<Command>,
    evt_tx: Sender<Event>,
    device: Arc<Box<dyn SdrDevice>>,
    mut config: SessionConfig,
    decoder_registry: DecoderRegistry,
    running: Arc<AtomicBool>,
    dropped_counter: Arc<AtomicU64>,
    shared_freq: Arc<AtomicU64>,
    shared_rate: Arc<AtomicU64>,
) {
    // DSP components
    let mut dc_remover = DcRemover::from_cutoff(100.0, config.sample_rate);
    let mut analyzer = match SpectrumAnalyzer::new(config.fft_size) {
        Ok(a) => a,
        Err(e) => {
            evt_tx
                .send(Event::Error(format!("Invalid FFT size: {e}")))
                .ok();
            return;
        }
    };

    let cfar_config = CfarConfig {
        method: CfarMethod::CellAveraging,
        num_reference: 24,
        num_guard: 4,
        threshold_factor: CfarConfig::from_pfa(config.pfa, &CfarMethod::CellAveraging, 24),
    };

    let mut block_count = 0u64;

    // Decoded message channel — decoders send here
    let (decoded_tx, decoded_rx) = crossbeam_channel::unbounded::<DecodedMessage>();

    // Active decoder handles
    let mut active_decoders: Vec<DecoderHandle> = Vec::new();

    // Recording state
    let mut recording: Option<RecordingState> = None;

    // Demod state (requires audio feature)
    #[cfg(feature = "audio")]
    let mut demod_state: Option<DemodState> = None;
    #[cfg(feature = "audio")]
    let mut active_demod_config: Option<DemodConfig> = None;

    // Analysis state
    let mut tracker: Option<SignalTracker> = None;
    let mut reference_spectrum: Option<Vec<f32>> = None;
    let mut last_spectrum_db: Vec<f32> = Vec::new();
    let mut last_samples: Vec<Sample> = Vec::new();
    let loop_start = Instant::now();

    // Backpressure: load shedder + dropped-event counter
    let buffer_capacity = PipelineConfig::default().buffer_depth;
    let mut load_shedder = LoadShedder::new(buffer_capacity);
    let events_dropped = AtomicU64::new(0);

    // Session timeline for event logging + annotations
    let mut timeline = SessionTimeline::new();
    let mut prev_health = HealthStatus::Normal;

    while running.load(Ordering::Relaxed) {
        // ----------------------------------------------------------------
        // Process commands (non-blocking)
        // ----------------------------------------------------------------
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Command::Tune(freq) => {
                    if let Err(e) = device.set_frequency(freq) {
                        try_send_event(
                            &evt_tx,
                            Event::Error(format!("Failed to tune: {e}")),
                            &events_dropped,
                        );
                    } else {
                        config.frequency = freq;
                        shared_freq.store(freq.to_bits(), Ordering::Relaxed);
                        timeline.log_event(TimelineEntry::FreqChange {
                            timestamp_s: timeline.elapsed_s(),
                            freq_hz: freq,
                        });
                        try_send_event(
                            &evt_tx,
                            Event::Status(StatusUpdate::FrequencyChanged(freq)),
                            &events_dropped,
                        );
                    }
                }
                Command::SetGain(mode) => {
                    if let Err(e) = device.set_gain(mode) {
                        try_send_event(
                            &evt_tx,
                            Event::Error(format!("Failed to set gain: {e}")),
                            &events_dropped,
                        );
                    } else {
                        timeline.log_event(TimelineEntry::GainChange {
                            timestamp_s: timeline.elapsed_s(),
                            gain: format!("{mode:?}"),
                        });
                        try_send_event(
                            &evt_tx,
                            Event::Status(StatusUpdate::GainChanged(mode)),
                            &events_dropped,
                        );
                    }
                }
                Command::SetSampleRate(rate) => {
                    if let Err(e) = device.set_sample_rate(rate) {
                        try_send_event(
                            &evt_tx,
                            Event::Error(format!("Failed to set sample rate: {e}")),
                            &events_dropped,
                        );
                    } else {
                        config.sample_rate = rate;
                        shared_rate.store(rate.to_bits(), Ordering::Relaxed);
                        dc_remover = DcRemover::from_cutoff(100.0, rate);
                        // Rebuild active demod chain with new sample rate
                        #[cfg(feature = "audio")]
                        if let Some(ref demod_cfg) = active_demod_config {
                            // Finalize old WAV writer before rebuilding
                            if let Some(mut old) = demod_state.take() {
                                if let Some(writer) = old.wav_writer.take() {
                                    writer.finalize().ok();
                                }
                            }
                            match build_demod_state(demod_cfg, rate) {
                                Ok(state) => {
                                    demod_state = Some(state);
                                }
                                Err(e) => {
                                    try_send_event(
                                        &evt_tx,
                                        Event::Error(format!(
                                            "Failed to rebuild demod after rate change: {e}"
                                        )),
                                        &events_dropped,
                                    );
                                    active_demod_config = None;
                                }
                            }
                        }
                    }
                }
                Command::EnableDecoder(name) => {
                    // Prevent duplicate decoder instances
                    if active_decoders.iter().any(|h| h.name() == name) {
                        try_send_event(
                            &evt_tx,
                            Event::Status(StatusUpdate::DecoderEnabled(name)),
                            &events_dropped,
                        );
                    } else if let Some(decoder) = decoder_registry.create(&name) {
                        // Check if the decoder needs a DDC for sample rate conversion.
                        // Decoders declare their required sample rate via requirements().
                        // If it differs from the hardware rate, create a DDC to decimate.
                        let reqs = decoder.requirements();
                        let hw_rate = f64::from_bits(shared_rate.load(Ordering::Relaxed));
                        let hw_freq = f64::from_bits(shared_freq.load(Ordering::Relaxed));
                        let rate_ratio = hw_rate / reqs.sample_rate;

                        let ddc = if rate_ratio > 1.5 {
                            // Decoder needs decimation — create a DDC chain.
                            // center_frequency == 0.0 means "same as hardware" (no shift).
                            let freq_offset = if reqs.center_frequency == 0.0 {
                                0.0
                            } else {
                                reqs.center_frequency - hw_freq
                            };
                            let ddc =
                                Ddc::new(freq_offset, hw_rate, reqs.sample_rate, reqs.bandwidth);
                            tracing::info!(
                                "Decoder '{}' needs {}→{} Hz — DDC created (offset {:.0} Hz, {:.1}x decimation)",
                                name,
                                hw_rate,
                                reqs.sample_rate,
                                freq_offset,
                                rate_ratio
                            );
                            Some(ddc)
                        } else {
                            // Decoder operates at native hardware rate (e.g., ADS-B at 2 MS/s)
                            None
                        };

                        let handle =
                            DecoderHandle::spawn_with_ddc(decoder, decoded_tx.clone(), 32, ddc);
                        if handle.is_alive() {
                            timeline.log_event(TimelineEntry::DecoderEnabled {
                                timestamp_s: timeline.elapsed_s(),
                                name: name.clone(),
                            });
                            try_send_event(
                                &evt_tx,
                                Event::Status(StatusUpdate::DecoderEnabled(name)),
                                &events_dropped,
                            );
                            active_decoders.push(handle);
                        } else {
                            try_send_event(
                                &evt_tx,
                                Event::Error(format!("Failed to start decoder thread: {name}")),
                                &events_dropped,
                            );
                        }
                    } else {
                        try_send_event(
                            &evt_tx,
                            Event::Error(format!("Unknown decoder: {name}")),
                            &events_dropped,
                        );
                    }
                }
                Command::DisableDecoder(name) => {
                    let idx = active_decoders.iter().position(|h| h.name() == name);
                    if let Some(idx) = idx {
                        let handle = active_decoders.remove(idx);
                        handle.stop();
                        timeline.log_event(TimelineEntry::DecoderDisabled {
                            timestamp_s: timeline.elapsed_s(),
                            name: name.clone(),
                        });
                        try_send_event(
                            &evt_tx,
                            Event::Status(StatusUpdate::DecoderDisabled(name)),
                            &events_dropped,
                        );
                    }
                }

                // Recording commands
                Command::StartRecord { path, format } => {
                    let result = match format {
                        RecordFormat::RawCf32 => RawIqWriter::new(&path)
                            .map(RecWriter::Raw)
                            .map_err(|e| format!("{e}")),
                        RecordFormat::Wav => WavIqWriter::new(&path, config.sample_rate)
                            .map(RecWriter::Wav)
                            .map_err(|e| format!("{e}")),
                        RecordFormat::SigMf => {
                            SigMfWriter::new(&path, config.frequency, config.sample_rate)
                                .map(|w| RecWriter::SigMf(Box::new(w)))
                                .map_err(|e| format!("{e}"))
                        }
                    };
                    match result {
                        Ok(writer) => {
                            recording = Some(RecordingState {
                                writer,
                                samples_written: 0,
                                rec_path: path.clone(),
                            });
                            timeline.log_event(TimelineEntry::RecordStart {
                                timestamp_s: timeline.elapsed_s(),
                                path: path.display().to_string(),
                            });
                            try_send_event(
                                &evt_tx,
                                Event::Status(StatusUpdate::RecordingStarted(path)),
                                &events_dropped,
                            );
                        }
                        Err(e) => {
                            try_send_event(
                                &evt_tx,
                                Event::Error(format!("Failed to start recording: {e}")),
                                &events_dropped,
                            );
                        }
                    }
                }
                Command::StopRecord => {
                    if let Some(rec) = recording.take() {
                        let was_raw = rec.writer.is_raw();
                        let rec_path = rec.rec_path.clone();
                        let total = match rec.writer.finish() {
                            Ok(n) => n,
                            Err(e) => {
                                tracing::error!("Recording finalize error: {e}");
                                try_send_event(
                                    &evt_tx,
                                    Event::Error(format!("Recording finalize error: {e}")),
                                    &events_dropped,
                                );
                                rec.samples_written
                            }
                        };
                        if was_raw {
                            write_sigmf_sidecar(&rec_path, config.frequency, config.sample_rate);
                        }
                        timeline.log_event(TimelineEntry::RecordStop {
                            timestamp_s: timeline.elapsed_s(),
                            samples: total,
                        });
                        try_send_event(
                            &evt_tx,
                            Event::Status(StatusUpdate::RecordingStopped(total)),
                            &events_dropped,
                        );
                    }
                }

                // Demod commands
                Command::StartDemod(demod_config) => {
                    #[cfg(feature = "audio")]
                    {
                        match build_demod_state(&demod_config, config.sample_rate) {
                            Ok(state) => {
                                demod_state = Some(state);
                                active_demod_config = Some(demod_config);
                                try_send_event(
                                    &evt_tx,
                                    Event::Status(StatusUpdate::Streaming),
                                    &events_dropped,
                                );
                            }
                            Err(e) => {
                                try_send_event(
                                    &evt_tx,
                                    Event::Error(format!("Failed to start demod: {e}")),
                                    &events_dropped,
                                );
                            }
                        }
                    }
                    #[cfg(not(feature = "audio"))]
                    {
                        let _ = demod_config;
                        try_send_event(
                            &evt_tx,
                            Event::Error("Demod requires the 'audio' feature".to_string()),
                            &events_dropped,
                        );
                    }
                }
                Command::StopDemod => {
                    #[cfg(feature = "audio")]
                    {
                        if let Some(mut state) = demod_state.take() {
                            // Finalize WAV if recording audio
                            if let Some(writer) = state.wav_writer.take() {
                                writer.finalize().ok();
                            }
                        }
                        active_demod_config = None;
                    }
                }
                Command::SetVolume(vol) => {
                    #[cfg(feature = "audio")]
                    if let Some(ref ds) = demod_state {
                        ds.audio_sink.set_volume(vol);
                    }
                    #[cfg(not(feature = "audio"))]
                    let _ = vol;
                }

                // Analysis commands
                Command::RunAnalysis { id, request } => {
                    let result = match request {
                        analysis::AnalysisRequest::MeasureSignal(measure_config) => {
                            let report = analysis::measurement::measure_signal(
                                &last_spectrum_db,
                                &measure_config,
                                config.sample_rate,
                            );
                            analysis::AnalysisResult::Measurement(report)
                        }
                        analysis::AnalysisRequest::AnalyzeBurst(burst_config) => {
                            let report =
                                analysis::burst::analyze_bursts(&last_samples, &burst_config);
                            analysis::AnalysisResult::Burst(report)
                        }
                        analysis::AnalysisRequest::EstimateModulation(mod_config) => {
                            let report = analysis::modulation::estimate_modulation(
                                &last_samples,
                                &mod_config,
                            );
                            analysis::AnalysisResult::Modulation(report)
                        }
                        analysis::AnalysisRequest::CompareSpectra => {
                            if let Some(ref reference) = reference_spectrum {
                                let report = analysis::comparison::compare_spectra(
                                    &analysis::comparison::CompareConfig {
                                        reference: reference.clone(),
                                        current: last_spectrum_db.clone(),
                                        sample_rate: config.sample_rate,
                                        threshold_db: 6.0,
                                    },
                                );
                                analysis::AnalysisResult::Comparison(report)
                            } else {
                                try_send_event(
                                    &evt_tx,
                                    Event::Error("No reference spectrum captured".to_string()),
                                    &events_dropped,
                                );
                                continue;
                            }
                        }
                        analysis::AnalysisRequest::InspectBitstream(bs_config) => {
                            let report = analysis::bitstream::analyze_bitstream(&bs_config);
                            analysis::AnalysisResult::Bitstream(report)
                        }
                        analysis::AnalysisRequest::TrackingSnapshot => {
                            if let Some(ref t) = tracker {
                                analysis::AnalysisResult::Tracking(t.snapshot())
                            } else {
                                try_send_event(
                                    &evt_tx,
                                    Event::Error("Tracking not active".to_string()),
                                    &events_dropped,
                                );
                                continue;
                            }
                        }
                        analysis::AnalysisRequest::Export(mut export_config) => {
                            // Substitute empty spectrum with latest data
                            if let analysis::export::ExportContent::Spectrum {
                                ref mut spectrum_db,
                                ref mut sample_rate,
                                ref mut center_freq,
                            } = export_config.content
                            {
                                if spectrum_db.is_empty() {
                                    *spectrum_db = last_spectrum_db.clone();
                                    *sample_rate = config.sample_rate;
                                    *center_freq = config.frequency;
                                }
                            }
                            match analysis::export::export_to_file(&export_config) {
                                Ok(path) => analysis::AnalysisResult::ExportComplete {
                                    path,
                                    format: format!("{:?}", export_config.format),
                                },
                                Err(e) => {
                                    try_send_event(
                                        &evt_tx,
                                        Event::Error(format!("Export failed: {e}")),
                                        &events_dropped,
                                    );
                                    continue;
                                }
                            }
                        }
                    };
                    // AnalysisResult is rare and important — use blocking send
                    // so it is never silently dropped by backpressure.
                    evt_tx.send(Event::AnalysisResult { id, result }).ok();
                }
                Command::CaptureReference => {
                    if last_spectrum_db.is_empty() {
                        try_send_event(
                            &evt_tx,
                            Event::Error("Cannot capture reference: no spectrum data yet".into()),
                            &events_dropped,
                        );
                    } else {
                        reference_spectrum = Some(last_spectrum_db.clone());
                        try_send_event(
                            &evt_tx,
                            Event::Status(StatusUpdate::AnalysisReferenceCapture),
                            &events_dropped,
                        );
                    }
                }
                Command::StartTracking => {
                    tracker = Some(SignalTracker::new(1800)); // 30 min at 1 Hz
                    try_send_event(
                        &evt_tx,
                        Event::Status(StatusUpdate::TrackingStarted),
                        &events_dropped,
                    );
                }
                Command::StopTracking => {
                    tracker = None;
                    try_send_event(
                        &evt_tx,
                        Event::Status(StatusUpdate::TrackingStopped),
                        &events_dropped,
                    );
                }
                Command::Export(mut export_config) => {
                    // Substitute empty spectrum with latest data
                    if let analysis::export::ExportContent::Spectrum {
                        ref mut spectrum_db,
                        ref mut sample_rate,
                        ref mut center_freq,
                    } = export_config.content
                    {
                        if spectrum_db.is_empty() {
                            *spectrum_db = last_spectrum_db.clone();
                            *sample_rate = config.sample_rate;
                            *center_freq = config.frequency;
                        }
                    }
                    match analysis::export::export_to_file(&export_config) {
                        Ok(path) => {
                            try_send_event(
                                &evt_tx,
                                Event::AnalysisResult {
                                    id: 0,
                                    result: analysis::AnalysisResult::ExportComplete {
                                        path,
                                        format: format!("{:?}", export_config.format),
                                    },
                                },
                                &events_dropped,
                            );
                        }
                        Err(e) => {
                            try_send_event(
                                &evt_tx,
                                Event::Error(format!("Export failed: {e}")),
                                &events_dropped,
                            );
                        }
                    }
                }

                Command::AddAnnotation { kind, text } => {
                    let ann_kind = match kind.as_str() {
                        "note" => AnnotationKind::Note,
                        "tag" => AnnotationKind::Tag,
                        _ => AnnotationKind::Bookmark,
                    };
                    let id = timeline.add_annotation(ann_kind, text, config.frequency);
                    try_send_event(&evt_tx, Event::AnnotationAdded(id), &events_dropped);
                }
                Command::ExportTimeline { path, format } => {
                    let result = match format {
                        TimelineExportFormat::Json => timeline.export_json(&path),
                        TimelineExportFormat::Csv => timeline.export_csv(&path),
                    };
                    match result {
                        Ok(out_path) => {
                            try_send_event(
                                &evt_tx,
                                Event::Status(StatusUpdate::RecordingStopped(0)),
                                &events_dropped,
                            );
                            tracing::info!("Timeline exported to {out_path}");
                        }
                        Err(e) => {
                            try_send_event(
                                &evt_tx,
                                Event::Error(format!("Timeline export failed: {e}")),
                                &events_dropped,
                            );
                        }
                    }
                }

                Command::Shutdown => {
                    running.store(false, Ordering::Relaxed);
                    break;
                }
            }
        }

        if !running.load(Ordering::Relaxed) {
            break;
        }

        // Forward decoded messages to frontend (non-blocking)
        while let Ok(msg) = decoded_rx.try_recv() {
            try_send_event(&evt_tx, Event::DecodedMessage(msg), &events_dropped);
        }

        // ----------------------------------------------------------------
        // Process sample block
        // ----------------------------------------------------------------
        let block = match consumer.try_recv() {
            Some(b) => b,
            None => {
                std::thread::sleep(std::time::Duration::from_millis(2));
                continue;
            }
        };

        // Update load shedder from buffer occupancy
        let buffer_len = consumer.len();
        if let Some(level) = load_shedder.update(buffer_len) {
            timeline.log_event(TimelineEntry::LoadShedding {
                timestamp_s: timeline.elapsed_s(),
                level,
            });
            try_send_event(
                &evt_tx,
                Event::Status(StatusUpdate::LoadShedding(level)),
                &events_dropped,
            );
        }

        let proc_start = Instant::now();
        let mut samples = block.samples;

        // 1. DC removal (with latency timing)
        let dc_start = Instant::now();
        dc_remover.process(&mut samples);
        let dc_removal_us = dc_start.elapsed().as_micros() as u64;

        // 2. Recording — write raw IQ after DC removal
        if let Some(ref mut rec) = recording {
            if let Err(e) = rec.writer.write_samples(&samples) {
                try_send_event(
                    &evt_tx,
                    Event::Error(format!("Recording write error: {e}")),
                    &events_dropped,
                );
                // Stop recording on error
                if let Some(rec) = recording.take() {
                    let was_raw = rec.writer.is_raw();
                    let rec_path = rec.rec_path.clone();
                    let total = rec.writer.finish().unwrap_or(0);
                    if was_raw {
                        write_sigmf_sidecar(&rec_path, config.frequency, config.sample_rate);
                    }
                    try_send_event(
                        &evt_tx,
                        Event::Status(StatusUpdate::RecordingStopped(total)),
                        &events_dropped,
                    );
                }
            } else {
                rec.samples_written += samples.len() as u64;
            }
        }

        // 3. Demodulation chain (DDC → AGC → Demod → Resample → Audio)
        let demod_start = Instant::now();
        #[cfg(feature = "audio")]
        let agc_gain = if let Some(ref mut ds) = demod_state {
            let emit_visualization = active_demod_config
                .as_ref()
                .map(|cfg| cfg.emit_visualization)
                .unwrap_or(true);
            let baseband = ds.ddc.process(&samples);
            let mut agc_out = baseband;
            ds.agc.process(&mut agc_out);

            let audio = ds.demod.process(&agc_out);

            let sink_muted = ds.audio_sink.volume() <= f32::EPSILON;
            let should_render_audio = ds.wav_writer.is_some() || !sink_muted;
            if should_render_audio {
                let audio_out = if let Some(ref mut resampler) = ds.resampler {
                    resampler.process(&audio)
                } else {
                    audio
                };

                if !sink_muted {
                    ds.audio_sink.write(&audio_out);
                }

                ds.total_audio_samples += audio_out.len() as u64;

                if let Some(ref mut writer) = ds.wav_writer {
                    for &sample in &audio_out {
                        let s16 = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
                        writer.write_sample(s16).ok();
                    }
                }
            }

            if emit_visualization {
                let stride = (agc_out.len() / 256).max(1);
                let constellation: Vec<(f32, f32)> = agc_out
                    .iter()
                    .step_by(stride)
                    .take(256)
                    .map(|s| (s.re, s.im))
                    .collect();
                let vis = DemodVisData {
                    constellation,
                    pll_phase_error: ds.demod.phase_error(),
                    pll_frequency_hz: ds.demod.frequency_estimate_hz(),
                    pll_locked: ds.demod.is_locked(),
                    agc_gain_db: ds.agc.gain_db() as f32,
                    mode: ds.demod.name().to_string(),
                };
                try_send_event(&evt_tx, Event::DemodVis(vis), &events_dropped);
            }

            ds.agc.gain_db() as f32
        } else {
            0.0f32
        };

        #[cfg(not(feature = "audio"))]
        let agc_gain = 0.0f32;
        let demod_us = demod_start.elapsed().as_micros() as u64;

        // 4. RMS power (always — cheap, needed for stats/tracking)
        let rms_dbfs = rms_power_dbfs(&samples);

        block_count += 1;

        // Cache samples for on-demand analysis (always)
        last_samples = samples.clone();

        // 5-10. Spectrum, CFAR, flatness, SNR, stats — skippable under load
        #[cfg(feature = "audio")]
        let spectrum_update_interval = active_demod_config
            .as_ref()
            .map(|cfg| u64::from(cfg.spectrum_update_interval_blocks.max(1)))
            .unwrap_or(1);
        #[cfg(not(feature = "audio"))]
        let spectrum_update_interval = 1u64;

        let do_spectrum = load_shedder.run_spectrum(block_count)
            && (block_count == 1 || block_count % spectrum_update_interval == 0);

        let (
            spectrum_db,
            noise_floor_db,
            detections,
            flatness,
            snr,
            stats,
            fft_us,
            cfar_elapsed_us,
            stats_elapsed_us,
        ) = if do_spectrum {
            let fft_start = Instant::now();
            let spectrum_db = if samples.len() >= config.fft_size * 2 {
                analyzer.compute_averaged_spectrum(&samples, 0.5)
            } else {
                analyzer.compute_spectrum(&samples)
            };
            let fft_elapsed = fft_start.elapsed().as_micros() as u64;

            let cfar_start = Instant::now();
            let noise_floor_db = noise_floor_sigma_clip(&spectrum_db, 3, 2.5);
            let spectrum_linear = db_to_linear(&spectrum_db);
            let detections = cfar_detect(&spectrum_linear, &cfar_config, config.sample_rate);
            let flatness = spectral_flatness(&spectrum_linear);
            let cfar_elapsed = cfar_start.elapsed().as_micros() as u64;

            let stats_start = Instant::now();
            let snr = snr_m2m4(&samples);
            let stats = signal_statistics(&samples);
            let stats_elapsed = stats_start.elapsed().as_micros() as u64;

            // Cache spectrum
            last_spectrum_db = spectrum_db.clone();

            (
                spectrum_db,
                noise_floor_db,
                detections,
                flatness,
                snr,
                stats,
                fft_elapsed,
                cfar_elapsed,
                stats_elapsed,
            )
        } else {
            // Reuse cached spectrum — skip expensive DSP
            let snr = snr_m2m4(&samples);
            let stats_start = Instant::now();
            let stats = signal_statistics(&samples);
            let stats_elapsed = stats_start.elapsed().as_micros() as u64;
            (
                last_spectrum_db.clone(),
                0.0,
                Vec::new(),
                0.0,
                snr,
                stats,
                0,
                0,
                stats_elapsed,
            )
        };

        let processing_time_us = proc_start.elapsed().as_micros() as u64;

        // Time-series tracking — push + emit at ~1 Hz (every 8 blocks at ~7.8 blocks/sec)
        // Only push at snapshot cadence so the 1800-entry buffer holds ~30 min.
        if let Some(ref mut t) = tracker {
            if block_count % 8 == 0 {
                let elapsed = loop_start.elapsed().as_secs_f64();
                t.push(snr, rms_dbfs, noise_floor_db, 0.0, flatness, elapsed);
                try_send_event(
                    &evt_tx,
                    Event::TrackingUpdate(t.snapshot()),
                    &events_dropped,
                );
            }
        }

        // Feed active decoders (non-blocking, skippable under heavy load)
        let decoder_feed_start = Instant::now();
        if load_shedder.feed_decoders(block_count) {
            // Check liveness every 64 blocks (~8s) and remove dead decoders
            if block_count % 64 == 0 {
                let before = active_decoders.len();
                active_decoders.retain(|h| {
                    if h.is_alive() {
                        true
                    } else {
                        try_send_event(
                            &evt_tx,
                            Event::Error(format!(
                                "Decoder '{}' died unexpectedly — removed",
                                h.name()
                            )),
                            &events_dropped,
                        );
                        false
                    }
                });
                if active_decoders.len() < before {
                    tracing::warn!("Removed {} dead decoder(s)", before - active_decoders.len());
                }
            }
            for handle in &active_decoders {
                handle.feed(samples.clone());
            }
        }
        let decoder_feed_us = decoder_feed_start.elapsed().as_micros() as u64;

        // Emit spectrum event (non-blocking, only when spectrum was computed)
        if do_spectrum {
            try_send_event(
                &evt_tx,
                Event::SpectrumReady(SpectrumFrame {
                    spectrum_db,
                    noise_floor_db,
                    rms_dbfs,
                    snr_db: snr,
                    spectral_flatness: flatness,
                    signal_stats: stats,
                    agc_gain_db: agc_gain,
                    block_count,
                }),
                &events_dropped,
            );

            // Emit detections if any
            if !detections.is_empty() {
                try_send_event(&evt_tx, Event::Detections(detections), &events_dropped);
            }
        }

        // Build latency breakdown for this block
        let latency = LatencyBreakdown {
            dc_removal_us,
            fft_us,
            cfar_us: cfar_elapsed_us,
            statistics_us: stats_elapsed_us,
            decoder_feed_us,
            demod_us,
            total_us: processing_time_us,
        };

        // Compute health status
        let current_events_dropped = events_dropped.load(Ordering::Relaxed);
        let health = if buffer_len as f32 / buffer_capacity as f32 > 0.75 || load_shedder.level == 2
        {
            HealthStatus::Critical
        } else if buffer_len as f32 / buffer_capacity as f32 > 0.40
            || current_events_dropped > 0
            || load_shedder.level == 1
        {
            HealthStatus::Warning
        } else {
            HealthStatus::Normal
        };

        // Emit health change event on transitions
        if health != prev_health {
            prev_health = health;
            try_send_event(
                &evt_tx,
                Event::Status(StatusUpdate::HealthChanged(health)),
                &events_dropped,
            );
        }

        // Periodic checkpoint (every 1000 blocks ≈ 8 seconds at 2 MS/s)
        if block_count % 1000 == 0 && block_count > 0 {
            let cp = SessionCheckpoint {
                schema_version: CHECKPOINT_SCHEMA_VERSION,
                timestamp: {
                    let elapsed = timeline.elapsed_s();
                    format!("{elapsed:.1}s")
                },
                config: config.clone(),
                frequency: config.frequency,
                gain: config.gain,
                active_decoders: active_decoders
                    .iter()
                    .map(|h| h.name().to_string())
                    .collect(),
                recording_path: recording
                    .as_ref()
                    .map(|r| r.path().to_string_lossy().to_string()),
                tracking_active: tracker.is_some(),
                timeline_entries: timeline.entry_count(),
                blocks_processed: block_count,
                events_dropped: current_events_dropped,
            };
            if let Err(e) = checkpoint::save_checkpoint(&cp) {
                tracing::warn!("Checkpoint save failed: {e}");
            }
        }

        // Periodic stats (non-blocking)
        if block_count % 10 == 0 {
            let block_duration_us = if config.sample_rate > 0.0 {
                (262144.0 / config.sample_rate * 1e6) as u64
            } else {
                1
            };
            let cpu_load = if block_duration_us > 0 {
                (processing_time_us as f32 / block_duration_us as f32) * 100.0
            } else {
                0.0
            };

            try_send_event(
                &evt_tx,
                Event::Stats(SessionStats {
                    blocks_processed: block_count,
                    blocks_dropped: dropped_counter.load(Ordering::Relaxed),
                    processing_time_us,
                    throughput_msps: config.sample_rate / 1e6,
                    cpu_load_percent: cpu_load,
                    buffer_occupancy: buffer_len as u16,
                    events_dropped: current_events_dropped,
                    health,
                    latency,
                }),
                &events_dropped,
            );
        }
    }

    // Clear checkpoint on clean shutdown
    checkpoint::clear_checkpoint();

    // Cleanup
    for handle in active_decoders {
        handle.stop();
    }

    // Finalize recording if still active
    if let Some(rec) = recording.take() {
        let was_raw = rec.writer.is_raw();
        let rec_path = rec.rec_path.clone();
        let total = rec.writer.finish().unwrap_or(0);
        if was_raw {
            write_sigmf_sidecar(&rec_path, config.frequency, config.sample_rate);
        }
        try_send_event(
            &evt_tx,
            Event::Status(StatusUpdate::RecordingStopped(total)),
            &events_dropped,
        );
    }

    // Finalize demod if still active
    #[cfg(feature = "audio")]
    if let Some(mut ds) = demod_state.take() {
        if let Some(writer) = ds.wav_writer.take() {
            writer.finalize().ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::decoder::{DecoderPlugin, DecoderRequirements};
    use crate::hardware::GainMode;
    use crate::session::DecodedMessage;
    use crate::types::Sample;

    // Note: Full SessionManager tests require hardware (RtlSdrDevice).
    // These unit tests verify the types and configuration.

    #[test]
    fn session_config_creation() {
        let config = SessionConfig {
            schema_version: 1,
            device_index: 0,
            frequency: 433.92e6,
            sample_rate: 2.048e6,
            gain: GainMode::Auto,
            ppm: 0,
            fft_size: 2048,
            pfa: 1e-4,
        };
        assert_eq!(config.fft_size, 2048);
        assert!((config.frequency - 433.92e6).abs() < 1.0);
    }

    #[test]
    fn decoder_registry_in_session() {
        struct TestDecoder;
        impl DecoderPlugin for TestDecoder {
            fn name(&self) -> &str {
                "test"
            }
            fn requirements(&self) -> DecoderRequirements {
                DecoderRequirements {
                    center_frequency: 100e6,
                    sample_rate: 48000.0,
                    bandwidth: 10000.0,
                    wants_iq: true,
                }
            }
            fn process(&mut self, _samples: &[Sample]) -> Vec<DecodedMessage> {
                vec![]
            }
            fn reset(&mut self) {}
        }

        let mut registry = DecoderRegistry::new();
        registry.register("test", || Box::new(TestDecoder));

        assert!(registry.create("test").is_some());
        assert!(registry.create("unknown").is_none());
    }

    #[test]
    fn demod_mode_defaults_coverage() {
        // Verify all recognized modes return defaults
        for mode in &[
            "am",
            "am-sync",
            "fm",
            "wfm",
            "wfm-stereo",
            "usb",
            "lsb",
            "cw",
        ] {
            let defaults = mode_defaults(mode);
            assert!(defaults.is_some(), "mode '{}' should have defaults", mode);
            let d = defaults.unwrap();
            assert!(d.sample_rate > 0.0);
            assert!(d.ddc_output_rate > 0.0);
            assert!(d.channel_bw > 0.0);
        }
        assert!(mode_defaults("unknown").is_none());
    }

    #[test]
    fn rec_writer_types() {
        // Verify RecordFormat→RecWriter mapping compiles
        let _formats = [
            RecordFormat::RawCf32,
            RecordFormat::Wav,
            RecordFormat::SigMf,
        ];
    }
}
