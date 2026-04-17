use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use wavecore::captures::CaptureCatalog;
use wavecore::hardware::GainMode;
use wavecore::mode::ModeController;
use wavecore::recording::RecordingMetadata;
use wavecore::session::SessionConfig;
use wavecore::session::{Command, Event, StatusUpdate};
use wavecore::util::utc_timestamp_now;

use crate::state::{RecordingContext, SessionOrigin};

/// Serializable decoded message DTO (converts Instant → elapsed_ms).
#[derive(Debug, Clone, Serialize)]
pub struct SerDecodedMessage {
    pub decoder: String,
    pub elapsed_ms: u64,
    pub summary: String,
    pub fields: BTreeMap<String, String>,
    pub raw_bits: Option<Vec<u8>>,
}

#[derive(Clone)]
pub struct BridgeState {
    pub mode_controller: Arc<Mutex<Option<ModeController>>>,
    pub session_config: Arc<Mutex<Option<SessionConfig>>>,
    pub session_origin: Arc<Mutex<Option<SessionOrigin>>>,
    pub recording_context: Arc<Mutex<Option<RecordingContext>>>,
}

/// Start the event bridge thread.
///
/// Drains SessionManager events and emits them as Tauri events
/// to the Svelte frontend. Also forwards events to the ModeController
/// and sends resulting commands back to the session.
pub fn start_event_bridge(
    app_handle: AppHandle,
    event_rx: Receiver<Event>,
    session_start: Instant,
    running: Arc<AtomicBool>,
    state: BridgeState,
    cmd_tx: Sender<Command>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("gui-bridge".to_string())
        .spawn(move || {
            let tick_interval = Duration::from_millis(33); // ~30 Hz
            let mut last_tick = Instant::now();

            while running.load(Ordering::Relaxed) {
                match event_rx.recv_timeout(Duration::from_millis(10)) {
                    Ok(event) => {
                        // Forward to mode controller
                        if let Some(ref mut mc) = *state.mode_controller.lock() {
                            let cmds = mc.handle_event(&event);
                            for cmd in cmds {
                                cmd_tx.send(cmd).ok();
                            }
                        }

                        dispatch_event(
                            &app_handle,
                            event,
                            session_start,
                            &state.session_config,
                            &state.session_origin,
                            &state.recording_context,
                        );
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                }

                // Periodic tick for mode controller
                if last_tick.elapsed() >= tick_interval {
                    if let Some(ref mut mc) = *state.mode_controller.lock() {
                        let cmds = mc.tick();
                        for cmd in cmds {
                            cmd_tx.send(cmd).ok();
                        }

                        // Emit mode status
                        if let Some(status) = mc.mode_status() {
                            app_handle.emit("wr:mode-status", &status).ok();
                        }
                    }
                    last_tick = Instant::now();
                }
            }
        })
        .expect("Failed to spawn bridge thread")
}

fn gain_mode_string(mode: GainMode) -> String {
    match mode {
        GainMode::Auto => "auto".to_string(),
        GainMode::Manual(db) => db.to_string(),
    }
}

fn format_to_metadata(path: &std::path::Path) -> String {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("wav") => "cf32-wav".to_string(),
        Some("sigmf") | Some("sigmf-data") => "sigmf-cf32_le".to_string(),
        _ => "cf32".to_string(),
    }
}

fn context_from_status(
    path: &std::path::Path,
    session_config: &Arc<Mutex<Option<SessionConfig>>>,
    session_origin: &Arc<Mutex<Option<SessionOrigin>>>,
) -> Option<RecordingContext> {
    let cfg = session_config.lock().clone()?;
    let origin = session_origin
        .lock()
        .as_ref()
        .copied()
        .unwrap_or(SessionOrigin::LiveRtlSdr);

    Some(RecordingContext {
        path: path.to_path_buf(),
        started_at: Instant::now(),
        center_freq: cfg.frequency,
        sample_rate: cfg.sample_rate,
        gain: gain_mode_string(cfg.gain),
        format: format_to_metadata(path),
        device: origin.device_name().to_string(),
        source: origin.capture_source(),
        label: None,
        notes: None,
        tags: Vec::new(),
        demod_mode: None,
        decoder: None,
        started: true,
    })
}

fn finalize_recording(context: RecordingContext, total_samples: u64) -> Result<(), String> {
    let metadata = RecordingMetadata {
        schema_version: 1,
        center_freq: context.center_freq,
        sample_rate: context.sample_rate,
        gain: context.gain,
        format: context.format,
        timestamp: utc_timestamp_now(),
        duration_secs: Some(context.started_at.elapsed().as_secs_f64()),
        device: context.device,
        samples_written: total_samples,
        label: context.label,
        notes: context.notes,
        tags: context.tags,
        demod_mode: context.demod_mode,
        decoder: context.decoder,
        timeline_path: None,
        report_path: None,
    };

    metadata
        .write_sidecar(&context.path)
        .map_err(|e| format!("Failed to write GUI recording metadata: {e}"))?;

    let mut catalog = CaptureCatalog::load();
    catalog.register(&context.path, &metadata, context.source);
    catalog
        .save()
        .map_err(|e| format!("Failed to update capture catalog: {e}"))?;
    Ok(())
}

