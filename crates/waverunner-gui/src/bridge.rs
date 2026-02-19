use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use wavecore::mode::ModeController;
use wavecore::session::{Command, Event};

/// Serializable decoded message DTO (converts Instant → elapsed_ms).
#[derive(Debug, Clone, Serialize)]
pub struct SerDecodedMessage {
    pub decoder: String,
    pub elapsed_ms: u64,
    pub summary: String,
    pub fields: BTreeMap<String, String>,
    pub raw_bits: Option<Vec<u8>>,
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
    mode_controller: Arc<Mutex<Option<ModeController>>>,
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
                        if let Some(ref mut mc) = *mode_controller.lock() {
                            let cmds = mc.handle_event(&event);
                            for cmd in cmds {
                                cmd_tx.send(cmd).ok();
                            }
                        }

                        dispatch_event(&app_handle, event, session_start);
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                }

                // Periodic tick for mode controller
                if last_tick.elapsed() >= tick_interval {
                    if let Some(ref mut mc) = *mode_controller.lock() {
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

fn dispatch_event(app: &AppHandle, event: Event, session_start: Instant) {
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
            app.emit("wr:analysis-result", &AnalysisEvent { id, result }).ok();
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
