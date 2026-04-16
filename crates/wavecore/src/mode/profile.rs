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
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub decoder: Option<String>,
    #[serde(default)]
    pub priority: bool,
    #[serde(default)]
    pub locked_out: bool,
    #[serde(default)]
    pub notes: Option<String>,
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
const APRS_TOML: &str = include_str!("profiles/aprs.toml");
const AIS_TOML: &str = include_str!("profiles/ais.toml");
const AIRBAND_TOML: &str = include_str!("profiles/airband.toml");
const PAGER_TOML: &str = include_str!("profiles/pager.toml");
const FM_BROADCAST_TOML: &str = include_str!("profiles/fm-broadcast.toml");
const FM_SURVEY_TOML: &str = include_str!("profiles/fm-survey.toml");
const ISM_SENSOR_HUNT_TOML: &str = include_str!("profiles/ism-sensor-hunt.toml");
const NOAA_APT_TOML: &str = include_str!("profiles/noaa-apt.toml");

/// Load all embedded (compiled-in) profiles.
pub fn load_embedded_profiles() -> Vec<MissionProfile> {
    let mut profiles = Vec::new();
    for toml_str in [
        AVIATION_TOML,
        APRS_TOML,
        AIS_TOML,
        AIRBAND_TOML,
        PAGER_TOML,
        FM_BROADCAST_TOML,
        FM_SURVEY_TOML,
        ISM_SENSOR_HUNT_TOML,
        NOAA_APT_TOML,
    ] {
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
    Monitoring { index: usize },
    /// Cycling through multiple frequencies.
    Cycling { slot: usize, dwell_start: Instant },
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

    fn effective_decoders(&self, entry: &FrequencyEntry) -> Vec<String> {
        if entry.locked_out {
            Vec::new()
        } else if let Some(ref decoder) = entry.decoder {
            vec![decoder.clone()]
        } else {
            self.profile.decoders.clone()
        }
    }

    fn effective_demod(&self, entry: &FrequencyEntry) -> Option<ProfileDemod> {
        if let Some(ref mode) = entry.mode {
            Some(ProfileDemod {
                mode: mode.clone(),
                bandwidth: self.profile.demod.as_ref().and_then(|d| d.bandwidth),
                squelch: self.profile.demod.as_ref().and_then(|d| d.squelch),
            })
        } else {
            self.profile.demod.clone()
        }
    }

    /// Build initial setup commands: tune, enable decoders, start demod.
    fn setup_commands(&self, entry: &FrequencyEntry) -> Vec<Command> {
        let mut cmds = vec![Command::Tune(entry.freq_hz)];

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

        for decoder in self.effective_decoders(entry) {
            cmds.push(Command::EnableDecoder(decoder.clone()));
        }

        if let Some(demod) = self.effective_demod(entry) {
            cmds.push(Command::StartDemod(DemodConfig {
                mode: demod.mode,
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

    fn teardown_commands(&self, entry: &FrequencyEntry) -> Vec<Command> {
        let mut cmds = Vec::new();
        if self.effective_demod(entry).is_some() {
            cmds.push(Command::StopDemod);
        }
        for decoder in self.effective_decoders(entry) {
            cmds.push(Command::DisableDecoder(decoder));
        }
        cmds
    }

    fn active_frequency_count(&self) -> usize {
        self.profile
            .frequencies
            .iter()
            .filter(|entry| !entry.locked_out)
            .count()
    }

    fn active_indices(&self) -> Vec<usize> {
        self.profile
            .frequencies
            .iter()
            .enumerate()
            .filter_map(|(idx, entry)| (!entry.locked_out).then_some(idx))
            .collect()
    }

    fn first_active_index(&self) -> Option<usize> {
        self.active_indices().into_iter().next()
    }

    /// Build the cycling order, revisiting priority entries more often.
    fn cycle_schedule(&self) -> Vec<usize> {
        let active = self.active_indices();
        if active.len() <= 1 {
            return active;
        }

        let priorities: Vec<usize> = active
            .iter()
            .copied()
            .filter(|idx| self.profile.frequencies[*idx].priority)
            .collect();

        if priorities.is_empty() || priorities.len() == active.len() {
            return active;
        }

        let mut schedule = active;
        schedule.extend(priorities);
        schedule
    }

    /// Check if this profile is single-frequency (all entries are monitor=true, or only one entry).
    fn is_single_freq(&self) -> bool {
        self.active_frequency_count() <= 1
            || self
                .profile
                .frequencies
                .iter()
                .filter(|entry| !entry.locked_out)
                .all(|entry| entry.monitor)
    }
}

impl Mode for ProfileMode {
    fn name(&self) -> &str {
        &self.profile.name
    }

    fn status(&self) -> String {
        match &self.state {
            ProfileState::Init => format!("{}: initializing", self.profile.name),
            ProfileState::Monitoring { index } => {
                let label = self.profile.frequencies[*index].label.as_str();
                format!("{}: monitoring {}", self.profile.name, label)
            }
            ProfileState::Cycling { slot, .. } => {
                let schedule = self.cycle_schedule();
                let Some(&index) = schedule.get(*slot) else {
                    return format!("{}: cycling", self.profile.name);
                };
                let entry = &self.profile.frequencies[index];
                let priority_marker = if entry.priority { " [priority]" } else { "" };
                format!(
                    "{}: cycling [{}/{}] {}{}",
                    self.profile.name,
                    slot + 1,
                    schedule.len(),
                    entry.label,
                    priority_marker,
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
                    let (path, rec_fmt) = match auto_rec.format.as_str() {
                        "wav" => (
                            std::path::PathBuf::from(&auto_rec.output_dir).join(format!(
                                "{}-{}.wav",
                                self.profile.name,
                                chrono_timestamp(),
                            )),
                            crate::session::RecordFormat::Wav,
                        ),
                        "sigmf" => (
                            std::path::PathBuf::from(&auto_rec.output_dir).join(format!(
                                "{}-{}",
                                self.profile.name,
                                chrono_timestamp()
                            )),
                            crate::session::RecordFormat::SigMf,
                        ),
                        _ => (
                            std::path::PathBuf::from(&auto_rec.output_dir).join(format!(
                                "{}-{}.cf32",
                                self.profile.name,
                                chrono_timestamp(),
                            )),
                            crate::session::RecordFormat::RawCf32,
                        ),
                    };
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

                if self.is_single_freq() {
                    let Some(first_index) = self.first_active_index() else {
                        return Vec::new();
                    };
                    let first_entry = &self.profile.frequencies[first_index];
                    let cmds = self.setup_commands(first_entry);
                    self.state = ProfileState::Monitoring { index: first_index };
                    cmds
                } else {
                    let schedule = self.cycle_schedule();
                    let Some(&first_index) = schedule.first() else {
                        return Vec::new();
                    };
                    let first_entry = &self.profile.frequencies[first_index];
                    let cmds = self.setup_commands(first_entry);
                    self.state = ProfileState::Cycling {
                        slot: 0,
                        dwell_start: Instant::now(),
                    };
                    cmds
                }
            }
            ProfileState::Monitoring { .. } => Vec::new(),
            ProfileState::Cycling { slot, dwell_start } => {
                let schedule = self.cycle_schedule();
                if schedule.len() <= 1 {
                    return Vec::new();
                }

                let Some(&current_index) = schedule.get(*slot) else {
                    return Vec::new();
                };
                let entry = &self.profile.frequencies[current_index];
                let dwell = entry.dwell_ms.unwrap_or(3000);

                if dwell_start.elapsed().as_millis() >= dwell as u128 {
                    let next_slot = (*slot + 1) % schedule.len();
                    let next_index = schedule[next_slot];
                    let next_entry = &self.profile.frequencies[next_index];

                    self.state = ProfileState::Cycling {
                        slot: next_slot,
                        dwell_start: Instant::now(),
                    };

                    let mut cmds = self.teardown_commands(entry);
                    cmds.extend(self.setup_commands(next_entry));
                    cmds
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
    fn profile_mode_revisits_priority_entries() {
        let profile = MissionProfile {
            schema_version: 1,
            name: "priority-test".to_string(),
            description: "priority scheduling".to_string(),
            frequencies: vec![
                FrequencyEntry {
                    freq_hz: 100_000_000.0,
                    label: "A".to_string(),
                    monitor: false,
                    dwell_ms: Some(0),
                    mode: None,
                    decoder: None,
                    priority: false,
                    locked_out: false,
                    notes: None,
                },
                FrequencyEntry {
                    freq_hz: 101_000_000.0,
                    label: "B".to_string(),
                    monitor: false,
                    dwell_ms: Some(0),
                    mode: None,
                    decoder: None,
                    priority: true,
                    locked_out: false,
                    notes: None,
                },
                FrequencyEntry {
                    freq_hz: 102_000_000.0,
                    label: "C".to_string(),
                    monitor: false,
                    dwell_ms: Some(0),
                    mode: None,
                    decoder: None,
                    priority: false,
                    locked_out: false,
                    notes: None,
                },
            ],
            decoders: Vec::new(),
            sample_rate: None,
            gain: None,
            demod: None,
            auto_record: None,
        };

        let mut mode = ProfileMode::new(profile);

        let init = mode.tick();
        assert!(
            init.iter().any(
                |cmd| matches!(cmd, Command::Tune(freq) if (*freq - 100_000_000.0).abs() < 1.0)
            )
        );

        let first_hop = mode.tick();
        assert!(
            first_hop.iter().any(
                |cmd| matches!(cmd, Command::Tune(freq) if (*freq - 101_000_000.0).abs() < 1.0)
            )
        );

        let second_hop = mode.tick();
        assert!(
            second_hop.iter().any(
                |cmd| matches!(cmd, Command::Tune(freq) if (*freq - 102_000_000.0).abs() < 1.0)
            )
        );

        let third_hop = mode.tick();
        assert!(
            third_hop.iter().any(
                |cmd| matches!(cmd, Command::Tune(freq) if (*freq - 101_000_000.0).abs() < 1.0)
            )
        );
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
        assert_eq!(profiles.len(), 9);
        let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"aviation"));
        assert!(names.contains(&"aprs"));
        assert!(names.contains(&"ais-watch"));
        assert!(names.contains(&"airband-voice"));
        assert!(names.contains(&"pager"));
        assert!(names.contains(&"fm-broadcast"));
        assert!(names.contains(&"fm-survey"));
        assert!(names.contains(&"ism-sensor-hunt"));
        assert!(names.contains(&"noaa-apt"));
    }

    #[test]
    fn invalid_toml_handled_gracefully() {
        let result = toml::from_str::<MissionProfile>("not valid toml {{{}");
        assert!(result.is_err());
    }

    #[test]
    fn entry_overrides_apply_decoder_and_mode() {
        let profile = MissionProfile {
            schema_version: 1,
            name: "test".to_string(),
            description: "test".to_string(),
            frequencies: vec![FrequencyEntry {
                freq_hz: 144_390_000.0,
                label: "APRS".to_string(),
                monitor: true,
                dwell_ms: None,
                mode: Some("fm".to_string()),
                decoder: Some("aprs".to_string()),
                priority: false,
                locked_out: false,
                notes: None,
            }],
            decoders: vec!["adsb".to_string()],
            sample_rate: Some(2_048_000.0),
            gain: None,
            demod: Some(ProfileDemod {
                mode: "am".to_string(),
                bandwidth: None,
                squelch: None,
            }),
            auto_record: None,
        };

        let mut mode = ProfileMode::new(profile);
        let cmds = mode.tick();
        assert!(
            cmds.iter()
                .any(|cmd| matches!(cmd, Command::EnableDecoder(name) if name == "aprs"))
        );
        assert!(
            cmds.iter()
                .any(|cmd| matches!(cmd, Command::StartDemod(cfg) if cfg.mode == "fm"))
        );
    }
}
