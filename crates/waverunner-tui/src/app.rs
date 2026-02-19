//! Application state for WaveRunner TUI.
//!
//! State is updated from SessionManager events on each frame tick.
//! The UI thread owns all state directly — no shared mutexes needed.
//!
//! ```text
//! SessionManager ──Event──→ App (owned state) ──→ UI rendering
//! ```

use std::collections::VecDeque;

use wavecore::dsp::detection::Detection;
use wavecore::dsp::statistics::SignalStats;
use wavecore::frequency_db::FrequencyDb;
use wavecore::mode::ModeController;
use wavecore::session::{DecodedMessage, DemodVisData, SessionStats, SpectrumFrame};
use wavecore::types::Frequency;

/// Tab views for the TUI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewTab {
    /// Spectrum + waterfall + signal stats (default)
    Standard,
    /// Spectrum + constellation + PLL state
    Constellation,
    /// Spectrum + detailed stats table
    Statistics,
    /// Spectrum + tracking sparkline + measurement readout
    Analysis,
}

impl ViewTab {
    pub fn next(&self) -> ViewTab {
        match self {
            ViewTab::Standard => ViewTab::Constellation,
            ViewTab::Constellation => ViewTab::Statistics,
            ViewTab::Statistics => ViewTab::Analysis,
            ViewTab::Analysis => ViewTab::Standard,
        }
    }

    pub fn prev(&self) -> ViewTab {
        match self {
            ViewTab::Standard => ViewTab::Analysis,
            ViewTab::Constellation => ViewTab::Standard,
            ViewTab::Statistics => ViewTab::Constellation,
            ViewTab::Analysis => ViewTab::Statistics,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            ViewTab::Standard => "Standard",
            ViewTab::Constellation => "Constellation",
            ViewTab::Statistics => "Statistics",
            ViewTab::Analysis => "Analysis",
        }
    }
}

/// Available decoder names for the [d]ecoder key.
///
/// Sourced from the canonical registry list so names can never drift.
pub const AVAILABLE_DECODERS: &[&str] = wavecore::dsp::decoders::DECODER_NAMES;

/// Maximum decoded messages kept in the ring buffer.
const MAX_DECODED_MESSAGES: usize = 500;

/// Tuning step sizes available for keyboard navigation.
/// Organized by SI prefix for intuitive stepping.
pub const STEP_SIZES: &[f64] = &[
    1.0,           // 1 Hz
    10.0,          // 10 Hz
    100.0,         // 100 Hz
    1_000.0,       // 1 kHz
    5_000.0,       // 5 kHz
    10_000.0,      // 10 kHz
    25_000.0,      // 25 kHz (VHF channel spacing)
    100_000.0,     // 100 kHz
    1_000_000.0,   // 1 MHz
    10_000_000.0,  // 10 MHz
];

/// Demodulation mode selection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DemodMode {
    Off,
    Am,
    AmSync,
    Fm,
    Wfm,
    WfmStereo,
    Usb,
    Lsb,
    Cw,
}

impl DemodMode {
    pub const ALL: &[DemodMode] = &[
        DemodMode::Off,
        DemodMode::Am,
        DemodMode::AmSync,
        DemodMode::Fm,
        DemodMode::Wfm,
        DemodMode::WfmStereo,
        DemodMode::Usb,
        DemodMode::Lsb,
        DemodMode::Cw,
    ];

    pub fn label(&self) -> &str {
        match self {
            DemodMode::Off => "OFF",
            DemodMode::Am => "AM",
            DemodMode::AmSync => "AM-S",
            DemodMode::Fm => "NFM",
            DemodMode::Wfm => "WFM",
            DemodMode::WfmStereo => "WFM-ST",
            DemodMode::Usb => "USB",
            DemodMode::Lsb => "LSB",
            DemodMode::Cw => "CW",
        }
    }

    /// Convert to SessionManager mode string. Returns None for Off.
    pub fn session_mode(&self) -> Option<&'static str> {
        match self {
            DemodMode::Off => None,
            DemodMode::Am => Some("am"),
            DemodMode::AmSync => Some("am-sync"),
            DemodMode::Fm => Some("fm"),
            DemodMode::Wfm => Some("wfm"),
            DemodMode::WfmStereo => Some("wfm-stereo"),
            DemodMode::Usb => Some("usb"),
            DemodMode::Lsb => Some("lsb"),
            DemodMode::Cw => Some("cw"),
        }
    }

    pub fn next(&self) -> DemodMode {
        let all = Self::ALL;
        let idx = all.iter().position(|m| m == self).unwrap_or(0);
        all[(idx + 1) % all.len()]
    }

    pub fn prev(&self) -> DemodMode {
        let all = Self::ALL;
        let idx = all.iter().position(|m| m == self).unwrap_or(0);
        all[(idx + all.len() - 1) % all.len()]
    }
}

