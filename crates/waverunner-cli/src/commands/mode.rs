use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;

use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::dsp::decoders;
use wavecore::mode::ModeController;
use wavecore::mode::general::{GeneralMode, GeneralModeConfig};
use wavecore::session::manager::SessionManager;
use wavecore::session::{Command, Event, SessionConfig};

use super::parse_frequency;

#[derive(clap::Args)]
pub struct ModeArgs {
    #[command(subcommand)]
    pub action: ModeAction,
}

#[derive(clap::Subcommand)]
pub enum ModeAction {
    /// List available mission profiles
    List,
    /// Run general auto-scan mode
    General(GeneralScanArgs),
    /// Run a named mission profile
    Run(RunProfileArgs),
}

#[derive(clap::Args)]
pub struct GeneralScanArgs {
    /// Start frequency (supports suffixes: k, M, G)
    #[arg(long, value_parser = parse_frequency, default_value = "88M")]
    pub start: f64,

    /// End frequency
    #[arg(long, value_parser = parse_frequency, default_value = "108M")]
    pub end: f64,

    /// Step size in Hz
    #[arg(long, value_parser = parse_frequency)]
    pub step: Option<f64>,

    /// Dwell time per step in milliseconds
    #[arg(long, default_value = "200")]
    pub dwell_ms: u64,

    /// Minimum SNR to park on a signal (dB)
    #[arg(long, default_value = "10")]
    pub min_snr: f32,

    /// Park duration in seconds (0 = indefinite)
    #[arg(long, default_value = "30")]
    pub park_secs: u64,

    /// Disable auto-decoding
    #[arg(long)]
    pub no_decode: bool,

    /// Enable audio demod on parked signals (police scanner mode)
    #[arg(long)]
    pub listen: bool,

    /// Sample rate in S/s
    #[arg(short = 'r', long, default_value = "2048000", value_parser = parse_frequency)]
    pub sample_rate: f64,

    /// Gain in dB, or "auto" for AGC
    #[arg(short, long, default_value = "auto")]
    pub gain: String,
}

#[derive(clap::Args)]
pub struct RunProfileArgs {
    /// Profile name (e.g. "aviation", "pager", "fm-broadcast")
    pub name: String,

    /// Gain override
    #[arg(short, long)]
    pub gain: Option<String>,
}

pub async fn run(args: ModeArgs, device_index: u32) -> Result<()> {
    match args.action {
        ModeAction::List => run_list(),
        ModeAction::General(scan_args) => run_general(scan_args, device_index).await,
        ModeAction::Run(profile_args) => run_profile(profile_args, device_index).await,
    }
}

fn run_list() -> Result<()> {
    let decoder_names: Vec<String> = wavecore::dsp::decoders::DECODER_NAMES
        .iter()
        .map(|s| s.to_string())
        .collect();
    let ctrl = ModeController::new(decoder_names);

    println!("Available mission profiles:");
    println!("{}", "-".repeat(50));

    for name in ctrl.list_profiles() {
        if let Some(profile) = ctrl.get_profile(name) {
            println!("  {:<20} {}", profile.name, profile.description);
        }
    }

    println!();
    println!("Use: waverunner mode run <name>");
    Ok(())
}

async fn run_general(args: GeneralScanArgs, device_index: u32) -> Result<()> {
    let gain_mode = wavecore::util::parse_gain(&args.gain).map_err(|e| anyhow::anyhow!("{e}"))?;

    let config = SessionConfig {
        schema_version: 1,
        device_index,
        frequency: args.start,
        sample_rate: args.sample_rate,
        gain: gain_mode,
        ppm: 0,
        fft_size: 2048,
        pfa: 1e-4,
    };

    let mut registry = DecoderRegistry::new();
    decoders::register_all(&mut registry);
    let (session, events) =
        SessionManager::new(config, registry).map_err(|e| anyhow::anyhow!("{e}"))?;

    let decoder_names: Vec<String> = decoders::DECODER_NAMES
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut mode_ctrl = ModeController::new(decoder_names);

    let scan_config = GeneralModeConfig {
        scan_start: args.start,
        scan_end: args.end,
        step_hz: args.step,
        dwell_ms: args.dwell_ms,
        min_snr_db: args.min_snr,
        park_duration_secs: args.park_secs,
        auto_decode: !args.no_decode,
        sample_rate: args.sample_rate,
        enable_audio: args.listen,
        audio_mode: None,
    };

    let mode = if args.listen {
        let freq_db = wavecore::frequency_db::FrequencyDb::auto_detect();
        GeneralMode::with_freq_db(scan_config, std::sync::Arc::new(freq_db))
    } else {
        GeneralMode::new(scan_config)
    };
    let init_cmds = mode_ctrl.activate(Box::new(mode));
    for cmd in init_cmds {
        session.send(cmd).ok();
    }

    let listen_label = if args.listen { " | Audio: ON" } else { "" };
    println!(
        "General scan: {:.3} MHz - {:.3} MHz | SNR threshold: {:.0} dB{listen_label}",
        args.start / 1e6,
        args.end / 1e6,
        args.min_snr,
    );
    println!("Press Ctrl+C to stop.\n");

    // Ctrl+C handler
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = Arc::clone(&running);
    let cmd_tx = session.cmd_sender();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        running_clone.store(false, Ordering::SeqCst);
        cmd_tx.send(Command::Shutdown).ok();
    });

    run_mode_loop(&session, &events, &mut mode_ctrl, &running);

    session.shutdown();
    println!("\nGeneral scan stopped.");
    Ok(())
}

