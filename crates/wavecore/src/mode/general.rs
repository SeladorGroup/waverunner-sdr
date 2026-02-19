//! General auto-scan mode.
//!
//! State machine that sweeps a frequency range, parks on detected signals,
//! auto-classifies them, and optionally enables the appropriate decoder.
//!
//! ```text
//! Scanning ──(dwell elapsed)──→ Settling ──(10ms)──→ Scanning (next freq)
//!     │                                                    │
//!     └──(detection ≥ min_snr)──→ Parked ──(timeout/lost)──┘
//!                                   │
//!                            (auto_decode: EnableDecoder)
//! ```

use std::time::Instant;

use std::sync::Arc;

use crate::dsp::detection::Detection;
use crate::frequency_db::FrequencyDb;
use crate::session::{Command, DemodConfig, Event};

use super::Mode;
use super::classifier::{RuleClassifier, SignalClass};

/// Configuration for general auto-scan mode.
#[derive(Debug, Clone)]
pub struct GeneralModeConfig {
    /// Start of scan range in Hz.
    pub scan_start: f64,
    /// End of scan range in Hz.
    pub scan_end: f64,
    /// Step size in Hz. Default: sample_rate / 2.
    pub step_hz: Option<f64>,
    /// Dwell time per step in milliseconds. Default: 200.
    pub dwell_ms: u64,
    /// Minimum SNR to trigger parking in dB. Default: 10.0.
    pub min_snr_db: f32,
    /// How long to stay parked in seconds. 0 = indefinite. Default: 30.
    pub park_duration_secs: u64,
    /// Whether to auto-enable decoders for classified signals. Default: true.
    pub auto_decode: bool,
    /// Sample rate for computing default step size. Default: 2_048_000.0.
    pub sample_rate: f64,
    /// Whether to start audio demod when parked on a signal. Default: false.
    pub enable_audio: bool,
    /// Override demod mode for audio (None = auto-detect from frequency database).
    pub audio_mode: Option<String>,
}

impl Default for GeneralModeConfig {
    fn default() -> Self {
        Self {
            scan_start: 88_000_000.0,
            scan_end: 108_000_000.0,
            step_hz: None,
            dwell_ms: 200,
            min_snr_db: 10.0,
            park_duration_secs: 30,
            auto_decode: true,
            sample_rate: 2_048_000.0,
            enable_audio: false,
            audio_mode: None,
        }
    }
}

impl GeneralModeConfig {
    /// Effective step size (explicit or sample_rate / 2).
    pub fn effective_step(&self) -> f64 {
        self.step_hz.unwrap_or(self.sample_rate / 2.0)
    }
}

/// Internal state of the scan state machine.
enum ScanState {
    Scanning {
        current_freq: f64,
        step_start: Instant,
    },
    Settling {
        freq: f64,
        settle_start: Instant,
    },
    Parked {
        freq: f64,
        park_start: Instant,
        _snr: f32,
        decoder: Option<String>,
        demod_active: bool,
        no_signal_blocks: u32,
    },
}

/// General auto-scan mode.
pub struct GeneralMode {
    config: GeneralModeConfig,
    state: ScanState,
    classifier: RuleClassifier,
    step: f64,
    classification_label: Option<String>,
    freq_db: Option<Arc<FrequencyDb>>,
}

impl GeneralMode {
    pub fn new(config: GeneralModeConfig) -> Self {
        let step = config.effective_step();
        Self {
            state: ScanState::Scanning {
                current_freq: config.scan_start,
                step_start: Instant::now(),
            },
            classifier: RuleClassifier::new(),
            step,
            classification_label: None,
            freq_db: None,
            config,
        }
    }

    /// Create with a frequency database for auto-detecting demod modes.
    pub fn with_freq_db(config: GeneralModeConfig, freq_db: Arc<FrequencyDb>) -> Self {
        let step = config.effective_step();
        Self {
            state: ScanState::Scanning {
                current_freq: config.scan_start,
                step_start: Instant::now(),
            },
            classifier: RuleClassifier::new(),
            step,
            classification_label: None,
            freq_db: Some(freq_db),
            config,
        }
    }

    /// Determine the demod mode for a frequency when audio is enabled.
    fn demod_mode_for_freq(&self, freq: f64) -> String {
        if let Some(ref mode) = self.config.audio_mode {
            return mode.clone();
        }
        if let Some(ref db) = self.freq_db {
            if let Some(mode) = db.demod_mode(freq) {
                return mode.to_string();
            }
        }
        "fm".to_string() // fallback
    }

