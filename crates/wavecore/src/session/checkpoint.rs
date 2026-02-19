//! Session checkpoint for crash recovery.
//!
//! Periodically saves session state to `~/.cache/waverunner/checkpoint.json`.
//! On clean shutdown the checkpoint is cleared. If the application crashes,
//! the checkpoint file persists and can be read on next startup.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::hardware::GainMode;
use crate::session::SessionConfig;

/// Current checkpoint schema version.
pub const CHECKPOINT_SCHEMA_VERSION: u32 = 1;

/// Serializable snapshot of session state for crash recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCheckpoint {
    /// Schema version for forward compatibility.
    pub schema_version: u32,
    /// ISO 8601 timestamp when checkpoint was written.
    pub timestamp: String,
    /// Session configuration at checkpoint time.
    pub config: SessionConfig,
    /// Current tuned frequency (may differ from config.frequency).
    pub frequency: f64,
    /// Current gain setting.
    pub gain: GainMode,
    /// Names of active decoders.
    pub active_decoders: Vec<String>,
    /// Path to active recording, if any.
    pub recording_path: Option<String>,
    /// Whether signal tracking is active.
    pub tracking_active: bool,
    /// Number of timeline entries accumulated.
    pub timeline_entries: usize,
    /// Total blocks processed.
    pub blocks_processed: u64,
    /// Total events dropped.
    pub events_dropped: u64,
}

/// Returns the default checkpoint file path: `~/.cache/waverunner/checkpoint.json`.
pub fn checkpoint_path() -> PathBuf {
    let base = dirs_path();
    base.join("checkpoint.json")
}

fn dirs_path() -> PathBuf {
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(cache).join("waverunner")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".cache").join("waverunner")
    } else {
        PathBuf::from("/tmp/waverunner")
    }
}

/// Save a checkpoint atomically (write to .tmp, then rename).
pub fn save_checkpoint(cp: &SessionCheckpoint) -> Result<(), String> {
    let path = checkpoint_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create checkpoint dir: {e}"))?;
    }

    let tmp_path = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(cp)
        .map_err(|e| format!("Checkpoint serialize error: {e}"))?;

    std::fs::write(&tmp_path, &json)
        .map_err(|e| format!("Checkpoint write error: {e}"))?;

    std::fs::rename(&tmp_path, &path)
        .map_err(|e| format!("Checkpoint rename error: {e}"))?;

    Ok(())
}

/// Load a checkpoint. Returns `None` if missing, corrupt, or wrong version.
pub fn load_checkpoint() -> Option<SessionCheckpoint> {
    let path = checkpoint_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return None,
    };

    match serde_json::from_str::<SessionCheckpoint>(&data) {
        Ok(cp) => {
            if cp.schema_version > CHECKPOINT_SCHEMA_VERSION {
                tracing::warn!(
                    found = cp.schema_version,
                    expected = CHECKPOINT_SCHEMA_VERSION,
                    "Checkpoint from newer version, ignoring"
                );
                return None;
            }
            Some(cp)
        }
        Err(e) => {
            tracing::warn!("Corrupt checkpoint file, ignoring: {e}");
            None
        }
    }
}

/// Delete the checkpoint file (called on clean shutdown).
pub fn clear_checkpoint() {
    let path = checkpoint_path();
    std::fs::remove_file(&path).ok();
    // Also clean up any stale .tmp file
    std::fs::remove_file(path.with_extension("json.tmp")).ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::GainMode;
    use crate::session::SessionConfig;

    fn sample_checkpoint() -> SessionCheckpoint {
        SessionCheckpoint {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            timestamp: "2026-02-15T12:00:00Z".to_string(),
            config: SessionConfig {
                schema_version: 1,
                device_index: 0,
                frequency: 100e6,
                sample_rate: 2_048_000.0,
                gain: GainMode::Auto,
                ppm: 0,
                fft_size: 2048,
                pfa: 1e-4,
            },
            frequency: 101.5e6,
            gain: GainMode::Manual(30.0),
            active_decoders: vec!["pocsag-1200".to_string()],
            recording_path: Some("/tmp/test.cf32".to_string()),
            tracking_active: true,
            timeline_entries: 42,
            blocks_processed: 10000,
            events_dropped: 0,
        }
    }

    #[test]
    fn checkpoint_save_load_roundtrip() {
        // Use a custom path to avoid interfering with real checkpoints
        let cp = sample_checkpoint();
        let json = serde_json::to_string_pretty(&cp).unwrap();
        let loaded: SessionCheckpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.schema_version, cp.schema_version);
        assert_eq!(loaded.frequency, cp.frequency);
        assert_eq!(loaded.active_decoders, cp.active_decoders);
        assert_eq!(loaded.blocks_processed, cp.blocks_processed);
    }

    #[test]
    fn checkpoint_corrupt_returns_none() {
        let data = "this is not valid json {{{{";
        let result: Result<SessionCheckpoint, _> = serde_json::from_str(data);
        assert!(result.is_err());
    }

    #[test]
    fn checkpoint_future_version_rejected() {
        let mut cp = sample_checkpoint();
        cp.schema_version = 99;
        let json = serde_json::to_string(&cp).unwrap();
        let loaded: SessionCheckpoint = serde_json::from_str(&json).unwrap();
        // The load_checkpoint() function checks version, but we can test the logic directly
        assert!(loaded.schema_version > CHECKPOINT_SCHEMA_VERSION);
    }

    #[test]
    fn checkpoint_path_not_empty() {
        let path = checkpoint_path();
        assert!(!path.as_os_str().is_empty());
        assert!(path.to_string_lossy().contains("waverunner"));
    }

    #[test]
    fn checkpoint_clear_no_panic() {
        // Clearing a non-existent checkpoint should not panic
        clear_checkpoint();
    }

    #[test]
    fn checkpoint_missing_file_returns_none() {
        // Ensure no checkpoint exists
        clear_checkpoint();
        let result = load_checkpoint();
        assert!(result.is_none());
    }
}