fn dispatch_event(
    app: &AppHandle,
    event: Event,
    session_start: Instant,
    session_config: &Arc<Mutex<Option<SessionConfig>>>,
    session_origin: &Arc<Mutex<Option<SessionOrigin>>>,
    recording_context: &Arc<Mutex<Option<RecordingContext>>>,
) {
    match &event {
        Event::Status(StatusUpdate::RecordingStarted(path)) => {
            let mut guard = recording_context.lock();
            if let Some(context) = guard.as_mut() {
                context.path = path.clone();
                context.started = true;
            } else if let Some(context) = context_from_status(path, session_config, session_origin)
            {
                *guard = Some(context);
            }
        }
        Event::Status(StatusUpdate::RecordingStopped(total_samples)) => {
            if let Some(context) = recording_context.lock().take() {
                if context.started {
                    if let Err(err) = finalize_recording(context, *total_samples) {
                        app.emit("wr:error", &err).ok();
                    }
                }
            }
        }
        Event::Error(_) => {
            let should_clear = recording_context
                .lock()
                .as_ref()
                .is_some_and(|context| !context.started);
            if should_clear {
                recording_context.lock().take();
            }
        }
        _ => {}
    }

    match event {
        Event::SpectrumReady(frame) => {
            app.emit("wr:spectrum", &frame).ok();
        }
        Event::Detections(detections) => {
            app.emit("wr:detections", &detections).ok();
        }
        Event::Stats(stats) => {
            app.emit("wr:stats", &stats).ok();
        }
        Event::DecodedMessage(msg) => {
            let ser = SerDecodedMessage {
                decoder: msg.decoder,
                elapsed_ms: session_start.elapsed().as_millis() as u64,
                summary: msg.summary,
                fields: msg.fields,
                raw_bits: msg.raw_bits,
            };
            app.emit("wr:decoded", &ser).ok();
        }
        Event::DemodVis(vis) => {
            app.emit("wr:demod-vis", &vis).ok();
        }
        Event::Status(status) => {
            app.emit("wr:status", &status).ok();
        }
        Event::AnalysisResult { id, result } => {
            #[derive(serde::Serialize)]
            struct AnalysisEvent {
                id: u64,
                result: wavecore::analysis::AnalysisResult,
            }
            app.emit("wr:analysis-result", &AnalysisEvent { id, result })
                .ok();
        }
        Event::TrackingUpdate(snapshot) => {
            app.emit("wr:tracking", &snapshot).ok();
        }
        Event::AnnotationAdded(id) => {
            app.emit("wr:annotation-added", &id).ok();
        }
        Event::Error(err) => {
            app.emit("wr:error", &err).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn format_to_metadata_maps_supported_recording_types() {
        assert_eq!(
            format_to_metadata(Path::new("/tmp/capture.wav")),
            "cf32-wav"
        );
        assert_eq!(
            format_to_metadata(Path::new("/tmp/capture.sigmf")),
            "sigmf-cf32_le"
        );
        assert_eq!(
            format_to_metadata(Path::new("/tmp/capture.sigmf-data")),
            "sigmf-cf32_le"
        );
        assert_eq!(format_to_metadata(Path::new("/tmp/capture.cf32")), "cf32");
    }

    #[test]
    fn context_from_status_uses_session_state() {
        let session_config = Arc::new(Mutex::new(Some(SessionConfig {
            schema_version: 1,
            device_index: 0,
            frequency: 94_900_000.0,
            sample_rate: 2_048_000.0,
            gain: GainMode::Manual(35.0),
            ppm: 0,
            fft_size: 2048,
            pfa: 1e-4,
        })));
        let session_origin = Arc::new(Mutex::new(Some(SessionOrigin::Replay)));

        let context = context_from_status(
            Path::new("/tmp/capture.sigmf"),
            &session_config,
            &session_origin,
        )
        .expect("context should be synthesized");

        assert_eq!(context.device, "replay");
        assert_eq!(context.source, SessionOrigin::Replay.capture_source());
        assert_eq!(context.format, "sigmf-cf32_le");
        assert!((context.center_freq - 94_900_000.0).abs() < 1.0);
        assert!((context.sample_rate - 2_048_000.0).abs() < 1.0);
        assert_eq!(context.gain, "35");
        assert!(context.started);
    }
}
