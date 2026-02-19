use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;

use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::recording::RecordingMetadata;
use wavecore::session::manager::SessionManager;
use wavecore::session::{Command, Event, RecordFormat, SessionConfig, StatusUpdate};

use super::parse_frequency;

#[derive(clap::Args)]
pub struct RecordArgs {
    /// Center frequency (supports suffixes: k, M, G)
    #[arg(value_parser = parse_frequency)]
    pub frequency: f64,

    /// Output file path
    #[arg(short, long)]
    pub output: PathBuf,

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
}

pub async fn run(args: RecordArgs, device_index: u32) -> Result<()> {
    let gain_mode = wavecore::util::parse_gain(&args.gain)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

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
    let (session, events) = SessionManager::new(config, registry)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Start recording
    let format = match args.format.as_str() {
        "wav" => RecordFormat::Wav,
        "sigmf" => RecordFormat::SigMf,
        _ => RecordFormat::RawCf32,
    };
    session
        .send(Command::StartRecord {
            path: args.output.clone(),
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
        args.output.display(),
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
        cmd_tx.send(Command::Shutdown).ok();
    });

    // Duration timer (uses std::thread to avoid needing tokio time feature)
    if args.duration > 0.0 {
        let cmd_tx = session.cmd_sender();
        let duration = args.duration;
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs_f64(duration));
            eprintln!("\nDuration reached.");
            cmd_tx.send(Command::StopRecord).ok();
            cmd_tx.send(Command::Shutdown).ok();
        });
    }

    let start = Instant::now();
    let sample_rate = args.sample_rate;
    let mut total_samples = 0u64;

    // Event loop — display progress, wait for recording to stop
    for event in events.iter() {
        match event {
            Event::Status(StatusUpdate::RecordingStopped(samples)) => {
                total_samples = samples;
                break;
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
    let timestamp = chrono_lite_timestamp();
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
    };
    metadata.write_sidecar(&args.output)?;

    session.shutdown();
    eprintln!(
        "\nRecording complete: {} samples ({:.1}s) written to {}",
        total_samples,
        elapsed,
        args.output.display(),
    );

    Ok(())
}

/// Simple ISO 8601 timestamp without pulling in chrono.
fn chrono_lite_timestamp() -> String {
    use std::time::SystemTime;
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();

    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    let mut year = 1970i64;
    let mut remaining_days = days as i64;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }

    let days_in_months: [i64; 12] = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 0;
    for (i, &days) in days_in_months.iter().enumerate() {
        if remaining_days < days {
            month = i + 1;
            break;
        }
        remaining_days -= days;
    }
    let day = remaining_days + 1;

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}
