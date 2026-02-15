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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};

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
use crate::session::{
    Command, DecodedMessage, DemodConfig, DemodVisData, Event, RecordFormat, SessionConfig,
    SessionStats, SpectrumFrame, StatusUpdate,
};
use crate::sigmf::SigMfWriter;
use crate::types::{Sample, SampleBlock};

// Demod chain imports
use crate::dsp::agc::Agc;
use crate::dsp::ddc::Ddc;
use crate::dsp::demod::am::{AmDemod, AmMode};
use crate::dsp::demod::cw::CwDemod;
use crate::dsp::demod::fm::{FmDemod, FmMode};
use crate::dsp::demod::ssb::{SsbDemod, Sideband};
use crate::dsp::demod::{Demodulator, mode_defaults};
use crate::dsp::resample::PolyphaseResampler;

#[cfg(feature = "audio")]
use crate::audio::AudioSink;

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

/// Active recording state in the processing loop.
struct RecordingState {
    writer: RecWriter,
    samples_written: u64,
    _path: PathBuf,
}

// ============================================================================
// Demod types (requires audio feature)
// ============================================================================

#[cfg(feature = "audio")]
struct DemodState {
    ddc: Ddc,
    agc: Agc,
    demod: Box<dyn Demodulator>,
    resampler: Option<PolyphaseResampler>,
    audio_sink: AudioSink,
    wav_writer: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>,
    total_audio_samples: u64,
}

