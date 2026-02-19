use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use parking_lot::Mutex;
use wavecore::mode::ModeController;
use wavecore::session::SessionConfig;
use wavecore::session::manager::SessionManager;

/// Shared application state managed by Tauri.
pub struct AppState {
    pub session: Mutex<Option<SessionManager>>,
    pub bridge_handle: Mutex<Option<JoinHandle<()>>>,
    pub bridge_running: Arc<AtomicBool>,
    pub session_start: Mutex<Option<Instant>>,
    pub mode_controller: Arc<Mutex<Option<ModeController>>>,
    pub tracking_active: AtomicBool,
    pub analysis_counter: std::sync::atomic::AtomicU64,
    pub session_config: Mutex<Option<SessionConfig>>,
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
            session_config: Mutex::new(None),
        }
    }
}
