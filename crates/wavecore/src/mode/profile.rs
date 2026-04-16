//! Mission profile system — TOML-defined operational presets.
//!
//! Profiles define frequency lists, decoders, demod settings, and
//! optional auto-record triggers. They can be compiled-in (embedded)
//! or loaded from user config files.

use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::hardware::GainMode;
use crate::session::{Command, DemodConfig, Event};

use super::Mode;

/// A mission profile loaded from TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionProfile {
    /// Schema version for forward compatibility.
    #[serde(default = "default_schema_v1")]
    pub schema_version: u32,
    pub name: String,
    pub description: String,
    pub frequencies: Vec<FrequencyEntry>,
    #[serde(default)]
    pub decoders: Vec<String>,
    pub sample_rate: Option<f64>,
    pub gain: Option<String>,
    pub demod: Option<ProfileDemod>,
    pub auto_record: Option<AutoRecordConfig>,
}

/// A frequency entry in a mission profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrequencyEntry {
    pub freq_hz: f64,
    pub label: String,
    /// If true, stay on this frequency. If false, scan past it.
    #[serde(default = "default_true")]
    pub monitor: bool,
    /// Dwell time in milliseconds when cycling (for non-monitor frequencies).
    pub dwell_ms: Option<u64>,
}

fn default_true() -> bool {
    true
}

fn default_schema_v1() -> u32 {
    1
}

/// Demodulation settings for a profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileDemod {
    pub mode: String,
    pub bandwidth: Option<f64>,
    pub squelch: Option<f64>,
}

/// Auto-record configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoRecordConfig {
    pub snr_trigger_db: f32,
    pub format: String,
    pub output_dir: String,
}

// ============================================================================
// Embedded profiles
// ============================================================================

const AVIATION_TOML: &str = include_str!("profiles/aviation.toml");
const PAGER_TOML: &str = include_str!("profiles/pager.toml");
const FM_BROADCAST_TOML: &str = include_str!("profiles/fm-broadcast.toml");

/// Load all embedded (compiled-in) profiles.
pub fn load_embedded_profiles() -> Vec<MissionProfile> {
    let mut profiles = Vec::new();
    for toml_str in [AVIATION_TOML, PAGER_TOML, FM_BROADCAST_TOML] {
        match toml::from_str::<MissionProfile>(toml_str) {
            Ok(p) => profiles.push(p),
            Err(e) => {
                tracing::warn!("Failed to parse embedded profile: {e}");
            }
        }
    }
    profiles
}

/// Load user profiles from a directory of TOML files.
pub fn load_user_profiles(dir: &Path) -> Vec<MissionProfile> {
    let mut profiles = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return profiles,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "toml") {
            match std::fs::read_to_string(&path) {
                Ok(content) => match toml::from_str::<MissionProfile>(&content) {
                    Ok(p) => profiles.push(p),
                    Err(e) => {
                        tracing::warn!("Failed to parse {}: {e}", path.display());
                    }
                },
                Err(e) => {
                    tracing::warn!("Failed to read {}: {e}", path.display());
                }
            }
        }
    }
    profiles
}

// ============================================================================
// ProfileMode
// ============================================================================

/// Internal state for profile execution.
enum ProfileState {
    /// Initial state before first tick.
    Init,
    /// Staying on a single frequency.
    Monitoring { freq: f64 },
    /// Cycling through multiple frequencies.
    Cycling { index: usize, dwell_start: Instant },
}

/// Mode implementation that executes a MissionProfile.
pub struct ProfileMode {
    profile: MissionProfile,
    state: ProfileState,
    recording: bool,
    gain_override: Option<GainMode>,
}

impl ProfileMode {
    pub fn new(profile: MissionProfile) -> Self {
        Self {
            profile,
            state: ProfileState::Init,
            recording: false,
            gain_override: None,
        }
    }

    /// Create a ProfileMode with an optional gain override (e.g. from CLI --gain).
    pub fn with_gain_override(profile: MissionProfile, gain_override: Option<GainMode>) -> Self {
        Self {
            profile,
            state: ProfileState::Init,
            recording: false,
            gain_override,
        }
    }