/// DSP-computed state, updated from SessionManager events.
#[derive(Clone)]
pub struct DspState {
    /// Power spectrum in dBFS, FFT-shifted (DC centered).
    pub spectrum_db: Vec<f32>,
    /// Peak hold envelope over spectrum (decays slowly).
    pub peak_hold_db: Vec<f32>,
    /// CFAR detection results.
    pub detections: Vec<Detection>,
    /// Signal statistics.
    pub stats: SignalStats,
    /// RMS power in dBFS.
    pub rms_dbfs: f32,
    /// Estimated noise floor in dB.
    pub noise_floor_db: f32,
    /// M2M4 SNR estimate in dB.
    pub snr_db: f32,
    /// Spectral flatness (Wiener entropy).
    pub spectral_flatness: f32,
    /// AGC gain in dB (when demod active).
    pub agc_gain_db: f32,
    /// Cumulative block count.
    pub block_count: u64,
    /// IQ constellation points from demod chain.
    pub constellation_points: Vec<(f32, f32)>,
    /// PLL lock indicator.
    pub pll_locked: bool,
    /// PLL frequency estimate in Hz.
    pub pll_frequency_hz: f64,
    /// PLL phase error in radians.
    pub pll_phase_error: f32,
}

impl Default for DspState {
    fn default() -> Self {
        Self {
            spectrum_db: Vec::new(),
            peak_hold_db: Vec::new(),
            detections: Vec::new(),
            stats: SignalStats {
                mean: wavecore::types::Sample::new(0.0, 0.0),
                variance: 0.0,
                rms: 0.0,
                peak: 0.0,
                crest_factor_db: 0.0,
                skewness: 0.0,
                kurtosis: 0.0,
                excess_kurtosis: 0.0,
            },
            rms_dbfs: -100.0,
            noise_floor_db: -100.0,
            snr_db: 0.0,
            spectral_flatness: 0.0,
            agc_gain_db: 0.0,
            block_count: 0,
            constellation_points: Vec::new(),
            pll_locked: false,
            pll_frequency_hz: 0.0,
            pll_phase_error: 0.0,
        }
    }
}

/// UI input mode — normal or frequency entry.
#[derive(Clone, Debug, PartialEq)]
pub enum InputMode {
    Normal,
    FrequencyEntry(String),
}

/// Main application state.
pub struct App {
    /// Center frequency in Hz.
    pub frequency: Frequency,
    /// Sample rate in S/s.
    pub sample_rate: f64,
    /// Current tuning step index into STEP_SIZES.
    pub step_index: usize,
    /// Gain mode string.
    pub gain: String,
    /// Current demod mode.
    pub demod_mode: DemodMode,
    /// Squelch threshold in dBFS (None = disabled).
    pub squelch: Option<f64>,

    /// DSP state from SessionManager events.
    pub dsp: DspState,

    /// Waterfall history — each row is a spectrum snapshot.
    /// Stored as a circular buffer with `waterfall_write` pointing to next row.
    pub waterfall: Vec<Vec<f32>>,
    pub waterfall_write: usize,
    pub waterfall_rows: usize,

    /// UI input mode.
    pub input_mode: InputMode,

    /// Whether the app is running.
    running: bool,

    /// Frame counter for UI updates.
    pub frame_count: u64,

    /// Session performance stats (from SessionManager Stats events).
    pub blocks_processed: u64,
    pub blocks_dropped: u64,
    pub cpu_load_percent: f32,
    pub throughput_msps: f64,

    /// Active decoder name (None = no decoder running).
    pub active_decoder: Option<String>,

    /// Ring buffer of decoded messages (newest at back).
    pub decoded_messages: VecDeque<DecodedMessage>,

    /// Current view tab.
    pub view_tab: ViewTab,

    /// Mode controller for orchestrating scan modes and profiles.
    pub mode_controller: ModeController,

    /// Current mode status string for header display.
    pub mode_status: Option<String>,

    /// Latest analysis result (from RunAnalysis command).
    pub analysis_result: Option<wavecore::analysis::AnalysisResult>,

