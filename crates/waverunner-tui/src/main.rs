//! WaveRunner TUI — Terminal-based SDR interface.
//!
//! Architecture (using SessionManager):
//!
//! ```text
//! ┌──────────────────┐    ┌────────────────────┐
//! │ SessionManager   │    │ UI Thread           │
//! │ (HW + DSP +     │──→ │ (event consumer +   │
//! │  recording +     │←── │  ratatui rendering) │
//! │  demod + decode) │    │                     │
//! └──────────────────┘    └────────────────────┘
//!     Events ──────────→      ←── Commands
//! ```
//!
//! The SessionManager runs hardware and DSP in background threads.
//! The UI thread drains events, updates app state, renders at 30 FPS,
//! and sends commands in response to keyboard input.

mod app;
mod constellation;
mod input;
mod spectrum;
mod ui;
mod waterfall;

use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::KeyEventKind;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::ExecutableCommand;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use wavecore::analysis;
use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::dsp::decoders;
use wavecore::session::manager::SessionManager;
use wavecore::session::{Command, DemodConfig, Event, SessionConfig};
use wavecore::util::{parse_frequency, parse_gain};

use app::{App, DemodMode};
use input::{Action, handle_key, poll_event};

#[derive(Parser)]
#[command(name = "waverunner-tui", about = "WaveRunner Terminal SDR Interface")]
struct Cli {
    /// Center frequency (supports suffixes: k, M, G)
    #[arg(default_value = "100M")]
    frequency: String,

    /// Sample rate in S/s
    #[arg(short, long, default_value = "2048000")]
    sample_rate: String,

    /// Gain in dB, or "auto" for AGC
    #[arg(short, long, default_value = "auto")]
    gain: String,

    /// FFT size for spectrum analysis
    #[arg(long, default_value = "2048")]
    fft_size: usize,

    /// PPM frequency correction
    #[arg(long, default_value = "0")]
    ppm: i32,

    /// CFAR false alarm probability
    #[arg(long, default_value = "1e-4")]
    pfa: f64,

    /// Device index
    #[arg(short, long, default_value = "0")]
    device: u32,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let frequency = parse_frequency(&cli.frequency).map_err(|e| anyhow::anyhow!("{e}"))?;
    let sample_rate = parse_frequency(&cli.sample_rate).map_err(|e| anyhow::anyhow!("{e}"))?;
    let gain_mode = parse_gain(&cli.gain).map_err(|e| anyhow::anyhow!("{e}"))?;

    // Initialize tracing to file (not stdout — that's the TUI)
    // Initialize tracing to file — fall back to sink if open fails
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/waverunner-tui.log");
    match log_file {
        Ok(file) => {
            tracing_subscriber::fmt()
                .with_writer(std::sync::Mutex::new(file))
                .with_env_filter("waverunner_tui=debug,wavecore=info")
                .init();
        }
        Err(e) => {
            eprintln!("Warning: could not open log file: {e}");
            tracing_subscriber::fmt()
                .with_writer(std::io::sink)
                .with_env_filter("waverunner_tui=debug,wavecore=info")
                .init();
        }
    }

    // Create SessionManager — handles hardware, pipeline, and DSP threads
    let config = SessionConfig {
        schema_version: 1,
        device_index: cli.device,
        frequency,
        sample_rate,
        gain: gain_mode,
        ppm: cli.ppm,
        fft_size: cli.fft_size,
        pfa: cli.pfa,
    };

    let mut registry = DecoderRegistry::new();
    decoders::register_all(&mut registry);
    let (session, events) = SessionManager::new(config, registry)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Create application state
    let mut app = App::new(frequency, sample_rate, cli.gain.clone(), cli.fft_size);

    // Set up terminal
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    stdout
        .execute(EnterAlternateScreen)
        .context("Failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create terminal")?;

    // Main UI loop — target 30 FPS (33ms per frame)
    let frame_duration = Duration::from_millis(33);