    /// Build initial setup commands: tune, enable decoders, start demod.
    fn setup_commands(&self, freq: f64) -> Vec<Command> {
        let mut cmds = vec![Command::Tune(freq)];

        if let Some(rate) = self.profile.sample_rate {
            cmds.push(Command::SetSampleRate(rate));
        }

        if let Some(override_gain) = self.gain_override {
            cmds.push(Command::SetGain(override_gain));
        } else if let Some(ref gain_str) = self.profile.gain {
            if let Ok(gain) = crate::util::parse_gain(gain_str) {
                cmds.push(Command::SetGain(gain));
            }
        }

        for decoder in &self.profile.decoders {
            cmds.push(Command::EnableDecoder(decoder.clone()));
        }

        if let Some(ref demod) = self.profile.demod {
            cmds.push(Command::StartDemod(DemodConfig {
                mode: demod.mode.clone(),
                audio_rate: 48000,
                bandwidth: demod.bandwidth,
                bfo: None,
                squelch: demod.squelch,
                deemph_us: None,
                output_wav: None,
                emit_visualization: false,
                spectrum_update_interval_blocks: 8,
            }));
        }

        cmds
    }

    /// Check if this profile is single-frequency (all entries are monitor=true, or only one entry).
    fn is_single_freq(&self) -> bool {
        self.profile.frequencies.len() <= 1 || self.profile.frequencies.iter().all(|f| f.monitor)
    }
}

impl Mode for ProfileMode {
    fn name(&self) -> &str {
        &self.profile.name
    }

    fn status(&self) -> String {
        match &self.state {
            ProfileState::Init => format!("{}: initializing", self.profile.name),
            ProfileState::Monitoring { freq } => {
                let label = self
                    .profile
                    .frequencies
                    .iter()
                    .find(|f| (f.freq_hz - freq).abs() < 1.0)
                    .map(|f| f.label.as_str())
                    .unwrap_or("unknown");
                format!("{}: monitoring {}", self.profile.name, label)
            }
            ProfileState::Cycling { index, .. } => {
                let entry = &self.profile.frequencies[*index];
                format!(
                    "{}: cycling [{}/{}] {}",
                    self.profile.name,
                    index + 1,
                    self.profile.frequencies.len(),
                    entry.label,
                )
            }
        }
    }

    fn handle_event(&mut self, event: &Event) -> Vec<Command> {
        // Auto-record: check SNR threshold in detections
        if let Some(ref auto_rec) = self.profile.auto_record {
            if let Event::Detections(dets) = event {
                let strong = dets.iter().any(|d| d.snr_db >= auto_rec.snr_trigger_db);
                if strong && !self.recording {
                    self.recording = true;
                    let (ext, rec_fmt) = match auto_rec.format.as_str() {
                        "wav" => ("wav", crate::session::RecordFormat::Wav),
                        "sigmf" => ("sigmf", crate::session::RecordFormat::SigMf),
                        _ => ("cf32", crate::session::RecordFormat::RawCf32),
                    };
                    let path = std::path::PathBuf::from(&auto_rec.output_dir).join(format!(
                        "{}-{}.{}",
                        self.profile.name,
                        chrono_timestamp(),
                        ext,
                    ));
                    return vec![Command::StartRecord {
                        path,
                        format: rec_fmt,
                    }];
                } else if !strong && self.recording {
                    self.recording = false;
                    return vec![Command::StopRecord];
                }
            }
        }
        Vec::new()
    }

    fn tick(&mut self) -> Vec<Command> {
        match &self.state {
            ProfileState::Init => {
                if self.profile.frequencies.is_empty() {
                    return Vec::new();
                }

                let first_freq = self.profile.frequencies[0].freq_hz;
                let cmds = self.setup_commands(first_freq);

                if self.is_single_freq() {
                    self.state = ProfileState::Monitoring { freq: first_freq };
                } else {
                    self.state = ProfileState::Cycling {
                        index: 0,
                        dwell_start: Instant::now(),
                    };
                }
                cmds
            }
            ProfileState::Monitoring { .. } => Vec::new(),
            ProfileState::Cycling { index, dwell_start } => {
                let entry = &self.profile.frequencies[*index];
                let dwell = entry.dwell_ms.unwrap_or(3000);

                if dwell_start.elapsed().as_millis() >= dwell as u128 {
                    let next_index = (index + 1) % self.profile.frequencies.len();
                    let next_freq = self.profile.frequencies[next_index].freq_hz;

                    self.state = ProfileState::Cycling {
                        index: next_index,
                        dwell_start: Instant::now(),
                    };

                    vec![Command::Tune(next_freq)]
                } else {
                    Vec::new()
                }
            }
        }
    }