    /// Latest tracking data snapshot (~1 Hz updates).
    pub tracking_data: Option<wavecore::analysis::tracking::TrackingSnapshot>,

    /// Whether time-series tracking is active.
    pub tracking_active: bool,

    /// Whether a reference spectrum has been captured.
    pub reference_captured: bool,

    /// Pipeline health status.
    pub health: wavecore::session::HealthStatus,

    /// Per-stage latency breakdown.
    pub latency: wavecore::session::LatencyBreakdown,

    /// Number of annotations in the session timeline.
    pub annotation_count: u64,

    /// Buffer occupancy (for display).
    pub buffer_occupancy: u16,

    /// Events dropped counter.
    pub events_dropped: u64,

    /// Audio volume 0-100%.
    pub volume: u8,

    /// Regional frequency database for band identification.
    pub frequency_db: FrequencyDb,

    /// Latest signal identification result (from [i] key).
    pub identify_result: Option<wavecore::signal_identify::IdentifyResult>,
}

impl App {
    pub fn new(
        frequency: f64,
        sample_rate: f64,
        gain: String,
        fft_size: usize,
    ) -> Self {
        let waterfall_rows = 100;
        Self {
            frequency,
            sample_rate,
            gain,
            demod_mode: DemodMode::Off,
            squelch: None,
            step_index: 6, // 25 kHz default
            dsp: DspState::default(),
            waterfall: vec![vec![-100.0; fft_size]; waterfall_rows],
            waterfall_write: 0,
            waterfall_rows,
            input_mode: InputMode::Normal,
            running: true,
            frame_count: 0,
            blocks_processed: 0,
            blocks_dropped: 0,
            cpu_load_percent: 0.0,
            throughput_msps: 0.0,
            active_decoder: None,
            decoded_messages: VecDeque::with_capacity(MAX_DECODED_MESSAGES),
            view_tab: ViewTab::Standard,
            mode_controller: ModeController::new(
                AVAILABLE_DECODERS.iter().map(|s| s.to_string()).collect(),
            ),
            mode_status: None,
            analysis_result: None,
            tracking_data: None,
            tracking_active: false,
            reference_captured: false,
            health: wavecore::session::HealthStatus::Normal,
            latency: wavecore::session::LatencyBreakdown::default(),
            annotation_count: 0,
            buffer_occupancy: 0,
            events_dropped: 0,
            volume: 80,
            frequency_db: FrequencyDb::auto_detect(),
            identify_result: None,
        }
    }

    /// Update DSP state from a SpectrumReady event.
    pub fn update_from_spectrum(&mut self, frame: SpectrumFrame) {
        // Peak hold with decay
        if self.dsp.peak_hold_db.len() != frame.spectrum_db.len() {
            self.dsp.peak_hold_db = frame.spectrum_db.clone();
        } else {
            for (peak, &current) in self.dsp.peak_hold_db.iter_mut().zip(&frame.spectrum_db) {
                if current > *peak {
                    *peak = current;
                } else {
                    *peak = (*peak - 0.5).max(current); // 0.5 dB/frame decay
                }
            }
        }

        self.dsp.spectrum_db = frame.spectrum_db;
        self.dsp.noise_floor_db = frame.noise_floor_db;
        self.dsp.rms_dbfs = frame.rms_dbfs;
        self.dsp.snr_db = frame.snr_db;
        self.dsp.spectral_flatness = frame.spectral_flatness;
        self.dsp.stats = frame.signal_stats;
        self.dsp.agc_gain_db = frame.agc_gain_db;
        self.dsp.block_count = frame.block_count;
    }

    /// Update demod visualization state.
    pub fn update_from_demod_vis(&mut self, vis: DemodVisData) {
        self.dsp.constellation_points = vis.constellation;
        self.dsp.pll_locked = vis.pll_locked;
        self.dsp.pll_frequency_hz = vis.pll_frequency_hz;
        self.dsp.pll_phase_error = vis.pll_phase_error;
    }

    /// Update performance stats from a Stats event.
    pub fn update_from_stats(&mut self, stats: SessionStats) {
        self.blocks_processed = stats.blocks_processed;
        self.blocks_dropped = stats.blocks_dropped;
        self.cpu_load_percent = stats.cpu_load_percent;
        self.throughput_msps = stats.throughput_msps;
        self.health = stats.health;
        self.latency = stats.latency;
        self.buffer_occupancy = stats.buffer_occupancy;
        self.events_dropped = stats.events_dropped;
    }

