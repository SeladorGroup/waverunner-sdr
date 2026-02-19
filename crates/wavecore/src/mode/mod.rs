//! Mode system for orchestrating SessionManager operations.
//!
//! Modes are behavioral layers that consume Events, apply decision logic,
//! and return Commands. They sit between the frontend and SessionManager,
//! never touching DSP internals.
//!
//! ```text
//! Frontend ──→ ModeController ──Command──→ SessionManager
//!                   ↑                           │
//!                   └──── handle_event() ←──Event──┘
//! ```

pub mod classifier;
pub mod general;
pub mod profile;

use crate::hardware::GainMode;
use crate::session::{Command, Event};

/// Behavioral layer that orchestrates SessionManager commands.
pub trait Mode: Send {
    /// Name of this mode (e.g. "general", "aviation").
    fn name(&self) -> &str;
    /// Human-readable status string for display.
    fn status(&self) -> String;
    /// React to a SessionManager event. Returns commands to send back.
    fn handle_event(&mut self, event: &Event) -> Vec<Command>;
    /// Periodic tick (~30 Hz from UI loop). Returns commands for time-based transitions.
    fn tick(&mut self) -> Vec<Command>;
    /// Reset mode state to initial.
    fn reset(&mut self);
}

/// Manages active mode. Frontend-agnostic — works in CLI, TUI, and GUI.
pub struct ModeController {
    active: Option<Box<dyn Mode>>,
    profiles: Vec<profile::MissionProfile>,
    available_decoders: Vec<String>,
}

impl ModeController {
    /// Create a new ModeController with the list of available decoder names.
    pub fn new(available_decoders: Vec<String>) -> Self {
        let mut ctrl = Self {
            active: None,
            profiles: Vec::new(),
            available_decoders,
        };
        ctrl.load_profiles();
        ctrl
    }

    /// Load embedded and user TOML profiles.
    pub fn load_profiles(&mut self) {
        self.profiles = profile::load_embedded_profiles();

        // Load user profiles from ~/.config/waverunner/profiles/
        if let Some(config_dir) = dirs_path() {
            let user_dir = config_dir.join("profiles");
            if user_dir.is_dir() {
                self.profiles
                    .extend(profile::load_user_profiles(&user_dir));
            }
        }
    }

    /// Activate a mode. Returns initial commands from the mode's first tick.
    pub fn activate(&mut self, mode: Box<dyn Mode>) -> Vec<Command> {
        self.active = Some(mode);
        // Give the mode its first tick to emit setup commands
        if let Some(ref mut m) = self.active {
            m.tick()
        } else {
            Vec::new()
        }
    }

    /// Deactivate the current mode. Returns cleanup commands.
    pub fn deactivate(&mut self) -> Vec<Command> {
        let mut cmds = Vec::new();
        if self.active.take().is_some() {
            // Stop any decoders and demod that the mode may have started
            for decoder in &self.available_decoders {
                cmds.push(Command::DisableDecoder(decoder.clone()));
            }
            cmds.push(Command::StopDemod);
        }
        cmds
    }

    /// Forward an event to the active mode.
    pub fn handle_event(&mut self, event: &Event) -> Vec<Command> {
        if let Some(ref mut mode) = self.active {
            mode.handle_event(event)
        } else {
            Vec::new()
        }
    }

    /// Periodic tick — delegate to active mode.
    pub fn tick(&mut self) -> Vec<Command> {
        if let Some(ref mut mode) = self.active {
            mode.tick()
        } else {
            Vec::new()
        }
    }

    /// Name of the active mode, if any.
    pub fn active_mode(&self) -> Option<&str> {
        self.active.as_ref().map(|m| m.name())
    }

    /// Status string from the active mode, if any.
    pub fn mode_status(&self) -> Option<String> {
        self.active.as_ref().map(|m| m.status())
    }

    /// List available profile names.
    pub fn list_profiles(&self) -> Vec<&str> {
        self.profiles.iter().map(|p| p.name.as_str()).collect()
    }

    /// Get a reference to a loaded profile by name.
    pub fn get_profile(&self, name: &str) -> Option<&profile::MissionProfile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// Create a ProfileMode from a named profile.
    pub fn create_profile_mode(&self, name: &str) -> Option<Box<dyn Mode>> {
        self.get_profile(name).map(|p| {
            Box::new(profile::ProfileMode::new(p.clone())) as Box<dyn Mode>
        })
    }

