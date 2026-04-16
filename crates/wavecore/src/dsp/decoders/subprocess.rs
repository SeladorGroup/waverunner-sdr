//! Generic subprocess bridge for external decoder tools.
//!
//! Provides a reusable framework for piping IQ/audio data to external tools
//! (rtl_433, redsea, multimon-ng, dump1090) and parsing their output into
//! [`DecodedMessage`]s.
//!
//! ## Signal Flow
//!
//! ```text
//! cf32 IQ samples ─→ InputFormat conversion ─→ tool stdin
//!                                                  │
//!                      DecodedMessage ← OutputParser ← tool stdout
//! ```

use std::collections::VecDeque;
use std::io::Write;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};

use crossbeam_channel::{Receiver, Sender};

use crate::session::DecodedMessage;
use crate::types::Sample;

/// How to convert cf32 IQ samples for the external tool's stdin.
pub enum InputFormat {
    /// Convert cf32 → interleaved cu8 (for rtl_433, dump1090).
    /// Each complex sample becomes 2 bytes: I then Q, range [0, 255].
    Cu8Iq,
    /// Already-demodulated f32 audio → S16LE mono (for redsea, multimon-ng).
    /// Caller must FM-demodulate first, then pass audio floats.
    S16LeAudio,
}

/// Parses lines from an external tool's stdout into decoded messages.
pub trait OutputParser: Send {
    /// Try to parse one stdout line into a decoded message.
    /// Return None to skip non-message lines (headers, empty lines, etc).
    fn parse_line(&mut self, line: &str) -> Option<DecodedMessage>;
}

/// Configuration for spawning an external tool subprocess.
pub struct SubprocessConfig {
    /// Executable name (must be on $PATH).
    pub command: String,
    /// Command-line arguments.
    pub args: Vec<String>,
    /// How to convert input samples for stdin.
    pub input_format: InputFormat,
    /// Parser for stdout lines.
    pub output_parser: Box<dyn OutputParser>,
    /// Thread name for the stdout reader.
    pub thread_name: String,
}

/// Reusable subprocess bridge for external decoder tools.
///
/// Handles lazy spawning, stdin piping, stdout reader thread,
/// message channel, and cleanup on death/reset/drop.
pub struct SubprocessBridge {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    message_rx: Option<Receiver<DecodedMessage>>,
    reader_handle: Option<std::thread::JoinHandle<()>>,
    stderr_rx: Option<Receiver<String>>,
    stderr_handle: Option<std::thread::JoinHandle<()>>,
    recent_stderr: VecDeque<String>,
    /// Whether initialization has been attempted (prevents retries).
    pub init_attempted: bool,
    init_error: Option<String>,
}

impl Default for SubprocessBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl SubprocessBridge {
    pub fn new() -> Self {
        Self {
            child: None,
            stdin: None,
            message_rx: None,
            reader_handle: None,
            stderr_rx: None,
            stderr_handle: None,
            recent_stderr: VecDeque::new(),
            init_attempted: false,
            init_error: None,
        }
    }

