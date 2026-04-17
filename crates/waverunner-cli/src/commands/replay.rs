use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;

use wavecore::captures::{find_capture, inspect_capture_input, latest_capture};
use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::dsp::decoders;
use wavecore::hardware::GainMode;
use wavecore::session::manager::SessionManager;
use wavecore::session::replay::{ReplayDevice, ReplayOptions};
use wavecore::session::{Command, DemodConfig, Event, SessionConfig, StatusUpdate};
use wavecore::util::format_freq;

use super::decode::{OutputFormat, print_message};
use super::demod::DemodMode;
use super::parse_frequency;

#[derive(clap::Args)]
pub struct ReplayArgs {
    /// Input IQ capture (.cf32, .wav, .cu8, .sigmf-data, .sigmf-meta, or SigMF stem)
    #[arg(
        required_unless_present_any = ["latest", "capture"],
        conflicts_with_all = ["latest", "capture"]
    )]
    pub input: Option<String>,

    /// Replay the newest indexed capture from the local library catalog
    #[arg(long, conflicts_with_all = ["input", "capture"])]
    pub latest: bool,

    /// Replay one indexed capture by selector (`latest`, id, capture path, or metadata path)
    #[arg(long, conflicts_with_all = ["input", "latest"])]
    pub capture: Option<String>,

    /// Sample rate in S/s. Defaults to metadata/SigMF when available.
    #[arg(short, long, value_parser = parse_frequency)]
    pub sample_rate: Option<f64>,

    /// Center frequency in Hz. Defaults to metadata/SigMF when available.
    #[arg(short, long, value_parser = parse_frequency)]
    pub frequency: Option<f64>,

    /// Start audio demodulation while replaying
    #[arg(short, long)]
    pub mode: Option<DemodMode>,

    /// Enable one or more decoders during replay (repeatable)
    #[arg(short = 'D', long = "decoder")]
    pub decoders: Vec<String>,

    /// Replay continuously instead of stopping at EOF
    #[arg(long = "loop")]
    pub loop_forever: bool,

    /// Disable real-time pacing and replay as fast as the pipeline allows
    #[arg(long)]
    pub fast: bool,

    /// Replay callback block size in IQ samples
    #[arg(long, default_value = "262144")]
    pub block_size: usize,

    /// Audio output rate in Hz when demodulating
    #[arg(long, default_value = "48000")]
    pub audio_rate: u32,

    /// Channel bandwidth override in Hz for demodulation
    #[arg(short, long)]
    pub bandwidth: Option<f64>,

    /// Squelch threshold in dBFS
    #[arg(long)]
    pub squelch: Option<f64>,

    /// BFO offset in Hz (SSB/CW)
    #[arg(long)]
    pub bfo: Option<f64>,

    /// De-emphasis time constant in µs
    #[arg(long)]
    pub deemph: Option<f64>,

    /// Write demodulated audio to a WAV file
    #[arg(short, long)]
    pub output: Option<String>,

    /// Output format for decoded messages
    #[arg(short = 'F', long, default_value = "text")]
    pub format: OutputFormat,
}