async fn run_profile(args: RunProfileArgs, device_index: u32) -> Result<()> {
    let decoder_names: Vec<String> = decoders::DECODER_NAMES
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut mode_ctrl = ModeController::new(decoder_names);

    let profile = mode_ctrl
        .get_profile(&args.name)
        .ok_or_else(|| anyhow::anyhow!("Unknown profile: {}", args.name))?
        .clone();

    let gain_str = args
        .gain
        .as_deref()
        .unwrap_or(profile.gain.as_deref().unwrap_or("auto"));
    let gain_mode = wavecore::util::parse_gain(gain_str).map_err(|e| anyhow::anyhow!("{e}"))?;

    let frequency = profile
        .frequencies
        .first()
        .map(|f| f.freq_hz)
        .unwrap_or(100e6);
    let sample_rate = profile.sample_rate.unwrap_or(2_048_000.0);

    let config = SessionConfig {
        schema_version: 1,
        device_index,
        frequency,
        sample_rate,
        gain: gain_mode,
        ppm: 0,
        fft_size: 2048,
        pfa: 1e-4,
    };

    let mut registry = DecoderRegistry::new();
    decoders::register_all(&mut registry);
    let (session, events) =
        SessionManager::new(config, registry).map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("Profile: {} — {}", profile.name, profile.description);
    println!("Press Ctrl+C to stop.\n");

    let gain_override = args.gain.as_ref().map(|_| gain_mode);
    let mode = mode_ctrl
        .create_profile_mode_with_gain(&args.name, gain_override)
        .ok_or_else(|| anyhow::anyhow!("Failed to create mode for profile: {}", args.name))?;
    let init_cmds = mode_ctrl.activate(mode);
    for cmd in init_cmds {
        session.send(cmd).ok();
    }

    // Ctrl+C handler
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = Arc::clone(&running);
    let cmd_tx = session.cmd_sender();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        running_clone.store(false, Ordering::SeqCst);
        cmd_tx.send(Command::Shutdown).ok();
    });

    run_mode_loop(&session, &events, &mut mode_ctrl, &running);

    session.shutdown();
    println!("\nProfile '{}' stopped.", args.name);
    Ok(())
}

/// Common event loop for mode execution.
fn run_mode_loop(
    session: &SessionManager,
    events: &crossbeam_channel::Receiver<Event>,
    mode_ctrl: &mut ModeController,
    running: &Arc<AtomicBool>,
) {
    let tick_interval = Duration::from_millis(33); // ~30 Hz
    let mut last_tick = Instant::now();
    let mut last_status = String::new();

    while running.load(Ordering::SeqCst) && session.is_running() {
        // Drain events
        while let Ok(event) = events.try_recv() {
            // Print decoded messages
            if let Event::DecodedMessage(ref msg) = event {
                println!("[{}] {}", msg.decoder.to_uppercase(), msg.summary,);
                for (key, value) in &msg.fields {
                    println!("  {:>16}: {}", key, value);
                }
            }

            // Forward to mode controller
            let cmds = mode_ctrl.handle_event(&event);
            for cmd in cmds {
                session.send(cmd).ok();
            }
        }

        // Periodic tick
        if last_tick.elapsed() >= tick_interval {
            let cmds = mode_ctrl.tick();
            for cmd in cmds {
                session.send(cmd).ok();
            }

            // Print status updates
            if let Some(status) = mode_ctrl.mode_status() {
                if status != last_status {
                    use std::io::Write;
                    eprint!("\r  {:<60}", status);
                    std::io::stderr().flush().ok();
                    last_status = status;
                }
            }

            last_tick = Instant::now();
        }

        std::thread::sleep(Duration::from_millis(5));
    }
}