    /// Check if the subprocess is running.
    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    /// Start the subprocess if not already running. Returns Ok(true) if ready,
    /// Ok(false) if already attempted and failed, Err with the init error message.
    pub fn ensure_started(&mut self, config: &mut SubprocessConfig) -> Result<bool, String> {
        if self.child.is_some() {
            return Ok(true);
        }
        if self.init_attempted {
            return Ok(false);
        }
        self.init_attempted = true;

        let result = Command::new(&config.command)
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        match result {
            Ok(mut child) => {
                let Some(stdin) = child.stdin.take() else {
                    let err = format!("{}: stdin not available", config.command);
                    child.kill().ok();
                    self.init_error = Some(err.clone());
                    return Err(err);
                };
                let Some(stdout) = child.stdout.take() else {
                    let err = format!("{}: stdout not available", config.command);
                    child.kill().ok();
                    self.init_error = Some(err.clone());
                    return Err(err);
                };
                let Some(stderr) = child.stderr.take() else {
                    let err = format!("{}: stderr not available", config.command);
                    child.kill().ok();
                    self.init_error = Some(err.clone());
                    return Err(err);
                };

                let (tx, rx) = crossbeam_channel::unbounded();
                let thread_name = config.thread_name.clone();
                let stderr_thread_name = format!("{thread_name}-stderr");
                let (stderr_tx, stderr_rx) = crossbeam_channel::unbounded();

                // Take the parser out of the config — it moves into the reader thread.
                // This is safe because ensure_started is only called once.
                let parser = std::mem::replace(&mut config.output_parser, Box::new(NullParser));

                let handle =
                    match std::thread::Builder::new()
                        .name(thread_name.clone())
                        .spawn(move || {
                            read_stdout_lines(stdout, tx, parser);
                        }) {
                        Ok(h) => h,
                        Err(e) => {
                            let err = format!("Failed to spawn {} reader thread: {e}", thread_name);
                            child.kill().ok();
                            self.init_error = Some(err.clone());
                            return Err(err);
                        }
                    };
                let stderr_handle = match std::thread::Builder::new()
                    .name(stderr_thread_name.clone())
                    .spawn(move || {
                        read_stderr_lines(stderr, stderr_tx);
                    }) {
                    Ok(h) => h,
                    Err(e) => {
                        let err =
                            format!("Failed to spawn {stderr_thread_name} reader thread: {e}");
                        child.kill().ok();
                        handle.join().ok();
                        self.init_error = Some(err.clone());
                        return Err(err);
                    }
                };

                self.child = Some(child);
                self.stdin = Some(stdin);
                self.message_rx = Some(rx);
                self.reader_handle = Some(handle);
                self.stderr_rx = Some(stderr_rx);
                self.stderr_handle = Some(stderr_handle);
                Ok(true)
            }
            Err(e) => {
                let err = format!("Failed to start {}: {e}", config.command);
                self.init_error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Take the init error (consumed on first call, like Option::take).
    pub fn take_init_error(&mut self) -> Option<String> {
        self.init_error.take()
    }

    /// Write raw bytes to the subprocess stdin.
    /// Returns false if the subprocess has died.
    pub fn write_stdin(&mut self, data: &[u8]) -> bool {
        if let Some(ref mut stdin) = self.stdin {
            if stdin.write_all(data).is_ok() {
                return true;
            }
        }
        self.capture_stderr();
        // Process died — cleanup
        self.cleanup_dead_process();
        false
    }

    /// Drain all available messages from the reader thread (non-blocking).
    pub fn drain_messages(&mut self) -> Vec<DecodedMessage> {
        self.capture_stderr();
        let mut messages = Vec::new();
        if let Some(ref rx) = self.message_rx {
            while let Ok(msg) = rx.try_recv() {
                messages.push(msg);
            }
        }
        messages
    }

    /// Return recent stderr output from the subprocess, if any.
    pub fn take_recent_stderr(&mut self) -> Option<String> {
        self.capture_stderr();
        if self.recent_stderr.is_empty() {
            return None;
        }
        Some(self.recent_stderr.drain(..).collect::<Vec<_>>().join(" | "))
    }

    /// Kill the subprocess and clean up all resources.
    pub fn kill(&mut self) {
        // Drop stdin first so tool sees EOF
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            child.kill().ok();
            child.wait().ok();
        }
        if let Some(handle) = self.reader_handle.take() {
            handle.join().ok();
        }
        if let Some(handle) = self.stderr_handle.take() {
            handle.join().ok();
        }
        self.capture_stderr();
        self.message_rx = None;
        self.stderr_rx = None;
    }

    /// Reset: kill subprocess and allow re-initialization.
    pub fn reset(&mut self) {
        self.kill();
        self.init_attempted = false;
        self.init_error = None;
    }

    /// Clean up after a dead subprocess (stdin write failed).
    fn cleanup_dead_process(&mut self) {
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            child.kill().ok();
            child.wait().ok();
        }
        if let Some(handle) = self.reader_handle.take() {
            handle.join().ok();
        }
        if let Some(handle) = self.stderr_handle.take() {
            handle.join().ok();
        }
        self.capture_stderr();
        self.message_rx = None;
        self.stderr_rx = None;
    }

    fn capture_stderr(&mut self) {
        const MAX_STDERR_LINES: usize = 8;

        if let Some(ref rx) = self.stderr_rx {
            while let Ok(line) = rx.try_recv() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if self.recent_stderr.len() == MAX_STDERR_LINES {
                    self.recent_stderr.pop_front();
                }
                self.recent_stderr.push_back(trimmed.to_string());
            }
        }
    }
}

impl Drop for SubprocessBridge {
    fn drop(&mut self) {
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            child.kill().ok();
            child.wait().ok();
        }
        if let Some(handle) = self.reader_handle.take() {
            handle.join().ok();
        }
        if let Some(handle) = self.stderr_handle.take() {
            handle.join().ok();
        }
        self.capture_stderr();
        // Reader thread exits when stdout closes
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Read lines from stdout and parse them via the OutputParser.
fn read_stdout_lines(
    stdout: ChildStdout,
    tx: Sender<DecodedMessage>,
    mut parser: Box<dyn OutputParser>,
) {
    use std::io::{BufRead, BufReader};

    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(msg) = parser.parse_line(trimmed) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    }
}

fn read_stderr_lines(stderr: ChildStderr, tx: Sender<String>) {
    use std::io::{BufRead, BufReader};

    let reader = BufReader::new(stderr);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if tx.send(line).is_err() {
            break;
        }
    }
}

/// Placeholder parser used after the real parser is moved to the reader thread.
struct NullParser;
impl OutputParser for NullParser {
    fn parse_line(&mut self, _line: &str) -> Option<DecodedMessage> {
        None
    }
}

// ============================================================================
// Conversion helpers
// ============================================================================