    while app.is_running() {
        let frame_start = Instant::now();

        // Drain all pending events from SessionManager
        while let Ok(event) = events.try_recv() {
            // Forward to mode controller
            let mode_cmds = app.mode_controller.handle_event(&event);
            for cmd in mode_cmds {
                session.send(cmd).ok();
            }

            match event {
                Event::SpectrumReady(frame) => {
                    app.push_waterfall(&frame.spectrum_db);
                    app.update_from_spectrum(frame);
                }
                Event::Detections(dets) => {
                    app.dsp.detections = dets;
                }
                Event::Stats(stats) => {
                    app.update_from_stats(stats);
                }
                Event::DecodedMessage(msg) => {
                    app.push_decoded_message(msg);
                }
                Event::DemodVis(vis) => {
                    app.update_from_demod_vis(vis);
                }
                Event::Error(e) => {
                    tracing::error!("Session error: {e}");
                }
                Event::AnalysisResult { id: _, result } => {
                    app.analysis_result = Some(result);
                }
                Event::TrackingUpdate(snapshot) => {
                    app.tracking_data = Some(snapshot);
                }
                Event::AnnotationAdded(_) => {}
                Event::Status(_) => {}
            }
        }

        // Mode controller tick
        let tick_cmds = app.mode_controller.tick();
        for cmd in tick_cmds {
            session.send(cmd).ok();
        }
        app.mode_status = app.mode_controller.mode_status();

        // Draw
        terminal.draw(|frame| ui::draw(frame, &app))?;

        app.frame_count += 1;

        // Process input events for the remainder of the frame time
        let elapsed = frame_start.elapsed();
        let remaining = frame_duration.saturating_sub(elapsed);

        if let Some(key) = poll_event(remaining) {
            // Only handle key press events (not release/repeat)
            if key.kind == KeyEventKind::Press {
                let action = handle_key(&mut app, key);
                match action {
                    Action::Quit => app.quit(),
                    Action::TuneUp => {
                        app.tune_up();
                        session.send(Command::Tune(app.frequency)).ok();
                    }
                    Action::TuneDown => {
                        app.tune_down();
                        session.send(Command::Tune(app.frequency)).ok();
                    }
                    Action::StepIncrease => app.step_increase(),
                    Action::StepDecrease => app.step_decrease(),
                    Action::CycleDemod => {
                        let prev = app.demod_mode;
                        app.cycle_demod();
                        send_demod_change(&session, prev, app.demod_mode, &app);
                    }
                    Action::CycleDemodBack => {
                        let prev = app.demod_mode;
                        app.cycle_demod_back();
                        send_demod_change(&session, prev, app.demod_mode, &app);
                    }
                    Action::CycleDecoder => {
                        let prev = app.active_decoder.clone();
                        app.cycle_decoder();
                        send_decoder_change(&session, prev.as_deref(), app.active_decoder.as_deref());
                    }
                    Action::CycleDecoderBack => {
                        let prev = app.active_decoder.clone();
                        app.cycle_decoder_back();
                        send_decoder_change(&session, prev.as_deref(), app.active_decoder.as_deref());
                    }
                    Action::ToggleSquelch => app.toggle_squelch(),
                    Action::SquelchUp => app.squelch_up(),
                    Action::SquelchDown => app.squelch_down(),
                    Action::FrequencyConfirm(freq) => {
                        app.frequency = freq;
                        session.send(Command::Tune(freq)).ok();
                    }
                    Action::CycleViewTab => app.cycle_view_tab(),
                    Action::CycleViewTabBack => app.cycle_view_tab_back(),
                    Action::CycleMode => {
                        let cmds = app.cycle_mode_forward();
                        for cmd in cmds {
                            session.send(cmd).ok();
                        }
                    }
                    Action::CycleModeBack => {
                        let cmds = app.cycle_mode_back();
                        for cmd in cmds {
                            session.send(cmd).ok();
                        }
                    }
                    Action::ToggleGeneralScan => {
                        let cmds = app.toggle_general_scan();
                        for cmd in cmds {
                            session.send(cmd).ok();
                        }
                    }
                    Action::RunMeasurement => {
                        // Measure around center of spectrum (or strongest detection)
                        let fft_size = app.dsp.spectrum_db.len();
                        let (center_bin, width_bins) = if let Some(det) = app.dsp.detections.first() {
                            (det.bin.min(fft_size.saturating_sub(1)), 100)
                        } else {
                            (fft_size / 2, 100)
                        };
                        let request = analysis::AnalysisRequest::MeasureSignal(
                            analysis::measurement::MeasureConfig {
                                signal_center_bin: center_bin,
                                signal_width_bins: width_bins,
                                adjacent_width_bins: width_bins,
                                obw_threshold_db: 26.0,
                            },
                        );
                        session.send(Command::RunAnalysis { id: app.frame_count, request }).ok();
                    }
                    Action::ToggleTracking => {
                        app.tracking_active = !app.tracking_active;
                        if app.tracking_active {
                            session.send(Command::StartTracking).ok();
                        } else {
                            session.send(Command::StopTracking).ok();
                        }
                    }
                    Action::CaptureReference => {
                        session.send(Command::CaptureReference).ok();
                        app.reference_captured = true;
                    }
                    Action::CompareReference => {
                        if app.reference_captured {
                            session.send(Command::RunAnalysis {
                                id: app.frame_count,
                                request: analysis::AnalysisRequest::CompareSpectra,
                            }).ok();
                        }
                    }
                    Action::ExportCsv => {
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let path = format!("/tmp/waverunner_export_{timestamp}.csv");
                        let config = analysis::export::ExportConfig {
                            path: std::path::PathBuf::from(&path),
                            format: analysis::export::ExportFormat::Csv,
                            content: analysis::export::ExportContent::Spectrum {
                                spectrum_db: app.dsp.spectrum_db.clone(),
                                sample_rate: app.sample_rate,
                                center_freq: app.frequency,
                            },
                        };
                        session.send(Command::Export(config)).ok();
                    }
                    Action::AddBookmark => {
                        let text = format!(
                            "Bookmark @ {:.6} MHz",
                            app.frequency / 1e6,
                        );
                        session
                            .send(Command::AddAnnotation {
                                kind: "bookmark".to_string(),
                                text,
                            })
                            .ok();
                        app.annotation_count += 1;
                    }
                    Action::ExportReport => {
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let path = format!(
                            "/tmp/waverunner_timeline_{timestamp}.json"
                        );
                        session
                            .send(Command::ExportTimeline {
                                path: std::path::PathBuf::from(&path),
                                format: wavecore::session::TimelineExportFormat::Json,
                            })
                            .ok();
                    }
                    Action::VolumeUp => {
                        app.volume_up();
                        session.send(Command::SetVolume(app.volume_f32())).ok();
                    }
                    Action::VolumeDown => {
                        app.volume_down();
                        session.send(Command::SetVolume(app.volume_f32())).ok();
                    }
                    Action::VolumeMute => {
                        app.volume_toggle_mute();
                        session.send(Command::SetVolume(app.volume_f32())).ok();
                    }
                    Action::SaveBookmark => {
                        let name = format!("{:.3} MHz", app.frequency / 1e6);
                        let mode = app.demod_mode.session_mode().map(|s| s.to_string());
                        let decoder = app.active_decoder.clone();
                        let bm = wavecore::bookmarks::Bookmark {
                            name: name.clone(),
                            frequency_hz: app.frequency,
                            mode,
                            decoder,
                            notes: None,
                        };
                        let mut store = wavecore::bookmarks::BookmarkStore::load();
                        store.add(bm);
                        if let Err(e) = store.save() {
                            tracing::error!("Failed to save bookmark: {e}");
                        }
                        app.annotation_count += 1; // reuse counter for visual feedback
                    }
                    Action::FrequencyEntry | Action::FrequencyCancel | Action::None => {}
                }
            }
        }
    }