/// Build a demodulation chain from configuration.
///
/// Creates DDC → AGC → Demodulator → Resampler → AudioSink.
#[cfg(feature = "audio")]
fn build_demod_state(
    config: &DemodConfig,
    sample_rate: f64,
) -> Result<DemodState, String> {
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

    let demod_out_rate = demod.sample_rate_out();
    let resampler = if (demod_out_rate - config.audio_rate as f64).abs() > 1.0 {
        Some(PolyphaseResampler::new(
            config.audio_rate as usize,
            demod_out_rate as usize,
            128,
            0.0,
        ))
    } else {
        None
    };

    let audio_sink = AudioSink::new(config.audio_rate)
        .map_err(|e| format!("Failed to create audio sink: {e}"))?;

    let wav_writer = if let Some(ref path) = config.output_wav {
        let channels = if config.mode == "wfm-stereo" { 2 } else { 1 };
        let spec = hound::WavSpec {
            channels,
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
    /// Create a new SessionManager and start processing.
    ///
    /// Returns the manager and a receiver for events. The caller reads
    /// events from the receiver in their main loop.
    pub fn new(
        config: SessionConfig,
        decoder_registry: DecoderRegistry,
    ) -> Result<(Self, Receiver<Event>), String> {
        // Open hardware
        let device = Arc::new(
            RtlSdrDevice::open(config.device_index)
                .map_err(|e| format!("Failed to open device: {e}"))?,
        );
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
        let running = Arc::new(AtomicBool::new(true));

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
                        center_freq: 0.0,
                        sample_rate: 0.0,
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
fn run_processing_loop(
    consumer: SampleConsumer,
    cmd_rx: Receiver<Command>,
    evt_tx: Sender<Event>,
    device: Arc<Box<dyn SdrDevice>>,
    config: SessionConfig,
    decoder_registry: DecoderRegistry,
    running: Arc<AtomicBool>,
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
    let blocks_dropped = AtomicU64::new(0);

    // Decoded message channel — decoders send here
    let (decoded_tx, decoded_rx) = crossbeam_channel::unbounded::<DecodedMessage>();

    // Active decoder handles
    let mut active_decoders: Vec<DecoderHandle> = Vec::new();

    // Recording state
    let mut recording: Option<RecordingState> = None;

    // Demod state (requires audio feature)
    #[cfg(feature = "audio")]
    let mut demod_state: Option<DemodState> = None;

    while running.load(Ordering::Relaxed) {
        // ----------------------------------------------------------------
        // Process commands (non-blocking)
        // ----------------------------------------------------------------
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Command::Tune(freq) => {
                    if let Err(e) = device.set_frequency(freq) {
                        evt_tx
                            .send(Event::Error(format!("Failed to tune: {e}")))
                            .ok();
                    } else {
                        evt_tx
                            .send(Event::Status(StatusUpdate::FrequencyChanged(freq)))
                            .ok();
                    }
                }
                Command::SetGain(mode) => {
                    if let Err(e) = device.set_gain(mode) {
                        evt_tx
                            .send(Event::Error(format!("Failed to set gain: {e}")))
                            .ok();
                    } else {
                        evt_tx
                            .send(Event::Status(StatusUpdate::GainChanged(mode)))
                            .ok();
                    }
                }
                Command::SetSampleRate(rate) => {
                    if let Err(e) = device.set_sample_rate(rate) {
                        evt_tx
                            .send(Event::Error(format!("Failed to set sample rate: {e}")))
                            .ok();
                    }
                }
                Command::EnableDecoder(name) => {
                    if let Some(decoder) = decoder_registry.create(&name) {
                        let handle =
                            DecoderHandle::spawn(decoder, decoded_tx.clone(), 32);
                        evt_tx
                            .send(Event::Status(StatusUpdate::DecoderEnabled(name)))
                            .ok();
                        active_decoders.push(handle);
                    } else {
                        evt_tx
                            .send(Event::Error(format!("Unknown decoder: {name}")))
                            .ok();
                    }
                }
                Command::DisableDecoder(name) => {
                    let idx = active_decoders
                        .iter()
                        .position(|h| h.name() == name);
                    if let Some(idx) = idx {
                        let handle = active_decoders.remove(idx);
                        handle.stop();
                        evt_tx
                            .send(Event::Status(StatusUpdate::DecoderDisabled(name)))
                            .ok();
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
                                _path: path.clone(),
                            });
                            evt_tx
                                .send(Event::Status(StatusUpdate::RecordingStarted(path)))
                                .ok();
                        }
                        Err(e) => {
                            evt_tx
                                .send(Event::Error(format!("Failed to start recording: {e}")))
                                .ok();
                        }
                    }
                }
                Command::StopRecord => {
                    if let Some(rec) = recording.take() {
                        let total = rec.writer.finish().unwrap_or(rec.samples_written);
                        evt_tx
                            .send(Event::Status(StatusUpdate::RecordingStopped(total)))
                            .ok();
                    }
                }

                // Demod commands
                Command::StartDemod(demod_config) => {
                    #[cfg(feature = "audio")]
                    {
                        match build_demod_state(&demod_config, config.sample_rate) {
                            Ok(state) => {
                                demod_state = Some(state);
                                evt_tx
                                    .send(Event::Status(StatusUpdate::Streaming))
                                    .ok();
                            }
                            Err(e) => {
                                evt_tx
                                    .send(Event::Error(format!("Failed to start demod: {e}")))
                                    .ok();
                            }
                        }
                    }
                    #[cfg(not(feature = "audio"))]
                    {
                        let _ = demod_config;
                        evt_tx
                            .send(Event::Error(
                                "Demod requires the 'audio' feature".to_string(),
                            ))
                            .ok();
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

        // Forward decoded messages to frontend
        while let Ok(msg) = decoded_rx.try_recv() {
            evt_tx.send(Event::DecodedMessage(msg)).ok();
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

        let proc_start = Instant::now();
        let mut samples = block.samples;

        // 1. DC removal
        dc_remover.process(&mut samples);

        // 2. Recording — write raw IQ after DC removal
        if let Some(ref mut rec) = recording {
            if let Err(e) = rec.writer.write_samples(&samples) {
                evt_tx
                    .send(Event::Error(format!("Recording write error: {e}")))
                    .ok();
                // Stop recording on error
                if let Some(rec) = recording.take() {
                    rec.writer.finish().ok();
                }
            } else {
                rec.samples_written += samples.len() as u64;
            }
        }

        // 3. Demodulation chain (DDC → AGC → Demod → Resample → Audio)
        #[cfg(feature = "audio")]
        let agc_gain = if let Some(ref mut ds) = demod_state {
            let baseband = ds.ddc.process(&samples);
            let mut agc_out = baseband;
            ds.agc.process(&mut agc_out);

            let audio = ds.demod.process(&agc_out);

            let audio_out = if let Some(ref mut resampler) = ds.resampler {
                let iq: Vec<Sample> = audio
                    .iter()
                    .map(|&s| Sample::new(s, 0.0))
                    .collect();
                let resampled = resampler.process(&iq);
                resampled.iter().map(|s| s.re).collect::<Vec<f32>>()
            } else {
                audio
            };

            ds.audio_sink.write(&audio_out);
            ds.total_audio_samples += audio_out.len() as u64;

            // WAV recording of demodulated audio
            if let Some(ref mut writer) = ds.wav_writer {
                for &sample in &audio_out {
                    let s16 = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
                    writer.write_sample(s16).ok();
                }
            }

            // Emit demod visualization data
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
            evt_tx.send(Event::DemodVis(vis)).ok();

            ds.agc.gain_db() as f32
        } else {
            0.0f32
        };

        #[cfg(not(feature = "audio"))]
        let agc_gain = 0.0f32;

        // 4. RMS power
        let rms_dbfs = rms_power_dbfs(&samples);

        // 5. Compute spectrum
        let spectrum_db = if samples.len() >= config.fft_size * 2 {
            analyzer.compute_averaged_spectrum(&samples, 0.5)
        } else {
            analyzer.compute_spectrum(&samples)
        };

        // 6. Noise floor via sigma-clipping
        let noise_floor_db = noise_floor_sigma_clip(&spectrum_db, 3, 2.5);

        // 7. CFAR detection
        let spectrum_linear = db_to_linear(&spectrum_db);
        let detections = cfar_detect(&spectrum_linear, &cfar_config, config.sample_rate);

        // 8. Spectral flatness
        let flatness = spectral_flatness(&spectrum_linear);

        // 9. SNR estimation
        let snr = snr_m2m4(&samples);

        // 10. Signal statistics
        let stats = signal_statistics(&samples);

        block_count += 1;
        let processing_time_us = proc_start.elapsed().as_micros() as u64;

        // Feed active decoders (non-blocking)
        for handle in &active_decoders {
            handle.feed(samples.clone());
        }

        // Emit spectrum event
        evt_tx
            .send(Event::SpectrumReady(SpectrumFrame {
                spectrum_db,
                noise_floor_db,
                rms_dbfs,
                snr_db: snr,
                spectral_flatness: flatness,
                signal_stats: stats,
                agc_gain_db: agc_gain,
                block_count,
            }))
            .ok();

        // Emit detections if any
        if !detections.is_empty() {
            evt_tx.send(Event::Detections(detections)).ok();
        }

        // Periodic stats
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

            evt_tx
                .send(Event::Stats(SessionStats {
                    blocks_processed: block_count,
                    blocks_dropped: blocks_dropped.load(Ordering::Relaxed),
                    processing_time_us,
                    throughput_msps: config.sample_rate / 1e6,
                    cpu_load_percent: cpu_load,
                }))
                .ok();
        }
    }

    // Cleanup
    for handle in active_decoders {
        handle.stop();
    }

    // Finalize recording if still active
    if let Some(rec) = recording.take() {
        let total = rec.writer.finish().unwrap_or(0);
        evt_tx
            .send(Event::Status(StatusUpdate::RecordingStopped(total)))
            .ok();
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
            fn name(&self) -> &str { "test" }
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
        for mode in &["am", "am-sync", "fm", "wfm", "wfm-stereo", "usb", "lsb", "cw"] {
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
        let _formats = [RecordFormat::RawCf32, RecordFormat::Wav, RecordFormat::SigMf];
    }
}