/// Convert cf32 IQ samples to unsigned 8-bit IQ (cu8).
///
/// cf32: `(f32, f32)` range `[-1, 1]`
/// cu8: `(u8, u8)` range `[0, 255]`, center at 127.5
pub fn samples_to_cu8(samples: &[Sample]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        buf.push(((s.re * 127.5) + 127.5).clamp(0.0, 255.0) as u8);
        buf.push(((s.im * 127.5) + 127.5).clamp(0.0, 255.0) as u8);
    }
    buf
}

/// Convert f32 audio samples to S16LE bytes.
///
/// Input: f32 range `[-1, 1]`
/// Output: signed 16-bit little-endian PCM
pub fn audio_to_s16le(audio: &[f32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(audio.len() * 2);
    for &sample in audio {
        let clamped = sample.clamp(-1.0, 1.0);
        let s16 = (clamped * 32767.0) as i16;
        buf.extend_from_slice(&s16.to_le_bytes());
    }
    buf
}

/// FM-demodulate cf32 IQ samples to f32 audio.
///
/// Uses quadrature demodulation: Δφ between consecutive samples.
/// Returns audio samples in approximately [-π, π] range, normalized to [-1, 1].
pub fn fm_demod(samples: &[Sample], prev_sample: &mut Sample) -> Vec<f32> {
    let mut audio = Vec::with_capacity(samples.len());
    for &s in samples {
        let dot = s.re * prev_sample.re + s.im * prev_sample.im;
        let cross = s.im * prev_sample.re - s.re * prev_sample.im;
        let phase_diff = cross.atan2(dot);
        // Normalize from [-π, π] to approximately [-1, 1]
        audio.push(phase_diff * std::f32::consts::FRAC_1_PI);
        *prev_sample = s;
    }
    audio
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn cu8_conversion_center() {
        let samples = vec![Sample::new(0.0, 0.0)];
        let cu8 = samples_to_cu8(&samples);
        assert_eq!(cu8, vec![127, 127]);
    }

    #[test]
    fn cu8_conversion_extremes() {
        let samples = vec![Sample::new(1.0, -1.0)];
        let cu8 = samples_to_cu8(&samples);
        assert_eq!(cu8, vec![255, 0]);
    }

    #[test]
    fn cu8_conversion_clamp() {
        let samples = vec![Sample::new(2.0, -2.0)];
        let cu8 = samples_to_cu8(&samples);
        assert_eq!(cu8, vec![255, 0]);
    }

    #[test]
    fn s16le_conversion_center() {
        let audio = vec![0.0f32];
        let bytes = audio_to_s16le(&audio);
        assert_eq!(bytes, vec![0, 0]);
    }

    #[test]
    fn s16le_conversion_max() {
        let audio = vec![1.0f32];
        let bytes = audio_to_s16le(&audio);
        let val = i16::from_le_bytes([bytes[0], bytes[1]]);
        assert_eq!(val, 32767);
    }

    #[test]
    fn s16le_conversion_min() {
        let audio = vec![-1.0f32];
        let bytes = audio_to_s16le(&audio);
        let val = i16::from_le_bytes([bytes[0], bytes[1]]);
        assert_eq!(val, -32767);
    }

    #[test]
    fn s16le_conversion_clamp() {
        let audio = vec![2.0f32, -2.0f32];
        let bytes = audio_to_s16le(&audio);
        let v1 = i16::from_le_bytes([bytes[0], bytes[1]]);
        let v2 = i16::from_le_bytes([bytes[2], bytes[3]]);
        assert_eq!(v1, 32767);
        assert_eq!(v2, -32767);
    }

    #[test]
    fn fm_demod_silent() {
        // Constant-phase signal → zero frequency deviation
        let samples = vec![Sample::new(1.0, 0.0); 10];
        let mut prev = Sample::new(1.0, 0.0);
        let audio = fm_demod(&samples, &mut prev);
        for &s in &audio {
            assert!(s.abs() < 0.01, "Expected ~0 deviation, got {s}");
        }
    }

    #[test]
    fn subprocess_bridge_new() {
        let bridge = SubprocessBridge::new();
        assert!(!bridge.is_running());
        assert!(!bridge.init_attempted);
    }

    #[test]
    fn subprocess_bridge_reset() {
        let mut bridge = SubprocessBridge::new();
        bridge.init_attempted = true;
        bridge.init_error = Some("test".to_string());
        bridge.reset();
        assert!(!bridge.init_attempted);
        assert!(bridge.init_error.is_none());
    }

    #[test]
    fn subprocess_bridge_captures_stderr_on_failure() {
        let mut bridge = SubprocessBridge::new();
        let mut config = SubprocessConfig {
            command: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "echo backend failed >&2; exit 1".to_string(),
            ],
            input_format: InputFormat::Cu8Iq,
            output_parser: Box::new(NullParser),
            thread_name: "stderr-test".to_string(),
        };

        bridge.ensure_started(&mut config).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(!bridge.write_stdin(&[0, 1, 2, 3]));

        let stderr = bridge.take_recent_stderr().unwrap();
        assert!(stderr.contains("backend failed"));
    }
}
