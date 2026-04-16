use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use serde::Serialize;
use tauri::State;
use wavecore::bookmarks::{Bookmark, BookmarkStore};
use wavecore::captures::{CaptureCatalog, CaptureRecord, default_capture_path};
use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::hardware::GainMode;
use wavecore::mode::ModeController;
use wavecore::mode::general::{GeneralMode, GeneralModeConfig};
use wavecore::session::manager::SessionManager;
use wavecore::session::replay::ReplayDevice;
use wavecore::session::{Command, DemodConfig, RecordFormat, SessionConfig};

use crate::bridge::{BridgeState, start_event_bridge};
use crate::state::{AppState, RecordingContext, SessionOrigin};

#[tauri::command]
pub fn connect_device(
    config: SessionConfig,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let mut session_guard = state.session.lock();
    if session_guard.is_some() {
        return Err("Already connected. Disconnect first.".to_string());
    }

    let registry = build_decoder_registry();
    let (session, event_rx) = SessionManager::new(config.clone(), registry)?;

    let now = Instant::now();
    *state.session_start.lock() = Some(now);
    *state.session_config.lock() = Some(config);
    *state.session_origin.lock() = Some(SessionOrigin::LiveRtlSdr);
    *state.recording_context.lock() = None;
    state.bridge_running.store(true, Ordering::Relaxed);

    // Initialize mode controller
    let decoder_names: Vec<String> = wavecore::dsp::decoders::DECODER_NAMES
        .iter()
        .map(|s| s.to_string())
        .collect();
    *state.mode_controller.lock() = Some(ModeController::new(decoder_names));

    let cmd_tx = session.cmd_sender();
    let handle = start_event_bridge(
        app_handle,
        event_rx,
        now,
        state.bridge_running.clone(),
        BridgeState {
            mode_controller: Arc::clone(&state.mode_controller),
            session_config: Arc::clone(&state.session_config),
            session_origin: Arc::clone(&state.session_origin),
            recording_context: Arc::clone(&state.recording_context),
        },
        cmd_tx,
    );
    *state.bridge_handle.lock() = Some(handle);
    *session_guard = Some(session);

    Ok(())
}

#[tauri::command]
pub fn disconnect_device(state: State<'_, AppState>) -> Result<(), String> {
    let session = state.session.lock().take();
    if let Some(session) = session {
        // Deactivate mode before shutdown
        if let Some(ref mut mc) = *state.mode_controller.lock() {
            let cmds = mc.deactivate();
            for cmd in cmds {
                session.send(cmd).ok();
            }
        }
        *state.mode_controller.lock() = None;

        let bridge_handle = state.bridge_handle.lock().take();
        session.shutdown();
        if let Some(handle) = bridge_handle {
            handle.join().ok();
        }
        state.bridge_running.store(false, Ordering::Relaxed);
        state.tracking_active.store(false, Ordering::Relaxed);
        state.analysis_counter.store(1, Ordering::Relaxed);
        *state.session_start.lock() = None;
        *state.session_config.lock() = None;
        *state.session_origin.lock() = None;
        *state.recording_context.lock() = None;
        Ok(())
    } else {
        Err("Not connected".to_string())
    }
}