    /// Push a new spectrum line into the waterfall circular buffer.
    pub fn push_waterfall(&mut self, spectrum: &[f32]) {
        let row = &mut self.waterfall[self.waterfall_write];
        row.clear();
        row.extend_from_slice(spectrum);
        self.waterfall_write = (self.waterfall_write + 1) % self.waterfall_rows;
    }

    /// Get waterfall rows in chronological order (oldest first).
    pub fn waterfall_ordered(&self) -> Vec<&[f32]> {
        let mut rows = Vec::with_capacity(self.waterfall_rows);
        for i in 0..self.waterfall_rows {
            let idx = (self.waterfall_write + i) % self.waterfall_rows;
            rows.push(self.waterfall[idx].as_slice());
        }
        rows
    }

    /// Current step size in Hz.
    pub fn step_hz(&self) -> f64 {
        STEP_SIZES[self.step_index]
    }

    /// Tune up by one step.
    pub fn tune_up(&mut self) {
        self.frequency += self.step_hz();
    }

    /// Tune down by one step.
    pub fn tune_down(&mut self) {
        self.frequency = (self.frequency - self.step_hz()).max(0.0);
    }

    /// Increase step size.
    pub fn step_increase(&mut self) {
        if self.step_index < STEP_SIZES.len() - 1 {
            self.step_index += 1;
        }
    }

    /// Decrease step size.
    pub fn step_decrease(&mut self) {
        if self.step_index > 0 {
            self.step_index -= 1;
        }
    }

    /// Cycle demod mode forward.
    pub fn cycle_demod(&mut self) {
        self.demod_mode = self.demod_mode.next();
    }

    /// Cycle demod mode backward.
    pub fn cycle_demod_back(&mut self) {
        self.demod_mode = self.demod_mode.prev();
    }

    /// Cycle view tab forward.
    pub fn cycle_view_tab(&mut self) {
        self.view_tab = self.view_tab.next();
    }

    /// Cycle view tab backward.
    pub fn cycle_view_tab_back(&mut self) {
        self.view_tab = self.view_tab.prev();
    }

    /// Toggle squelch or adjust by 3 dB.
    pub fn toggle_squelch(&mut self) {
        self.squelch = match self.squelch {
            None => Some(-30.0),
            Some(_) => None,
        };
    }

    pub fn squelch_up(&mut self) {
        if let Some(ref mut sq) = self.squelch {
            *sq = (*sq + 3.0).min(0.0);
        }
    }

    pub fn squelch_down(&mut self) {
        if let Some(ref mut sq) = self.squelch {
            *sq -= 3.0;
        }
    }

    /// Push a decoded message into the ring buffer.
    pub fn push_decoded_message(&mut self, msg: DecodedMessage) {
        if self.decoded_messages.len() >= MAX_DECODED_MESSAGES {
            self.decoded_messages.pop_front();
        }
        self.decoded_messages.push_back(msg);
    }

    /// Cycle active decoder forward: None → pocsag → adsb → rds → None.
    pub fn cycle_decoder(&mut self) {
        let current_idx = self
            .active_decoder
            .as_ref()
            .and_then(|d| AVAILABLE_DECODERS.iter().position(|&name| name == d));
        self.active_decoder = match current_idx {
            None => Some(AVAILABLE_DECODERS[0].to_string()),
            Some(idx) if idx + 1 < AVAILABLE_DECODERS.len() => {
                Some(AVAILABLE_DECODERS[idx + 1].to_string())
            }
            _ => None,
        };
    }

    /// Cycle active decoder backward: None ← pocsag ← adsb ← rds ← None.
    pub fn cycle_decoder_back(&mut self) {
        let current_idx = self
            .active_decoder
            .as_ref()
            .and_then(|d| AVAILABLE_DECODERS.iter().position(|&name| name == d));
        self.active_decoder = match current_idx {
            None => Some(AVAILABLE_DECODERS[AVAILABLE_DECODERS.len() - 1].to_string()),
            Some(0) => None,
            Some(idx) => Some(AVAILABLE_DECODERS[idx - 1].to_string()),
        };
    }