pub async fn run(args: ReplayArgs, _device_index: u32) -> Result<()> {
    if args.fast && args.mode.is_some() && args.output.is_none() {
        anyhow::bail!(
            "--fast with live audio demod is not useful. Add --output to capture audio or omit --fast."
        );
    }

    let input = resolve_input_path(&args)?;
    let capture = inspect_capture_input(Path::new(&input))
        .map_err(|e| anyhow::anyhow!("Failed to inspect capture: {e}"))?;
    let data_path = Path::new(&capture.data_path);
    let sample_rate = args
        .sample_rate
        .or(capture.sample_rate)
        .ok_or_else(|| anyhow::anyhow!("Sample rate is required when capture metadata is missing. Pass --sample-rate explicitly."))?;
    let frequency = args.frequency.or(capture.center_freq);

    if !args.decoders.is_empty() && frequency.is_none() {
        anyhow::bail!(
            "Center frequency is required when enabling decoders and could not be inferred from metadata. Pass --frequency explicitly."
        );
    }

    let nominal_frequency = frequency.unwrap_or(0.0);
    let device = ReplayDevice::open_with_options(
        data_path,
        sample_rate,
        ReplayOptions {
            realtime: !args.fast,
            block_size: args.block_size,
            looping: args.loop_forever,
        },
    )
    .map_err(|e| anyhow::anyhow!("Failed to open replay file: {e}"))?;

    let mut registry = DecoderRegistry::new();
    decoders::register_all(&mut registry);
    for decoder in &args.decoders {
        if registry.create(decoder).is_none() {
            anyhow::bail!("Unknown decoder: {decoder}");
        }
    }

    let config = SessionConfig {
        schema_version: 1,
        device_index: 0,
        frequency: nominal_frequency,
        sample_rate,
        gain: GainMode::Auto,
        ppm: 0,
        fft_size: 2048,
        pfa: 1e-4,
    };

    let (session, event_rx) = SessionManager::new_with_device(config, device, registry)
        .map_err(|e| anyhow::anyhow!("Failed to start replay session: {e}"))?;

    if let Some(mode) = args.mode {
        session
            .send(Command::StartDemod(DemodConfig {
                mode: mode.as_str().to_string(),
                audio_rate: args.audio_rate,
                bandwidth: args.bandwidth,
                bfo: args.bfo,
                squelch: args.squelch,
                deemph_us: args.deemph,
                output_wav: args.output.as_ref().map(|path| path.into()),
                emit_visualization: false,
                spectrum_update_interval_blocks: 8,
            }))
            .map_err(|e| anyhow::anyhow!("Failed to start demodulator: {e}"))?;
    } else if args.output.is_some() {
        anyhow::bail!("--output requires --mode");
    }

    for decoder in &args.decoders {
        session
            .send(Command::EnableDecoder(decoder.clone()))
            .map_err(|e| anyhow::anyhow!("Failed to enable decoder {decoder}: {e}"))?;
    }

    print_banner(
        &args,
        data_path,
        sample_rate,
        frequency,
        &capture.metadata_path,
    );

    let running = session.running_flag();
    let cmd_tx = session.cmd_sender();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        running.store(false, std::sync::atomic::Ordering::Relaxed);
        cmd_tx.send(Command::StopDemod).ok();
        cmd_tx.send(Command::Shutdown).ok();
    });

    let mut msg_count = 0u64;
    let start_time = Instant::now();
    let mut last_status = Instant::now();

    while session.is_running() {
        match event_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::DecodedMessage(msg)) => {
                msg_count += 1;
                print_message(&msg, msg_count, &args.format);
            }
            Ok(Event::Stats(stats)) => {
                if matches!(args.format, OutputFormat::Text)
                    && last_status.elapsed() >= Duration::from_millis(250)
                {
                    let health = match stats.health {
                        wavecore::session::HealthStatus::Normal => "normal",
                        wavecore::session::HealthStatus::Warning => "warning",
                        wavecore::session::HealthStatus::Critical => "critical",
                    };
                    eprint!(
                        "\r  [{:.1}s] blocks: {} | messages: {} | health: {} | CPU {:>5.1}% ",
                        start_time.elapsed().as_secs_f64(),
                        stats.blocks_processed,
                        msg_count,
                        health,
                        stats.cpu_load_percent,
                    );
                    use std::io::Write;
                    std::io::stderr().flush().ok();
                    last_status = Instant::now();
                }
            }
            Ok(Event::Status(StatusUpdate::HealthChanged(status))) => {
                if matches!(args.format, OutputFormat::Text) {
                    eprintln!("\nHealth changed: {status:?}");
                }
            }
            Ok(Event::Error(e)) => eprintln!("\nError: {e}"),
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    while let Ok(event) = event_rx.try_recv() {
        if let Event::DecodedMessage(msg) = event {
            msg_count += 1;
            print_message(&msg, msg_count, &args.format);
        }
    }

    session.shutdown();
    let elapsed = start_time.elapsed().as_secs_f64();
    eprintln!(
        "\nReplay finished. {} decoded message(s) in {:.1}s.",
        msg_count, elapsed
    );
    Ok(())
}

fn resolve_input_path(args: &ReplayArgs) -> Result<String> {
    if let Some(selector) = args.capture.as_deref() {
        let capture = find_capture(selector).map_err(|e| anyhow::anyhow!("{e}"))?;
        return Ok(capture.metadata_path.unwrap_or(capture.path));
    }

    if args.latest {
        let capture = latest_capture().map_err(|e| anyhow::anyhow!("{e}"))?;
        return Ok(capture.metadata_path.unwrap_or(capture.path));
    }

    args.input
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Replay input path is required"))
}

fn print_banner(
    args: &ReplayArgs,
    data_path: &Path,
    sample_rate: f64,
    frequency: Option<f64>,
    metadata_path: &Option<String>,
) {
    let freq_label = frequency
        .map(format_freq)
        .unwrap_or_else(|| "(unknown center frequency)".to_string());
    println!(
        "Replaying {} | Rate: {:.3} MS/s | Freq: {}",
        data_path.display(),
        sample_rate / 1e6,
        freq_label,
    );
    if let Some(path) = metadata_path {
        println!("Metadata: {path}");
    }
    println!(
        "Pacing: {} | Loop: {} | Decoders: {}{}",
        if args.fast { "fast" } else { "real-time" },
        if args.loop_forever { "on" } else { "off" },
        if args.decoders.is_empty() {
            "-".to_string()
        } else {
            args.decoders.join(", ")
        },
        args.mode
            .map(|mode| format!(" | Demod: {}", mode.as_str().to_uppercase()))
            .unwrap_or_default(),
    );
    println!("Press Ctrl+C to stop.\n");
}