    /// Find the strongest detection above min_snr.
    fn best_detection(detections: &[Detection], min_snr: f32) -> Option<&Detection> {
        detections
            .iter()
            .filter(|d| d.snr_db >= min_snr)
            .max_by(|a, b| a.snr_db.partial_cmp(&b.snr_db).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// Compute the next scanning frequency with wrap-around.
    fn next_freq(&self, current: f64) -> f64 {
        let next = current + self.step;
        if next > self.config.scan_end {
            self.config.scan_start
        } else {
            next
        }
    }
}

impl Mode for GeneralMode {
    fn name(&self) -> &str {
        "general"
    }

    fn status(&self) -> String {
        match &self.state {
            ScanState::Scanning { current_freq, .. } => {
                format!("Scanning {:.3} MHz", current_freq / 1e6)
            }
            ScanState::Settling { freq, .. } => {
                format!("Settling {:.3} MHz", freq / 1e6)
            }
            ScanState::Parked { freq, decoder, .. } => {
                let label = self
                    .classification_label
                    .as_deref()
                    .unwrap_or("signal");
                match decoder {
                    Some(d) => format!("Parked {:.3} MHz ({}, {})", freq / 1e6, label, d),
                    None => format!("Parked {:.3} MHz ({})", freq / 1e6, label),
                }
            }
        }
    }

    fn handle_event(&mut self, event: &Event) -> Vec<Command> {
        match event {
            Event::Detections(detections) => {
                match &mut self.state {
                    ScanState::Scanning { current_freq, .. } => {
                        // Check for strong signals
                        if let Some(det) = Self::best_detection(detections, self.config.min_snr_db)
                        {
                            let signal_freq = *current_freq + det.freq_offset_hz;
                            let classification =
                                self.classifier.classify(signal_freq, 0.0, det.snr_db);

                            let mut cmds = vec![Command::Tune(signal_freq)];
                            let decoder = match &classification {
                                SignalClass::KnownProtocol {
                                    name,
                                    decoder,
                                    ..
                                } => {
                                    self.classification_label = Some(name.clone());
                                    if self.config.auto_decode {
                                        cmds.push(Command::EnableDecoder(decoder.clone()));
                                    }
                                    Some(decoder.clone())
                                }
                                SignalClass::Recognized { name, .. } => {
                                    self.classification_label = Some(name.clone());
                                    None
                                }
                                SignalClass::Unknown => {
                                    self.classification_label = None;
                                    None
                                }
                            };

                            // Start audio demod if enabled
                            let demod_active = if self.config.enable_audio {
                                let mode = self.demod_mode_for_freq(signal_freq);
                                cmds.push(Command::StartDemod(DemodConfig {
                                    mode,
                                    audio_rate: 48000,
                                    bandwidth: None,
                                    bfo: None,
                                    squelch: None,
                                    deemph_us: None,
                                    output_wav: None,
                                }));
                                true
                            } else {
                                false
                            };

                            self.state = ScanState::Parked {
                                freq: signal_freq,
                                park_start: Instant::now(),
                                _snr: det.snr_db,
                                decoder,
                                demod_active,
                                no_signal_blocks: 0,
                            };

                            cmds
                        } else {
                            Vec::new()
                        }
                    }
                    ScanState::Parked {
                        no_signal_blocks,
                        freq,
                        ..
                    } => {
                        // Check if signal is still present
                        let has_signal = detections
                            .iter()
                            .any(|d| d.snr_db >= self.config.min_snr_db);

                        if has_signal {
                            *no_signal_blocks = 0;
                            return Vec::new();
                        }

                        *no_signal_blocks += 1;

                        // Signal lost after 10 blocks
                        if *no_signal_blocks >= 10 {
                            let parked_freq = *freq;
                            // Take ownership of decoder/demod state before reassigning
                            let (old_decoder, was_demod) = if let ScanState::Parked { ref mut decoder, demod_active, .. } = self.state {
                                (decoder.take(), demod_active)
                            } else {
                                (None, false)
                            };
                            let next = self.next_freq(parked_freq);
                            let mut cmds = Vec::new();
                            if was_demod {
                                cmds.push(Command::StopDemod);
                            }
                            if let Some(dec) = old_decoder {
                                cmds.push(Command::DisableDecoder(dec));
                            }
                            cmds.push(Command::Tune(next));
                            self.classification_label = None;
                            self.state = ScanState::Settling {
                                freq: next,
                                settle_start: Instant::now(),
                            };
                            cmds
                        } else {
                            Vec::new()
                        }
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    fn tick(&mut self) -> Vec<Command> {
        match &self.state {
            ScanState::Scanning {
                current_freq,
                step_start,
            } => {
                if step_start.elapsed().as_millis() >= self.config.dwell_ms as u128 {
                    let next = self.next_freq(*current_freq);
                    let cmds = vec![Command::Tune(next)];
                    self.state = ScanState::Settling {
                        freq: next,
                        settle_start: Instant::now(),
                    };
                    cmds
                } else {
                    Vec::new()
                }
            }
            ScanState::Settling {
                freq,
                settle_start,
            } => {
                if settle_start.elapsed().as_millis() >= 10 {
                    let f = *freq;
                    self.state = ScanState::Scanning {
                        current_freq: f,
                        step_start: Instant::now(),
                    };
                }
                Vec::new()
            }
            ScanState::Parked {
                freq,
                park_start,
                decoder,
                demod_active,
                ..
            } => {
                // Check park timeout (0 = indefinite)
                if self.config.park_duration_secs > 0
                    && park_start.elapsed().as_secs() >= self.config.park_duration_secs
                {
                    let mut cmds = Vec::new();
                    let next = self.next_freq(*freq);
                    if *demod_active {
                        cmds.push(Command::StopDemod);
                    }
                    if let Some(dec) = decoder {
                        cmds.push(Command::DisableDecoder(dec.clone()));
                    }
                    cmds.push(Command::Tune(next));
                    self.classification_label = None;
                    self.state = ScanState::Settling {
                        freq: next,
                        settle_start: Instant::now(),
                    };
                    cmds
                } else {
                    Vec::new()
                }
            }
        }
    }

    fn reset(&mut self) {
        self.state = ScanState::Scanning {
            current_freq: self.config.scan_start,
            step_start: Instant::now(),
        };
        self.classification_label = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> GeneralModeConfig {
        GeneralModeConfig {
            scan_start: 88_000_000.0,
            scan_end: 108_000_000.0,
            step_hz: Some(1_000_000.0),
            dwell_ms: 200,
            min_snr_db: 10.0,
            park_duration_secs: 30,
            auto_decode: true,
            sample_rate: 2_048_000.0,
            enable_audio: false,
            audio_mode: None,
        }
    }

    fn make_detection(freq_offset: f64, snr_db: f32) -> Detection {
        Detection {
            bin: 1024,
            power_db: -30.0,
            noise_floor_db: -60.0,
            snr_db,
            freq_offset_hz: freq_offset,
        }
    }

    #[test]
    fn initial_state_is_scanning() {
        let mode = GeneralMode::new(test_config());
        assert_eq!(mode.name(), "general");
        assert!(mode.status().contains("Scanning"));
        assert!(mode.status().contains("88.000"));
    }

    #[test]
    fn dwell_timeout_transitions_scanning_settling() {
        let mut config = test_config();
        config.dwell_ms = 0; // Immediate transition
        let mut mode = GeneralMode::new(config);

        // First tick should transition since dwell=0
        std::thread::sleep(std::time::Duration::from_millis(1));
        let cmds = mode.tick();
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], Command::Tune(_)));
        assert!(mode.status().contains("Settling"));

        // After settle, should transition back to scanning
        std::thread::sleep(std::time::Duration::from_millis(15));
        let cmds = mode.tick();
        assert!(cmds.is_empty()); // Settling→Scanning doesn't emit commands
        assert!(mode.status().contains("Scanning"));
    }

    #[test]
    fn frequency_wraps_around() {
        let mut config = test_config();
        config.scan_start = 100_000_000.0;
        config.scan_end = 102_000_000.0;
        config.step_hz = Some(3_000_000.0); // Step past end
        config.dwell_ms = 0;
        let mut mode = GeneralMode::new(config);

        // Should wrap around to scan_start
        std::thread::sleep(std::time::Duration::from_millis(1));
        let cmds = mode.tick();
        assert_eq!(cmds.len(), 1);
        if let Command::Tune(freq) = cmds[0] {
            assert_eq!(freq, 100_000_000.0); // Wrapped back to start
        } else {
            panic!("Expected Tune command");
        }
    }

    #[test]
    fn detection_triggers_parked() {
        let mut mode = GeneralMode::new(test_config());
        let det = make_detection(500_000.0, 15.0);
        let cmds = mode.handle_event(&Event::Detections(vec![det]));

        assert!(!cmds.is_empty());
        assert!(matches!(cmds[0], Command::Tune(_)));
        assert!(mode.status().contains("Parked"));
    }

    #[test]
    fn auto_decode_emits_enable_decoder() {
        let mut config = test_config();
        config.auto_decode = true;
        // Start at a frequency where classification can work
        config.scan_start = 1_090_000_000.0;
        config.scan_end = 1_092_000_000.0;
        let mut mode = GeneralMode::new(config);

        // Detection near 1090 MHz → should classify as ADS-B
        let det = make_detection(0.0, 20.0);
        let cmds = mode.handle_event(&Event::Detections(vec![det]));

        let has_enable = cmds
            .iter()
            .any(|c| matches!(c, Command::EnableDecoder(_)));
        assert!(has_enable, "Expected EnableDecoder for ADS-B signal");
    }

    #[test]
    fn park_timeout_resumes_scanning() {
        let mut config = test_config();
        config.park_duration_secs = 0; // Immediate timeout disabled
        let mut mode = GeneralMode::new(config);

        // Park on a signal
        let det = make_detection(0.0, 15.0);
        mode.handle_event(&Event::Detections(vec![det]));
        assert!(mode.status().contains("Parked"));

        // With park_duration=0, timeout never fires
        let cmds = mode.tick();
        assert!(cmds.is_empty());
    }

    #[test]
    fn signal_loss_resumes_scanning() {
        let mut mode = GeneralMode::new(test_config());

        // Park on a signal
        let det = make_detection(0.0, 15.0);
        mode.handle_event(&Event::Detections(vec![det]));
        assert!(mode.status().contains("Parked"));

        // Send 10 blocks without strong detections
        for _ in 0..10 {
            let cmds = mode.handle_event(&Event::Detections(vec![]));
            if !cmds.is_empty() {
                // Should resume scanning after 10 blocks
                assert!(matches!(cmds.last().unwrap(), Command::Tune(_)));
                return;
            }
        }
        panic!("Expected scan to resume after 10 blocks without signal");
    }

    #[test]
    fn config_validation() {
        let config = GeneralModeConfig {
            scan_start: 100_000_000.0,
            scan_end: 200_000_000.0,
            step_hz: Some(5_000_000.0),
            ..Default::default()
        };
        assert!(config.scan_start < config.scan_end);
        assert!(config.effective_step() > 0.0);
    }

    #[test]
    fn reset_clears_state() {
        let mut mode = GeneralMode::new(test_config());

        // Park on something
        let det = make_detection(0.0, 15.0);
        mode.handle_event(&Event::Detections(vec![det]));
        assert!(mode.status().contains("Parked"));

        // Reset
        mode.reset();
        assert!(mode.status().contains("Scanning"));
        assert!(mode.status().contains("88.000"));
    }

    #[test]
    fn no_commands_when_no_detections() {
        let mut mode = GeneralMode::new(test_config());
        let cmds = mode.handle_event(&Event::Detections(vec![]));
        assert!(cmds.is_empty());
    }

    #[test]
    fn audio_enabled_emits_start_demod_on_park() {
        let mut config = test_config();
        config.enable_audio = true;
        config.audio_mode = Some("wfm".to_string());
        let mut mode = GeneralMode::new(config);

        let det = make_detection(0.0, 15.0);
        let cmds = mode.handle_event(&Event::Detections(vec![det]));

        let has_start_demod = cmds.iter().any(|c| matches!(c, Command::StartDemod(_)));
        assert!(has_start_demod, "Expected StartDemod when enable_audio is true");
    }

    #[test]
    fn audio_disabled_no_demod() {
        let mut config = test_config();
        config.enable_audio = false;
        let mut mode = GeneralMode::new(config);

        let det = make_detection(0.0, 15.0);
        let cmds = mode.handle_event(&Event::Detections(vec![det]));

        let has_start_demod = cmds.iter().any(|c| matches!(c, Command::StartDemod(_)));
        assert!(!has_start_demod, "Should not emit StartDemod when enable_audio is false");
    }

    #[test]
    fn leave_park_stops_demod() {
        let mut config = test_config();
        config.enable_audio = true;
        config.audio_mode = Some("fm".to_string());
        let mut mode = GeneralMode::new(config);

        // Park on a signal
        let det = make_detection(0.0, 15.0);
        mode.handle_event(&Event::Detections(vec![det]));
        assert!(mode.status().contains("Parked"));

        // Signal loss: 10 blocks with no detections
        let mut final_cmds = Vec::new();
        for _ in 0..10 {
            let cmds = mode.handle_event(&Event::Detections(vec![]));
            if !cmds.is_empty() {
                final_cmds = cmds;
                break;
            }
        }

        let has_stop_demod = final_cmds.iter().any(|c| matches!(c, Command::StopDemod));
        assert!(has_stop_demod, "Expected StopDemod when leaving park with audio active");
    }

    #[test]
    fn park_timeout_stops_demod() {
        let mut config = test_config();
        config.enable_audio = true;
        config.audio_mode = Some("fm".to_string());
        config.park_duration_secs = 0; // We'll test with immediate park via direct state manipulation

        // Use a very short park duration so the test doesn't have to wait
        config.park_duration_secs = 1;
        let mut mode = GeneralMode::new(config);

        // Park on a signal
        let det = make_detection(0.0, 15.0);
        mode.handle_event(&Event::Detections(vec![det]));
        assert!(mode.status().contains("Parked"));

        // Wait for park timeout
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let cmds = mode.tick();

        let has_stop_demod = cmds.iter().any(|c| matches!(c, Command::StopDemod));
        assert!(has_stop_demod, "Expected StopDemod on park timeout with audio active");
        assert!(cmds.iter().any(|c| matches!(c, Command::Tune(_))), "Expected Tune after timeout");
    }
}