    /// Cycle mode forward: None → aviation → pager → fm-broadcast → general → None.
    /// Returns commands from activating/deactivating the mode.
    pub fn cycle_mode_forward(&mut self) -> Vec<wavecore::session::Command> {
        let profiles = self.mode_controller.list_profiles().iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let current = self.mode_controller.active_mode().map(|s| s.to_string());

        let mut cmds = self.mode_controller.deactivate();

        let next = match current.as_deref() {
            None if !profiles.is_empty() => Some(profiles[0].clone()),
            Some(name) => {
                if name == "general" {
                    None
                } else if let Some(idx) = profiles.iter().position(|p| p == name) {
                    if idx + 1 < profiles.len() {
                        Some(profiles[idx + 1].clone())
                    } else {
                        Some("general".to_string())
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(ref name) = next {
            if name == "general" {
                let config = wavecore::mode::general::GeneralModeConfig {
                    scan_start: self.frequency - self.sample_rate / 2.0,
                    scan_end: self.frequency + self.sample_rate / 2.0,
                    ..Default::default()
                };
                let mode = wavecore::mode::general::GeneralMode::new(config);
                cmds.extend(self.mode_controller.activate(Box::new(mode)));
            } else if let Some(mode) = self.mode_controller.create_profile_mode(name) {
                cmds.extend(self.mode_controller.activate(mode));
            }
        }

        self.mode_status = self.mode_controller.mode_status();
        cmds
    }

    /// Cycle mode backward.
    pub fn cycle_mode_back(&mut self) -> Vec<wavecore::session::Command> {
        let profiles = self.mode_controller.list_profiles().iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let current = self.mode_controller.active_mode().map(|s| s.to_string());

        let mut cmds = self.mode_controller.deactivate();

        let next = match current.as_deref() {
            None => Some("general".to_string()),
            Some("general") if !profiles.is_empty() => Some(profiles[profiles.len() - 1].clone()),
            Some("general") => None,
            Some(name) => {
                if let Some(idx) = profiles.iter().position(|p| p == name) {
                    if idx > 0 {
                        Some(profiles[idx - 1].clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        };

        if let Some(ref name) = next {
            if name == "general" {
                let config = wavecore::mode::general::GeneralModeConfig {
                    scan_start: self.frequency - self.sample_rate / 2.0,
                    scan_end: self.frequency + self.sample_rate / 2.0,
                    ..Default::default()
                };
                let mode = wavecore::mode::general::GeneralMode::new(config);
                cmds.extend(self.mode_controller.activate(Box::new(mode)));
            } else if let Some(mode) = self.mode_controller.create_profile_mode(name) {
                cmds.extend(self.mode_controller.activate(mode));
            }
        }

        self.mode_status = self.mode_controller.mode_status();
        cmds
    }

    /// Toggle general scan mode on/off using the visible spectrum range.
    /// If a demod mode is active, scanning will play audio on parked signals.
    pub fn toggle_general_scan(&mut self) -> Vec<wavecore::session::Command> {
        if self.mode_controller.active_mode() == Some("general") {
            let cmds = self.mode_controller.deactivate();
            self.mode_status = None;
            cmds
        } else {
            let mut cmds = self.mode_controller.deactivate();
            let has_demod = self.demod_mode != DemodMode::Off;
            let config = wavecore::mode::general::GeneralModeConfig {
                scan_start: self.frequency - self.sample_rate / 2.0,
                scan_end: self.frequency + self.sample_rate / 2.0,
                enable_audio: has_demod,
                audio_mode: self.demod_mode.session_mode().map(|s| s.to_string()),
                ..Default::default()
            };
            let mode = if has_demod {
                wavecore::mode::general::GeneralMode::with_freq_db(
                    config,
                    std::sync::Arc::new(self.frequency_db.clone()),
                )
            } else {
                wavecore::mode::general::GeneralMode::new(config)
            };
            cmds.extend(self.mode_controller.activate(Box::new(mode)));
            self.mode_status = self.mode_controller.mode_status();
            cmds
        }
    }

    /// Increase volume by 5%.
    pub fn volume_up(&mut self) {
        self.volume = self.volume.saturating_add(5).min(100);
    }

    /// Decrease volume by 5%.
    pub fn volume_down(&mut self) {
        self.volume = self.volume.saturating_sub(5);
    }

    /// Toggle mute (0) / unmute (80).
    pub fn volume_toggle_mute(&mut self) {
        if self.volume > 0 {
            self.volume = 0;
        } else {
            self.volume = 80;
        }
    }

    /// Volume as f32 0.0..1.0 for the audio sink.
    pub fn volume_f32(&self) -> f32 {
        self.volume as f32 / 100.0
    }

    /// Check if app is running.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Signal app to quit.
    pub fn quit(&mut self) {
        self.running = false;
    }
}
