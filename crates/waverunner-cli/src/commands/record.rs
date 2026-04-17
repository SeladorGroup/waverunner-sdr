use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;

use wavecore::captures::{CaptureCatalog, CaptureSource, default_capture_path};
use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::recording::RecordingMetadata;
use wavecore::session::manager::SessionManager;
use wavecore::session::{Command, Event, RecordFormat, SessionConfig, StatusUpdate};
use wavecore::util::utc_timestamp_now;

use super::parse_frequency;

#[derive(clap::Args)]
pub struct RecordArgs {
    /// Center frequency (supports suffixes: k, M, G)
    #[arg(value_parser = parse_frequency)]
    pub frequency: f64,

    /// Output file path. If omitted, WaveRunner generates one in the capture library.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Duration in seconds (0 = until Ctrl+C)
    #[arg(short = 'D', long, default_value = "0")]
    pub duration: f64,

    /// Output format: raw (interleaved f32), wav (2-ch float WAV), or sigmf
    #[arg(short, long, default_value = "raw", value_parser = ["raw", "wav", "sigmf"])]
    pub format: String,

    /// Sample rate in S/s
    #[arg(short = 'r', long, default_value = "2048000", value_parser = parse_frequency)]
    pub sample_rate: f64,

    /// Gain in dB, or "auto" for AGC
    #[arg(short, long, default_value = "auto")]
    pub gain: String,

    /// PPM frequency correction
    #[arg(long, default_value = "0")]
    pub ppm: i32,

    /// Optional human-readable label stored alongside the recording
    #[arg(long)]
    pub label: Option<String>,

    /// Optional notes stored alongside the recording
    #[arg(long)]
    pub notes: Option<String>,

    /// Tags stored alongside the recording (repeatable)
    #[arg(long = "tag")]
    pub tags: Vec<String>,

    /// Export the session timeline next to the recording
    #[arg(long)]
    pub timeline: bool,

    /// Explicit timeline output path (defaults to `<recording>.timeline.json`)
    #[arg(long)]
    pub timeline_output: Option<PathBuf>,
}

pub async fn run(args: RecordArgs, device_index: u32) -> Result<()> {
    let gain_mode = wavecore::util::parse_gain(&args.gain).map_err(|e| anyhow::anyhow!("{e}"))?;
    let output_path = args.output.clone().unwrap_or(
        default_capture_path(&args.format, args.label.as_deref())
            .map_err(|e| anyhow::anyhow!("Failed to create default output path: {e}"))?,
    );

    let config = SessionConfig {
        schema_version: 1,
        device_index,
        frequency: args.frequency,
        sample_rate: args.sample_rate,
        gain: gain_mode,
        ppm: args.ppm,
        fft_size: 1024,
        pfa: 1e-4,
    };

    let registry = DecoderRegistry::new();
    let (session, events) =
        SessionManager::new(config, registry).map_err(|e| anyhow::anyhow!("{e}"))?;

    // Start recording
    let format = match args.format.as_str() {
        "wav" => RecordFormat::Wav,
        "sigmf" => RecordFormat::SigMf,
        _ => RecordFormat::RawCf32,
    };
    session
        .send(Command::StartRecord {
            path: output_path.clone(),
            format,
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let duration_str = if args.duration > 0.0 {
        format!("{:.1}s", args.duration)
    } else {
        "until Ctrl+C".to_string()
    };

    println!(
        "Recording {:.6} MHz to {} ({}, {:.3} MS/s, {})",
        args.frequency / 1e6,
        output_path.display(),
        args.format.to_uppercase(),
        args.sample_rate / 1e6,
        duration_str,
    );

    // Ctrl+C handler
    let cmd_tx = session.cmd_sender();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        eprintln!("\nStopping recording...");
        cmd_tx.send(Command::StopRecord).ok();
    });

    // Duration timer (uses std::thread to avoid needing tokio time feature)
    if args.duration > 0.0 {
        let cmd_tx = session.cmd_sender();
        let duration = args.duration;
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs_f64(duration));
            eprintln!("\nDuration reached.");
            cmd_tx.send(Command::StopRecord).ok();
        });
    }

    let start = Instant::now();
    let sample_rate = args.sample_rate;
    let mut total_samples = 0u64;
    let timeline_path = if args.timeline {
        Some(
            args.timeline_output
                .clone()
                .unwrap_or_else(|| default_timeline_path(&output_path)),
        )
    } else {
        None
    };
    let mut recording_stopped = false;
    let mut timeline_exported = timeline_path.is_none();

    // Event loop — display progress, wait for recording to stop
    for event in events.iter() {
        match event {
            Event::Status(StatusUpdate::RecordingStopped(samples)) => {
                total_samples = samples;
                recording_stopped = true;
                if let Some(path) = timeline_path.clone() {
                    session
                        .send(Command::ExportTimeline {
                            path,
                            format: wavecore::session::TimelineExportFormat::Json,
                        })
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                } else {
                    break;
                }
            }
            Event::Status(StatusUpdate::TimelineExported(_)) => {
                timeline_exported = true;
                if recording_stopped {
                    break;
                }
            }
            Event::Stats(_) => {
                // Estimate progress from elapsed time
                let elapsed = start.elapsed().as_secs_f64();
                let est_samples = (elapsed * sample_rate) as u64;
                let est_mb = (est_samples * 8) as f64 / 1_048_576.0;
                eprint!(
                    "\r  {:.1}s | ~{} samples | ~{:.2} MB",
                    elapsed, est_samples, est_mb,
                );
                use std::io::Write;
                std::io::stderr().flush().ok();
            }
            Event::Error(e) => {
                eprintln!("\nError: {e}");
                break;
            }
            _ => {}
        }
    }

    let elapsed = start.elapsed().as_secs_f64();

    // Write metadata sidecar
    let timestamp = utc_timestamp_now();
    let metadata = RecordingMetadata {
        schema_version: 1,
        center_freq: args.frequency,
        sample_rate: args.sample_rate,
        gain: args.gain.clone(),
        format: match args.format.as_str() {
            "wav" => "cf32-wav".to_string(),
            "sigmf" => "sigmf-cf32_le".to_string(),
            _ => "cf32".to_string(),
        },
        timestamp,
        duration_secs: Some(elapsed),
        device: "rtlsdr".to_string(),
        samples_written: total_samples,
        label: args.label.clone(),
        notes: args.notes.clone(),
        tags: args.tags.clone(),
        demod_mode: None,
        decoder: None,
        timeline_path: timeline_path
            .filter(|_| timeline_exported)
            .map(|path| path.display().to_string()),
        report_path: None,
    };
    metadata.write_sidecar(&output_path)?;

    let mut catalog = CaptureCatalog::load();
    catalog.register(&output_path, &metadata, CaptureSource::LiveRecord);
    if let Err(e) = catalog.save() {
        eprintln!("Failed to update recent capture catalog: {e}");
    }

    session.shutdown();
    eprintln!(
        "\nRecording complete: {} samples ({:.1}s) written to {}",
        total_samples,
        elapsed,
        output_path.display(),
    );

    Ok(())
}

fn default_timeline_path(recording_path: &Path) -> PathBuf {
    let file_name = recording_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("capture");
    recording_path.with_file_name(format!("{file_name}.timeline.json"))
}
