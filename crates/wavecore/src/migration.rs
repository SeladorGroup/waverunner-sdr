//! Schema version checking and migration helpers.
//!
//! All persisted formats (SessionConfig, RecordingMetadata, MissionProfile,
//! SessionCheckpoint, timeline exports) carry a `schema_version` field.
//! This module provides version validation and forward-migration stubs.

/// Current schema version for all WaveRunner formats.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Result of a schema version check.
#[derive(Debug, PartialEq)]
pub enum SchemaCheck {
    /// Version matches current — no migration needed.
    Current,
    /// Version is older — migration may be possible.
    NeedsMigration { from: u32, to: u32 },
    /// Version is newer — produced by a newer WaveRunner release.
    TooNew { found: u32, max: u32 },
}

/// Check a schema version against the current expected version.
///
/// Returns `SchemaCheck::Current` if versions match,
/// `SchemaCheck::NeedsMigration` if the found version is older,
/// or `SchemaCheck::TooNew` if the found version is newer.
pub fn check_schema_version(found: u32) -> SchemaCheck {
    match found.cmp(&CURRENT_SCHEMA_VERSION) {
        std::cmp::Ordering::Equal => SchemaCheck::Current,
        std::cmp::Ordering::Less => SchemaCheck::NeedsMigration {
            from: found,
            to: CURRENT_SCHEMA_VERSION,
        },
        std::cmp::Ordering::Greater => SchemaCheck::TooNew {
            found,
            max: CURRENT_SCHEMA_VERSION,
        },
    }
}

/// Attempt to migrate a SessionConfig JSON value from an older schema version.
///
/// Currently only v1 exists, so this is a no-op placeholder for future
/// migration paths (e.g., v1→v2 field renames, type changes).
pub fn migrate_session_config(
    value: serde_json::Value,
    from_version: u32,
) -> Result<serde_json::Value, String> {
    match from_version {
        0 => {
            // Pre-versioned config (treated as v1 with defaults)
            let mut obj = value;
            if let Some(map) = obj.as_object_mut() {
                map.insert(
                    "schema_version".to_string(),
                    serde_json::Value::Number(1.into()),
                );
            }
            Ok(obj)
        }
        1 => Ok(value), // Current version, no migration needed
        v => Err(format!(
            "Cannot migrate SessionConfig from version {v} (current: {CURRENT_SCHEMA_VERSION})"
        )),
    }
}

/// Attempt to migrate a RecordingMetadata JSON value from an older schema version.
pub fn migrate_recording_metadata(
    value: serde_json::Value,
    from_version: u32,
) -> Result<serde_json::Value, String> {
    match from_version {
        0 => {
            let mut obj = value;
            if let Some(map) = obj.as_object_mut() {
                map.insert(
                    "schema_version".to_string(),
                    serde_json::Value::Number(1.into()),
                );
            }
            Ok(obj)
        }
        1 => Ok(value),
        v => Err(format!(
            "Cannot migrate RecordingMetadata from version {v} (current: {CURRENT_SCHEMA_VERSION})"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_current_version() {
        assert_eq!(
            check_schema_version(CURRENT_SCHEMA_VERSION),
            SchemaCheck::Current,
        );
    }

    #[test]
    fn check_older_version() {
        assert_eq!(
            check_schema_version(0),
            SchemaCheck::NeedsMigration { from: 0, to: 1 },
        );
    }

    #[test]
    fn check_newer_version_rejected() {
        assert_eq!(
            check_schema_version(99),
            SchemaCheck::TooNew { found: 99, max: 1 },
        );
    }

    #[test]
    fn migrate_session_config_v0() {
        let json = serde_json::json!({
            "device_index": 0,
            "frequency": 100e6,
            "sample_rate": 2.048e6,
            "gain": "Auto",
            "ppm": 0,
            "fft_size": 2048,
            "pfa": 0.0001,
        });

        let migrated = migrate_session_config(json, 0).unwrap();
        assert_eq!(migrated["schema_version"], 1);
        assert_eq!(migrated["device_index"], 0);
    }

    #[test]
    fn migrate_session_config_v1_noop() {
        let json = serde_json::json!({
            "schema_version": 1,
            "device_index": 0,
            "frequency": 100e6,
        });

        let migrated = migrate_session_config(json.clone(), 1).unwrap();
        assert_eq!(migrated, json);
    }

    #[test]
    fn migrate_session_config_future_version_errors() {
        let json = serde_json::json!({"schema_version": 99});
        let result = migrate_session_config(json, 99);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Cannot migrate"));
    }

    #[test]
    fn session_config_missing_version_defaults_v1() {
        // Old JSON without schema_version should deserialize with default v1
        let json = r#"{
            "device_index": 0,
            "frequency": 100000000.0,
            "sample_rate": 2048000.0,
            "gain": "Auto",
            "ppm": 0,
            "fft_size": 2048,
            "pfa": 0.0001
        }"#;
        let config: crate::session::SessionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.schema_version, 1);
    }

    #[test]
    fn recording_metadata_version_field() {
        let json = r#"{
            "center_freq": 433920000.0,
            "sample_rate": 2048000.0,
            "gain": "auto",
            "format": "cf32",
            "timestamp": "2026-02-14T12:00:00Z",
            "duration_secs": 10.0,
            "device": "RTL-SDR",
            "samples_written": 1000
        }"#;
        let meta: crate::recording::RecordingMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(meta.schema_version, 1);
    }

    #[test]
    fn profile_version_field() {
        // Profile without schema_version should default to 1
        let toml = r#"
            name = "test"
            description = "test profile"
            [[frequencies]]
            freq_hz = 100000000.0
            label = "test"
        "#;
        let profile: crate::mode::profile::MissionProfile = toml::from_str(toml).unwrap();
        assert_eq!(profile.schema_version, 1);
    }

    #[test]
    fn migrate_recording_metadata_v0() {
        let json = serde_json::json!({
            "center_freq": 100e6,
            "sample_rate": 2.048e6,
        });
        let migrated = migrate_recording_metadata(json, 0).unwrap();
        assert_eq!(migrated["schema_version"], 1);
    }
}
