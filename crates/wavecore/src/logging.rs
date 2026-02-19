//! Structured logging helpers for WaveRunner.
//!
//! Provides session ID generation and a logging field contract
//! for consistent structured log output across all components.
//!
//! ## Field Contract
//!
//! All structured log events include:
//! - `timestamp` — ISO 8601 (provided by tracing-subscriber)
//! - `level` — TRACE/DEBUG/INFO/WARN/ERROR
//! - `session_id` — 8-char hex identifier, unique per session
//! - `component` — subsystem name (manager, decoder, demod, etc.)
//! - `event` — machine-readable event code (e.g., "block_processed")
//! - `message` — human-readable description

use std::sync::atomic::{AtomicU64, Ordering};

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a new session ID (8-char hex).
///
/// Uses a combination of process ID, monotonic counter, and timestamp
/// to produce short, unique-enough identifiers for log correlation.
pub fn new_session_id() -> String {
    let pid = std::process::id();
    let counter = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u32;
    format!("{:04x}{:04x}", pid.wrapping_add(counter as u32) & 0xFFFF, ts & 0xFFFF)
}

/// Known component names for structured logging.
pub mod components {
    pub const MANAGER: &str = "manager";
    pub const DECODER: &str = "decoder";
    pub const DEMOD: &str = "demod";
    pub const HARDWARE: &str = "hardware";
    pub const PIPELINE: &str = "pipeline";
    pub const ANALYSIS: &str = "analysis";
    pub const RECORDING: &str = "recording";
    pub const MODE: &str = "mode";
    pub const EXPORT: &str = "export";
    pub const CLI: &str = "cli";
    pub const TUI: &str = "tui";
    pub const GUI: &str = "gui";
}

/// Known event codes for structured logging.
pub mod events {
    pub const SESSION_START: &str = "session_start";
    pub const SESSION_STOP: &str = "session_stop";
    pub const BLOCK_PROCESSED: &str = "block_processed";
    pub const LOAD_SHEDDING: &str = "load_shedding";
    pub const HEALTH_CHANGED: &str = "health_changed";
    pub const DECODER_ENABLED: &str = "decoder_enabled";
    pub const DECODER_DISABLED: &str = "decoder_disabled";
    pub const RECORDING_START: &str = "recording_start";
    pub const RECORDING_STOP: &str = "recording_stop";
    pub const CHECKPOINT_SAVED: &str = "checkpoint_saved";
    pub const EXPORT_COMPLETE: &str = "export_complete";
    pub const TUNE: &str = "tune";
    pub const GAIN_CHANGE: &str = "gain_change";
    pub const ERROR: &str = "error";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_format() {
        let id = new_session_id();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn session_id_unique() {
        let ids: Vec<String> = (0..100).map(|_| new_session_id()).collect();
        // With monotonic counter, consecutive IDs should differ
        for window in ids.windows(2) {
            assert_ne!(window[0], window[1]);
        }
    }

    #[test]
    fn component_names_non_empty() {
        assert!(!components::MANAGER.is_empty());
        assert!(!components::DECODER.is_empty());
        assert!(!components::PIPELINE.is_empty());
    }

    #[test]
    fn event_codes_non_empty() {
        assert!(!events::SESSION_START.is_empty());
        assert!(!events::BLOCK_PROCESSED.is_empty());
        assert!(!events::ERROR.is_empty());
    }
}
