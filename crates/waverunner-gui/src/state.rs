use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;
use std::time::Instant;

use parking_lot::Mutex;
use wavecore::captures::CaptureSource;
use wavecore::mode::ModeController;
use wavecore::session::SessionConfig;
use wavecore::session::manager::SessionManager;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionOrigin {
    LiveRtlSdr,
    Replay,
}

impl SessionOrigin {
    pub fn device_name(self) -> &'static str {
        match self {
            SessionOrigin::LiveRtlSdr => "rtlsdr",
            SessionOrigin::Replay => "replay",
        }
    }

    pub fn capture_source(self) -> CaptureSource {
        match self {
            SessionOrigin::LiveRtlSdr => CaptureSource::LiveRecord,
            SessionOrigin::Replay => CaptureSource::ReplayExport,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RecordingContext {
    pub path: std::path::PathBuf,
    pub started_at: Instant,
    pub center_freq: f64,
    pub sample_rate: f64,
    pub gain: String,
    pub format: String,
    pub device: String,
    pub source: CaptureSource,
    pub label: Option<String>,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    pub demod_mode: Option<String>,
    pub decoder: Option<String>,
    pub started: bool,
}

/// Shared application state managed by Tauri.
pub struct AppState {
    pub session: Mutex<Option<SessionManager>>,
    pub bridge_handle: Mutex<Option<JoinHandle<()>>>,
    pub bridge_running: Arc<AtomicBool>,
    pub session_start: Mutex<Option<Instant>>,
    pub mode_controller: Arc<Mutex<Option<ModeController>>>,
    pub tracking_active: AtomicBool,
    pub analysis_counter: std::sync::atomic::AtomicU64,
    pub session_config: Arc<Mutex<Option<SessionConfig>>>,
    pub session_origin: Arc<Mutex<Option<SessionOrigin>>>,
    pub recording_context: Arc<Mutex<Option<RecordingContext>>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            session: Mutex::new(None),
            bridge_handle: Mutex::new(None),
            bridge_running: Arc::new(AtomicBool::new(false)),
            session_start: Mutex::new(None),
            mode_controller: Arc::new(Mutex::new(None)),
            tracking_active: AtomicBool::new(false),
            analysis_counter: std::sync::atomic::AtomicU64::new(1),
            session_config: Arc::new(Mutex::new(None)),
            session_origin: Arc::new(Mutex::new(None)),
            recording_context: Arc::new(Mutex::new(None)),
        }
    }
}
