use std::time::Instant;

use anyhow::Result;
use crossbeam_channel::select;

use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::dsp::demod::mode_defaults;
use wavecore::session::manager::SessionManager;
use wavecore::session::{Command, DemodConfig, Event, SessionConfig, StatusUpdate};

use super::parse_frequency;

#[derive(clap::Args)]
pub struct DemodArgs {
    /// Center frequency (supports suffixes: k, M, G)
    #[arg(value_parser = parse_frequency)]
    pub frequency: f64,

    /// Demodulation mode
    #[arg(short, long)]
    pub mode: DemodMode,

    /// Sample rate in S/s (default: auto-select based on mode)
    #[arg(short, long, value_parser = parse_frequency)]
    pub sample_rate: Option<f64>,

    /// Gain in dB, or "auto" for AGC
    #[arg(short, long, default_value = "auto")]
    pub gain: String,

    /// PPM frequency correction
    #[arg(long, default_value = "0")]
    pub ppm: i32,

    /// Channel bandwidth in Hz (default: auto based on mode)
    #[arg(short, long)]
    pub bandwidth: Option<f64>,

    /// Squelch threshold in dBFS (e.g., -30). Disabled if not set.
    #[arg(long)]
    pub squelch: Option<f64>,

    /// Audio output rate in Hz
    #[arg(long, default_value = "48000")]
    pub audio_rate: u32,

    /// BFO offset in Hz (for SSB/CW, default: 1500 for SSB, 700 for CW)
    #[arg(long)]
    pub bfo: Option<f64>,

    /// De-emphasis time constant in us (0=disable, 50=EU, 75=US). Default: 75 for WFM.
    #[arg(long)]
    pub deemph: Option<f64>,

    /// Output WAV file for recording demodulated audio
    #[arg(short, long)]
    pub output: Option<String>,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum DemodMode {
    Am,
    #[value(name = "am-sync")]
    AmSync,
    Fm,
    Wfm,
    #[value(name = "wfm-stereo")]
    WfmStereo,
    Usb,
    Lsb,
    Cw,
}

impl DemodMode {
    /// Convert CLI enum to string key for SessionManager.
    fn as_str(&self) -> &'static str {
        match self {
            DemodMode::Am => "am",
            DemodMode::AmSync => "am-sync",
            DemodMode::Fm => "fm",
            DemodMode::Wfm => "wfm",
            DemodMode::WfmStereo => "wfm-stereo",
            DemodMode::Usb => "usb",
            DemodMode::Lsb => "lsb",
            DemodMode::Cw => "cw",
        }
    }
}

pub async fn run(args: DemodArgs, device_index: u32) -> Result<()> {
    let mode_str = args.mode.as_str();

    // Get defaults from wavecore for this mode
    let defaults = mode_defaults(mode_str)
        .ok_or_else(|| anyhow::anyhow!("Unknown demod mode: {mode_str}"))?;

    let sample_rate = args.sample_rate.unwrap_or(defaults.sample_rate);
    let gain_mode = wavecore::util::parse_gain(&args.gain)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let config = SessionConfig {
        device_index,
        frequency: args.frequency,
        sample_rate,
        gain: gain_mode,
        ppm: args.ppm,
        fft_size: 1024,
        pfa: 1e-4,
    };

    let registry = DecoderRegistry::new();
    let (session, events) = SessionManager::new(config, registry)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Start demod
    let demod_config = DemodConfig {
        mode: mode_str.to_string(),
        audio_rate: args.audio_rate,
        bandwidth: args.bandwidth,
        bfo: args.bfo,
        squelch: args.squelch,
        deemph_us: args.deemph,
        output_wav: args.output.as_ref().map(|s| s.into()),
    };

    session
        .send(Command::StartDemod(demod_config))
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Print header
    println!(
        "Demod {} @ {:.6} MHz | Rate: {:.3} MS/s → {} Hz audio",
        mode_str.to_uppercase(),
        args.frequency / 1e6,
        sample_rate / 1e6,
        args.audio_rate,
    );
    if let Some(sq) = args.squelch {
        println!("Squelch: {sq:.0} dBFS");
    }
    println!("Press Ctrl+C to stop.\n");

    // Ctrl+C handler
    let cmd_tx = session.cmd_sender();
    let (stop_tx, stop_rx) = crossbeam_channel::bounded::<()>(1);
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        println!("\nStopping...");
        cmd_tx.send(Command::StopDemod).ok();
        cmd_tx.send(Command::Shutdown).ok();
        stop_tx.send(()).ok();
    });

    let start = Instant::now();

    // Event loop — display status
    loop {
        select! {
            recv(stop_rx) -> _ => break,
            recv(events) -> event => {
                match event {
                    Ok(Event::SpectrumReady(frame)) => {
                        let elapsed = start.elapsed().as_secs_f64();
                        print!(
                            "\r  {:.1}s | AGC: {:+.1} dB | RMS: {:.1} dBFS ",
                            elapsed,
                            frame.agc_gain_db,
                            frame.rms_dbfs,
                        );
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                    Ok(Event::Error(e)) => {
                        eprintln!("\nError: {e}");
                    }
                    Ok(Event::Status(StatusUpdate::Streaming)) => {
                        // Demod started successfully
                    }
                    Err(_) => break,
                    _ => {}
                }
            }
        }
    }

    if let Some(ref path) = args.output {
        println!("\nSaved audio to: {path}");
    }

    session.shutdown();

    let elapsed = start.elapsed().as_secs_f64();
    println!("Done. {elapsed:.1}s demodulated.");
    Ok(())
}