#[tauri::command]
pub fn replay_file(
    path: String,
    sample_rate: f64,
    frequency: f64,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let mut session_guard = state.session.lock();
    if session_guard.is_some() {
        return Err("Already connected. Disconnect first.".to_string());
    }

    let device = ReplayDevice::open(std::path::Path::new(&path), sample_rate)
        .map_err(|e| format!("Failed to open replay file: {e}"))?;

    let config = SessionConfig {
        schema_version: 1,
        device_index: 0,
        frequency,
        sample_rate,
        gain: GainMode::Auto,
        ppm: 0,
        fft_size: 2048,
        pfa: 1e-4,
    };

    let registry = build_decoder_registry();
    let (session, event_rx) = SessionManager::new_with_device(config.clone(), device, registry)?;

    let now = Instant::now();
    *state.session_start.lock() = Some(now);
    *state.session_config.lock() = Some(config);
    *state.session_origin.lock() = Some(SessionOrigin::Replay);
    *state.recording_context.lock() = None;
    state.bridge_running.store(true, Ordering::Relaxed);

    // Initialize mode controller
    let decoder_names: Vec<String> = wavecore::dsp::decoders::DECODER_NAMES
        .iter()
        .map(|s| s.to_string())
        .collect();
    *state.mode_controller.lock() = Some(ModeController::new(decoder_names));

    let cmd_tx = session.cmd_sender();
    let handle = start_event_bridge(
        app_handle,
        event_rx,
        now,
        state.bridge_running.clone(),
        BridgeState {
            mode_controller: Arc::clone(&state.mode_controller),
            session_config: Arc::clone(&state.session_config),
            session_origin: Arc::clone(&state.session_origin),
            recording_context: Arc::clone(&state.recording_context),
        },
        cmd_tx,
    );
    *state.bridge_handle.lock() = Some(handle);
    *session_guard = Some(session);

    Ok(())
}

#[tauri::command]
pub fn tune(frequency: f64, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(ref mut cfg) = *state.session_config.lock() {
        cfg.frequency = frequency;
    }
    send_command(&state, Command::Tune(frequency))
}

#[tauri::command]
pub fn set_gain(mode: GainMode, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(ref mut cfg) = *state.session_config.lock() {
        cfg.gain = mode;
    }
    send_command(&state, Command::SetGain(mode))
}

#[tauri::command]
pub fn set_sample_rate(rate: f64, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(ref mut cfg) = *state.session_config.lock() {
        cfg.sample_rate = rate;
    }
    send_command(&state, Command::SetSampleRate(rate))
}

#[tauri::command]
pub fn start_demod(config: DemodConfig, state: State<'_, AppState>) -> Result<(), String> {
    send_command(&state, Command::StartDemod(config))
}

#[tauri::command]
pub fn stop_demod(state: State<'_, AppState>) -> Result<(), String> {
    send_command(&state, Command::StopDemod)
}

#[tauri::command]
pub fn enable_decoder(name: String, state: State<'_, AppState>) -> Result<(), String> {
    send_command(&state, Command::EnableDecoder(name))
}

#[tauri::command]
pub fn disable_decoder(name: String, state: State<'_, AppState>) -> Result<(), String> {
    send_command(&state, Command::DisableDecoder(name))
}

#[tauri::command]
pub fn start_record(
    path: String,
    format: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let cfg = state
        .session_config
        .lock()
        .clone()
        .ok_or_else(|| "Not connected".to_string())?;
    let origin = state
        .session_origin
        .lock()
        .as_ref()
        .copied()
        .unwrap_or(SessionOrigin::LiveRtlSdr);
    let fmt = match format.as_str() {
        "cf32" | "raw" => RecordFormat::RawCf32,
        "wav" => RecordFormat::Wav,
        "sigmf" => RecordFormat::SigMf,
        _ => return Err(format!("Unknown format: {format}")),
    };
    let metadata_format = match fmt {
        RecordFormat::RawCf32 => "cf32".to_string(),
        RecordFormat::Wav => "cf32-wav".to_string(),
        RecordFormat::SigMf => "sigmf-cf32_le".to_string(),
    };
    let recording_path = PathBuf::from(path);
    let gain = match cfg.gain {
        GainMode::Auto => "auto".to_string(),
        GainMode::Manual(db) => db.to_string(),
    };

    {
        let mut recording = state.recording_context.lock();
        if recording.is_some() {
            return Err("Recording already in progress".to_string());
        }
        *recording = Some(RecordingContext {
            path: recording_path.clone(),
            started_at: Instant::now(),
            center_freq: cfg.frequency,
            sample_rate: cfg.sample_rate,
            gain,
            format: metadata_format,
            device: origin.device_name().to_string(),
            source: origin.capture_source(),
            started: false,
        });
    }

    let result = send_command(
        &state,
        Command::StartRecord {
            path: recording_path,
            format: fmt,
        },
    );
    if result.is_err() {
        state.recording_context.lock().take();
    }
    result
}