    fn reset(&mut self) {
        self.state = ProfileState::Init;
        self.recording = false;
    }
}

/// Generate a simple timestamp string for filenames.
fn chrono_timestamp() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", dur.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aviation_profile_deserializes() {
        let profile: MissionProfile = toml::from_str(AVIATION_TOML).unwrap();
        assert_eq!(profile.name, "aviation");
        assert_eq!(profile.decoders, vec!["adsb"]);
        assert_eq!(profile.frequencies.len(), 1);
        assert_eq!(profile.frequencies[0].freq_hz, 1_090_000_000.0);
        assert!(profile.frequencies[0].monitor);
    }

    #[test]
    fn pager_profile_deserializes() {
        let profile: MissionProfile = toml::from_str(PAGER_TOML).unwrap();
        assert_eq!(profile.name, "pager");
        assert_eq!(profile.decoders, vec!["pocsag"]);
        assert_eq!(profile.frequencies[0].freq_hz, 929_612_500.0);
    }

    #[test]
    fn fm_broadcast_profile_deserializes() {
        let profile: MissionProfile = toml::from_str(FM_BROADCAST_TOML).unwrap();
        assert_eq!(profile.name, "fm-broadcast");
        assert_eq!(profile.frequencies.len(), 2);
        assert!(!profile.frequencies[0].monitor);
        assert_eq!(profile.frequencies[0].dwell_ms, Some(3000));
        assert!(profile.demod.is_some());
        assert_eq!(profile.demod.unwrap().mode, "wfm");
    }

    #[test]
    fn toml_round_trip() {
        let profile: MissionProfile = toml::from_str(AVIATION_TOML).unwrap();
        let serialized = toml::to_string(&profile).unwrap();
        let deserialized: MissionProfile = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.name, profile.name);
        assert_eq!(deserialized.frequencies.len(), profile.frequencies.len());
    }

    #[test]
    fn profile_mode_monitoring_emits_setup() {
        let profile: MissionProfile = toml::from_str(AVIATION_TOML).unwrap();
        let mut mode = ProfileMode::new(profile);

        // First tick should emit setup commands
        let cmds = mode.tick();
        assert!(!cmds.is_empty());

        // Should have Tune command
        assert!(
            cmds.iter()
                .any(|c| matches!(c, Command::Tune(f) if *f == 1_090_000_000.0))
        );
        // Should have EnableDecoder
        assert!(
            cmds.iter()
                .any(|c| matches!(c, Command::EnableDecoder(d) if d == "adsb"))
        );

        // Status should show monitoring
        assert!(mode.status().contains("monitoring"));
    }

    #[test]
    fn profile_mode_cycling_steps_through() {
        let profile: MissionProfile = toml::from_str(FM_BROADCAST_TOML).unwrap();
        let mut mode = ProfileMode::new(profile);

        // First tick initializes
        let cmds = mode.tick();
        assert!(!cmds.is_empty());
        assert!(mode.status().contains("cycling"));
        assert!(mode.status().contains("1/2"));

        // Subsequent ticks before dwell shouldn't emit commands
        let cmds = mode.tick();
        assert!(cmds.is_empty());
    }

    #[test]
    fn auto_record_triggers() {
        let mut profile: MissionProfile = toml::from_str(AVIATION_TOML).unwrap();
        profile.auto_record = Some(AutoRecordConfig {
            snr_trigger_db: 15.0,
            format: "cf32".to_string(),
            output_dir: "/tmp".to_string(),
        });

        let mut mode = ProfileMode::new(profile);
        mode.tick(); // Initialize

        // Strong detection triggers recording
        let det = crate::dsp::detection::Detection {
            bin: 0,
            power_db: -20.0,
            noise_floor_db: -50.0,
            snr_db: 20.0,
            freq_offset_hz: 0.0,
        };
        let cmds = mode.handle_event(&Event::Detections(vec![det]));
        assert!(
            cmds.iter()
                .any(|c| matches!(c, Command::StartRecord { .. }))
        );
    }

    #[test]
    fn embedded_profiles_load() {
        let profiles = load_embedded_profiles();
        assert_eq!(profiles.len(), 3);
        let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"aviation"));
        assert!(names.contains(&"pager"));
        assert!(names.contains(&"fm-broadcast"));
    }

    #[test]
    fn invalid_toml_handled_gracefully() {
        let result = toml::from_str::<MissionProfile>("not valid toml {{{}");
        assert!(result.is_err());
    }
}