    // Cleanup terminal
    disable_raw_mode().context("Failed to disable raw mode")?;
    terminal
        .backend_mut()
        .execute(LeaveAlternateScreen)
        .context("Failed to leave alternate screen")?;

    // Shutdown session (stops hardware + processing threads)
    session.shutdown();

    println!("WaveRunner TUI exited.");
    Ok(())
}

/// Send decoder enable/disable commands when the active decoder changes.
fn send_decoder_change(session: &SessionManager, prev: Option<&str>, next: Option<&str>) {
    if let Some(name) = prev {
        session.send(Command::DisableDecoder(name.to_string())).ok();
    }
    if let Some(name) = next {
        session.send(Command::EnableDecoder(name.to_string())).ok();
    }
}

/// Send demod start/stop commands when the demod mode changes.
fn send_demod_change(session: &SessionManager, prev: DemodMode, next: DemodMode, app: &App) {
    // Stop current demod if active
    if prev.session_mode().is_some() {
        session.send(Command::StopDemod).ok();
    }

    // Start new demod if not Off
    if let Some(mode) = next.session_mode() {
        let config = DemodConfig {
            mode: mode.to_string(),
            audio_rate: 48000,
            bandwidth: None,
            bfo: None,
            squelch: app.squelch,
            deemph_us: None,
            output_wav: None,
        };
        session.send(Command::StartDemod(config)).ok();
        session.send(Command::SetVolume(app.volume_f32())).ok();
    }
}