    /// Create a ProfileMode with an optional gain override (e.g. from CLI --gain).
    pub fn create_profile_mode_with_gain(
        &self,
        name: &str,
        gain_override: Option<GainMode>,
    ) -> Option<Box<dyn Mode>> {
        self.get_profile(name).map(|p| {
            Box::new(profile::ProfileMode::with_gain_override(p.clone(), gain_override))
                as Box<dyn Mode>
        })
    }
}

/// Get the config directory path (~/.config/waverunner).
fn dirs_path() -> Option<std::path::PathBuf> {
    // Use XDG_CONFIG_HOME or fallback to ~/.config
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        Some(std::path::PathBuf::from(xdg).join("waverunner"))
    } else if let Ok(home) = std::env::var("HOME") {
        Some(
            std::path::PathBuf::from(home)
                .join(".config")
                .join("waverunner"),
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Event;

    /// A simple test mode for verifying ModeController behavior.
    struct TestMode {
        tick_count: u32,
    }

    impl TestMode {
        fn new() -> Self {
            Self { tick_count: 0 }
        }
    }

    impl Mode for TestMode {
        fn name(&self) -> &str {
            "test"
        }

        fn status(&self) -> String {
            format!("ticks: {}", self.tick_count)
        }

        fn handle_event(&mut self, event: &Event) -> Vec<Command> {
            match event {
                Event::Error(_) => vec![Command::StopDemod],
                _ => Vec::new(),
            }
        }

        fn tick(&mut self) -> Vec<Command> {
            self.tick_count += 1;
            if self.tick_count == 1 {
                vec![Command::Tune(100e6)]
            } else {
                Vec::new()
            }
        }

        fn reset(&mut self) {
            self.tick_count = 0;
        }
    }

    #[test]
    fn controller_creation() {
        let decoders = vec!["pocsag".to_string(), "adsb".to_string()];
        let ctrl = ModeController::new(decoders);
        assert!(ctrl.active_mode().is_none());
        assert!(ctrl.mode_status().is_none());
    }

    #[test]
    fn activate_deactivate_lifecycle() {
        let decoders = vec!["pocsag".to_string(), "adsb".to_string()];
        let mut ctrl = ModeController::new(decoders);

        // Activate
        let cmds = ctrl.activate(Box::new(TestMode::new()));
        assert_eq!(ctrl.active_mode(), Some("test"));
        // First tick emits Tune command
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], Command::Tune(_)));

        // Deactivate
        let cmds = ctrl.deactivate();
        assert!(ctrl.active_mode().is_none());
        // Should emit DisableDecoder for each decoder + StopDemod
        assert_eq!(cmds.len(), 3); // 2 decoders + StopDemod
    }

    #[test]
    fn handle_event_delegates() {
        let mut ctrl = ModeController::new(vec![]);
        ctrl.activate(Box::new(TestMode::new()));

        let cmds = ctrl.handle_event(&Event::Error("test".to_string()));
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], Command::StopDemod));
    }

    #[test]
    fn no_commands_when_no_mode() {
        let mut ctrl = ModeController::new(vec![]);
        assert!(ctrl.handle_event(&Event::Error("x".to_string())).is_empty());
        assert!(ctrl.tick().is_empty());
    }

    #[test]
    fn deactivate_returns_cleanup() {
        let decoders = vec!["pocsag".to_string(), "adsb".to_string(), "rds".to_string()];
        let mut ctrl = ModeController::new(decoders);
        ctrl.activate(Box::new(TestMode::new()));

        let cmds = ctrl.deactivate();
        // 3 DisableDecoder + 1 StopDemod = 4
        assert_eq!(cmds.len(), 4);
        assert!(matches!(cmds[3], Command::StopDemod));
    }

    #[test]
    fn list_profiles_returns_embedded() {
        let ctrl = ModeController::new(vec![]);
        let names = ctrl.list_profiles();
        assert!(names.contains(&"aviation"));
        assert!(names.contains(&"pager"));
        assert!(names.contains(&"fm-broadcast"));
    }
}
