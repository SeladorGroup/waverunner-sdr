//! Keyboard input handling.
//!
//! Key bindings follow a hybrid Vim/radio convention:
//!
//! | Key          | Action                          |
//! |--------------|---------------------------------|
//! | q, Esc       | Quit                            |
//! | j, ↑         | Tune up by step                 |
//! | k, ↓         | Tune down by step               |
//! | h, ←         | Decrease step size              |
//! | l, →         | Increase step size              |
//! | m            | Cycle demod mode forward         |
//! | M            | Cycle demod mode backward        |
//! | f            | Enter frequency input mode       |
//! | s            | Toggle squelch                   |
//! | S (+shift)   | Squelch up (less sensitive)      |
//! | +            | Squelch up                       |
//! | -            | Squelch down (more sensitive)    |
//! | Enter        | Confirm frequency entry          |
//! | Backspace    | Delete char in frequency entry   |

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, InputMode};

/// Action produced by keyboard input.
pub enum Action {
    None,
    Quit,
    TuneUp,
    TuneDown,
    StepIncrease,
    StepDecrease,
    CycleDemod,
    CycleDemodBack,
    ToggleSquelch,
    SquelchUp,
    SquelchDown,
    FrequencyEntry,
    FrequencyConfirm(f64),
    FrequencyCancel,
    CycleDecoder,
    CycleDecoderBack,
    CycleViewTab,
    CycleViewTabBack,
    CycleMode,
    CycleModeBack,
    ToggleGeneralScan,
    RunMeasurement,
    ToggleTracking,
    CaptureReference,
    CompareReference,
    ExportCsv,
    AddBookmark,
    ExportReport,
    VolumeUp,
    VolumeDown,
    VolumeMute,
    SaveBookmark,
    IdentifySignal,
    ExportPng,
}

/// Parse a frequency string entered by the user.
/// Delegates to wavecore::util::parse_frequency, returning Option for UI use.
fn parse_freq_input(s: &str) -> Option<f64> {
    wavecore::util::parse_frequency(s).ok()
}

/// Process a key event and return the resulting action.
pub fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    match &app.input_mode {
        InputMode::FrequencyEntry(current) => {
            match key.code {
                KeyCode::Enter => {
                    let text = current.clone();
                    app.input_mode = InputMode::Normal;
                    if let Some(freq) = parse_freq_input(&text) {
                        Action::FrequencyConfirm(freq)
                    } else {
                        Action::FrequencyCancel
                    }
                }
                KeyCode::Esc => {
                    app.input_mode = InputMode::Normal;
                    Action::FrequencyCancel
                }
                KeyCode::Backspace => {
                    let mut text = current.clone();
                    text.pop();
                    app.input_mode = InputMode::FrequencyEntry(text);
                    Action::None
                }
                KeyCode::Char(c) => {
                    // Only allow digits, dots, and suffix letters
                    if c.is_ascii_digit() || c == '.' || "gGmMkK".contains(c) {
                        let mut text = current.clone();
                        text.push(c);
                        app.input_mode = InputMode::FrequencyEntry(text);
                    }
                    Action::None
                }
                _ => Action::None,
            }
        }
        InputMode::Normal => {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
                KeyCode::Char('j') | KeyCode::Up => Action::TuneUp,
                KeyCode::Char('k') | KeyCode::Down => Action::TuneDown,
                KeyCode::Char('h') | KeyCode::Left => Action::StepDecrease,
                KeyCode::Char('l') | KeyCode::Right => Action::StepIncrease,
                KeyCode::Char('m') => Action::CycleDemod,
                KeyCode::Char('M') => Action::CycleDemodBack,
                KeyCode::Char('d') => Action::CycleDecoder,
                KeyCode::Char('D') => Action::CycleDecoderBack,
                KeyCode::Char('p') => Action::CycleMode,
                KeyCode::Char('P') => Action::CycleModeBack,
                KeyCode::Char('g') => Action::ToggleGeneralScan,
                KeyCode::Char('f') => {
                    app.input_mode = InputMode::FrequencyEntry(String::new());
                    Action::FrequencyEntry
                }
                KeyCode::Char('s') => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        Action::SquelchUp
                    } else {
                        Action::ToggleSquelch
                    }
                }
                KeyCode::Char('S') => Action::SquelchUp,
                KeyCode::Char('+') | KeyCode::Char('=') => Action::SquelchUp,
                KeyCode::Char('-') => Action::SquelchDown,
                KeyCode::Tab => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        Action::CycleViewTabBack
                    } else {
                        Action::CycleViewTab
                    }
                }
                KeyCode::BackTab => Action::CycleViewTabBack,
                KeyCode::Char('`') => Action::CycleViewTabBack,
                KeyCode::Char('a') => Action::RunMeasurement,
                KeyCode::Char('A') => Action::ToggleTracking,
                KeyCode::Char('r') => Action::CaptureReference,
                KeyCode::Char('c') => Action::CompareReference,
                KeyCode::Char('x') => Action::ExportCsv,
                KeyCode::Char('b') => Action::AddBookmark,
                KeyCode::Char('R') => Action::ExportReport,
                KeyCode::Char(']') => Action::VolumeUp,
                KeyCode::Char('[') => Action::VolumeDown,
                KeyCode::Char('\\') => Action::VolumeMute,
                KeyCode::Char('B') => Action::SaveBookmark,
                KeyCode::Char('i') => Action::IdentifySignal,
                KeyCode::Char('X') => Action::ExportPng,
                _ => Action::None,
            }
        }
    }
}

/// Poll for keyboard events with a timeout.
/// Returns None if no event within the timeout.
pub fn poll_event(timeout: std::time::Duration) -> Option<KeyEvent> {
    if event::poll(timeout).unwrap_or(false) {
        if let Ok(Event::Key(key)) = event::read() {
            return Some(key);
        }
    }
    None
}