#[tauri::command]
pub fn stop_record(state: State<'_, AppState>) -> Result<(), String> {
    send_command(&state, Command::StopRecord)
}

#[tauri::command]
pub fn generate_capture_path(format: String, label: Option<String>) -> Result<String, String> {
    default_capture_path(&format, label.as_deref())
        .map(|path| path.display().to_string())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_recent_captures(limit: Option<usize>) -> Vec<CaptureRecord> {
    let mut catalog = CaptureCatalog::load();
    if catalog.prune_missing() > 0 {
        if let Err(e) = catalog.save() {
            tracing::warn!("Failed to save pruned capture catalog: {e}");
        }
    }
    catalog.list_recent(limit.unwrap_or(12))
}

#[tauri::command]
pub fn list_bookmarks() -> Vec<Bookmark> {
    BookmarkStore::load().list().to_vec()
}

#[tauri::command]
pub fn save_current_bookmark(
    name: String,
    mode: Option<String>,
    decoder: Option<String>,
    notes: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let cfg = state
        .session_config
        .lock()
        .clone()
        .ok_or_else(|| "Not connected".to_string())?;
    let mut store = BookmarkStore::load();
    store.add(Bookmark {
        name,
        frequency_hz: cfg.frequency,
        mode,
        decoder,
        notes,
    });
    store.save()
}

#[tauri::command]
pub fn remove_bookmark(name: String) -> Result<(), String> {
    let mut store = BookmarkStore::load();
    if store.remove(&name) {
        store.save()
    } else {
        Ok(())
    }
}

#[tauri::command]
pub fn get_available_devices() -> Result<Vec<wavecore::types::DeviceInfo>, String> {
    use wavecore::hardware::DeviceEnumerator;
    use wavecore::hardware::rtlsdr::RtlSdrDevice;
    RtlSdrDevice::enumerate().map_err(|e| format!("Enumeration failed: {e}"))
}

#[tauri::command]
pub fn get_available_decoders() -> Vec<String> {
    wavecore::dsp::decoders::DECODER_NAMES
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct DecoderInfo {
    pub name: String,
    pub backend: String,
    pub summary: String,
    pub required_tool: Option<String>,
    pub ready: bool,
    pub resolved_command: Option<String>,
}

#[tauri::command]
pub fn get_decoder_catalog() -> Vec<DecoderInfo> {
    let tool_index: std::collections::HashMap<_, _> =
        wavecore::dsp::decoders::tools::cached_tools()
            .iter()
            .map(|tool| (tool.name, tool))
            .collect();

    wavecore::dsp::decoders::DECODER_DESCRIPTORS
        .iter()
        .map(|descriptor| {
            let tool = descriptor
                .required_tool
                .and_then(|tool_name| tool_index.get(tool_name).copied());
            DecoderInfo {
                name: descriptor.name.to_string(),
                backend: descriptor.backend.as_str().to_string(),
                summary: descriptor.summary.to_string(),
                required_tool: descriptor.required_tool.map(|tool| tool.to_string()),
                ready: tool.map(|tool| tool.installed).unwrap_or(true),
                resolved_command: tool
                    .and_then(|tool| tool.resolved_command.map(|cmd| cmd.to_string())),
            }
        })
        .collect()
}

fn send_command(state: &State<'_, AppState>, cmd: Command) -> Result<(), String> {
    let guard = state.session.lock();
    match guard.as_ref() {
        Some(session) => session.send(cmd),
        None => Err("Not connected".to_string()),
    }
}

// ============================================================================
// Mode commands
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct ProfileInfo {
    pub name: String,
    pub description: String,
}

#[tauri::command]
pub fn list_profiles(state: State<'_, AppState>) -> Vec<ProfileInfo> {
    let guard = state.mode_controller.lock();
    match guard.as_ref() {
        Some(mc) => mc
            .list_profiles()
            .iter()
            .filter_map(|name| {
                mc.get_profile(name).map(|p| ProfileInfo {
                    name: p.name.clone(),
                    description: p.description.clone(),
                })
            })
            .collect(),
        None => {
            // Return profiles from a temporary controller
            let decoder_names: Vec<String> = wavecore::dsp::decoders::DECODER_NAMES
                .iter()
                .map(|s| s.to_string())
                .collect();
            let mc = ModeController::new(decoder_names);
            mc.list_profiles()
                .iter()
                .filter_map(|name| {
                    mc.get_profile(name).map(|p| ProfileInfo {
                        name: p.name.clone(),
                        description: p.description.clone(),
                    })
                })
                .collect()
        }
    }
}

#[tauri::command]
pub fn activate_profile(name: String, state: State<'_, AppState>) -> Result<(), String> {
    // Collect commands from mode controller, then send via session
    let cmds = {
        let mut mc_guard = state.mode_controller.lock();
        let mc = mc_guard
            .as_mut()
            .ok_or_else(|| "Not connected".to_string())?;

        let mut cmds = mc.deactivate();
        let mode = mc
            .create_profile_mode(&name)
            .ok_or_else(|| format!("Unknown profile: {name}"))?;
        cmds.extend(mc.activate(mode));
        cmds
    };

    let guard = state.session.lock();
    let session = guard.as_ref().ok_or_else(|| "Not connected".to_string())?;
    for cmd in cmds {
        session
            .send(cmd)
            .map_err(|e| format!("Failed to send mode command: {e}"))?;
    }
    Ok(())
}

#[tauri::command]
pub fn activate_general_scan(
    scan_start: f64,
    scan_end: f64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let cmds = {
        let mut mc_guard = state.mode_controller.lock();
        let mc = mc_guard
            .as_mut()
            .ok_or_else(|| "Not connected".to_string())?;

        let mut cmds = mc.deactivate();
        let config = GeneralModeConfig {
            scan_start,
            scan_end,
            ..Default::default()
        };
        let mode = GeneralMode::new(config);
        cmds.extend(mc.activate(Box::new(mode)));
        cmds
    };

    let guard = state.session.lock();
    let session = guard.as_ref().ok_or_else(|| "Not connected".to_string())?;
    for cmd in cmds {
        session
            .send(cmd)
            .map_err(|e| format!("Failed to send mode command: {e}"))?;
    }
    Ok(())
}

#[tauri::command]
pub fn deactivate_mode(state: State<'_, AppState>) -> Result<(), String> {
    let cmds = {
        let mut mc_guard = state.mode_controller.lock();
        let mc = mc_guard
            .as_mut()
            .ok_or_else(|| "Not connected".to_string())?;
        mc.deactivate()
    };

    let guard = state.session.lock();
    let session = guard.as_ref().ok_or_else(|| "Not connected".to_string())?;
    for cmd in cmds {
        session
            .send(cmd)
            .map_err(|e| format!("Failed to send mode command: {e}"))?;
    }
    Ok(())
}

// ============================================================================
// Analysis commands
// ============================================================================

#[tauri::command]
pub fn measure_signal(state: State<'_, AppState>) -> Result<(), String> {
    let cfg = state.session_config.lock().clone().ok_or("Not connected")?;
    let id = state.analysis_counter.fetch_add(1, Ordering::Relaxed);
    let request = wavecore::analysis::AnalysisRequest::MeasureSignal(
        wavecore::analysis::measurement::MeasureConfig {
            signal_center_bin: cfg.fft_size / 2,
            signal_width_bins: 100,
            adjacent_width_bins: 100,
            obw_threshold_db: 26.0,
        },
    );
    send_command(&state, Command::RunAnalysis { id, request })
}

#[tauri::command]
pub fn analyze_burst(threshold_db: f32, state: State<'_, AppState>) -> Result<(), String> {
    let cfg = state.session_config.lock().clone().ok_or("Not connected")?;
    let id = state.analysis_counter.fetch_add(1, Ordering::Relaxed);
    let request =
        wavecore::analysis::AnalysisRequest::AnalyzeBurst(wavecore::analysis::burst::BurstConfig {
            threshold_db,
            min_burst_samples: 10,
            sample_rate: cfg.sample_rate,
        });
    send_command(&state, Command::RunAnalysis { id, request })
}

#[tauri::command]
pub fn estimate_modulation(state: State<'_, AppState>) -> Result<(), String> {
    let cfg = state.session_config.lock().clone().ok_or("Not connected")?;
    let id = state.analysis_counter.fetch_add(1, Ordering::Relaxed);
    let request = wavecore::analysis::AnalysisRequest::EstimateModulation(
        wavecore::analysis::modulation::ModulationConfig {
            sample_rate: cfg.sample_rate,
            fft_size: cfg.fft_size,
        },
    );
    send_command(&state, Command::RunAnalysis { id, request })
}

#[tauri::command]
pub fn compare_spectra(state: State<'_, AppState>) -> Result<(), String> {
    let id = state.analysis_counter.fetch_add(1, Ordering::Relaxed);
    send_command(
        &state,
        Command::RunAnalysis {
            id,
            request: wavecore::analysis::AnalysisRequest::CompareSpectra,
        },
    )
}

#[tauri::command]
pub fn capture_reference(state: State<'_, AppState>) -> Result<(), String> {
    send_command(&state, Command::CaptureReference)
}

#[tauri::command]
pub fn toggle_tracking(state: State<'_, AppState>) -> Result<bool, String> {
    let was_active = state.tracking_active.load(Ordering::Relaxed);
    let now_active = !was_active;
    // Send command first — only flip state if it succeeds
    if now_active {
        send_command(&state, Command::StartTracking)?;
    } else {
        send_command(&state, Command::StopTracking)?;
    }
    state.tracking_active.store(now_active, Ordering::Relaxed);
    Ok(now_active)
}

#[tauri::command]
pub fn export_data(format: String, path: String, state: State<'_, AppState>) -> Result<(), String> {
    let cfg = state.session_config.lock().clone().ok_or("Not connected")?;
    let fmt = match format.as_str() {
        "json" => wavecore::analysis::export::ExportFormat::Json,
        _ => wavecore::analysis::export::ExportFormat::Csv,
    };
    let export_config = wavecore::analysis::export::ExportConfig {
        path: PathBuf::from(path),
        format: fmt,
        content: wavecore::analysis::export::ExportContent::Spectrum {
            spectrum_db: Vec::new(), // manager substitutes latest data
            sample_rate: cfg.sample_rate,
            center_freq: cfg.frequency,
        },
    };
    send_command(&state, Command::Export(export_config))
}

// ============================================================================
// Timeline / annotation commands
// ============================================================================

#[tauri::command]
pub fn add_annotation(
    kind: String,
    text: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    send_command(&state, Command::AddAnnotation { kind, text })
}

#[tauri::command]
pub fn export_timeline(
    path: String,
    format: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let fmt = match format.as_str() {
        "csv" => wavecore::session::TimelineExportFormat::Csv,
        _ => wavecore::session::TimelineExportFormat::Json,
    };
    send_command(
        &state,
        Command::ExportTimeline {
            path: PathBuf::from(path),
            format: fmt,
        },
    )
}

fn build_decoder_registry() -> DecoderRegistry {
    let mut registry = DecoderRegistry::new();
    wavecore::dsp::decoders::register_all(&mut registry);
    registry
}
