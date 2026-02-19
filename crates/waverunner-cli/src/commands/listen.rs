use std::time::Instant;

use anyhow::Result;
use crossbeam_channel::select;

use wavecore::bookmarks::BookmarkStore;
use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::dsp::demod::mode_defaults;
use wavecore::frequency_db::FrequencyDb;
use wavecore::session::manager::SessionManager;
use wavecore::session::{Command, DemodConfig, Event, SessionConfig};

#[derive(clap::Args)]
pub struct ListenArgs {
    /// Center frequency (e.g. 98.3M) or bookmark name
    pub frequency: String,

    /// Demodulation mode (auto-detected from frequency if omitted)
    #[arg(short, long)]
    pub mode: Option<ListenMode>,

    /// Gain in dB, or "auto" for AGC
    #[arg(short, long, default_value = "auto")]
    pub gain: String,

    /// Volume 0-100 (default: 80)
    #[arg(long, default_value = "80")]
    pub volume: u8,

    /// Squelch threshold in dBFS (e.g., -30). Disabled if not set.
    #[arg(long)]
    pub squelch: Option<f64>,

    /// Audio output rate in Hz
    #[arg(long, default_value = "48000")]
    pub audio_rate: u32,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum ListenMode {
    Am,
    Fm,
    Wfm,
    Usb,
    Lsb,
    Cw,
}

impl ListenMode {
    fn as_str(&self) -> &'static str {
        match self {
            ListenMode::Am => "am",
            ListenMode::Fm => "fm",
            ListenMode::Wfm => "wfm",
            ListenMode::Usb => "usb",
            ListenMode::Lsb => "lsb",
            ListenMode::Cw => "cw",
        }
    }
}

pub async fn run(args: ListenArgs, device_index: u32) -> Result<()> {
    // Resolve frequency: try parsing as number first, then as bookmark name
    let (frequency, bookmark_mode) = match wavecore::util::parse_frequency(&args.frequency) {
        Ok(freq) => (freq, None),
        Err(_) => {
            let store = BookmarkStore::load();
            if let Some(bm) = store.find(&args.frequency) {
                let mode = bm.mode.clone();
                (bm.frequency_hz, mode)
            } else {
                return Err(anyhow::anyhow!(
                    "\"{}\" is not a valid frequency or bookmark name",
                    args.frequency
                ));
            }
        }
    };

    let db = FrequencyDb::auto_detect();

    let (mode_string, band_name) = if let Some(mode) = args.mode {
        (mode.as_str().to_string(), "Manual".to_string())
    } else if let Some(ref bm_mode) = bookmark_mode {
        (bm_mode.clone(), format!("Bookmark: {}", args.frequency))
    } else if let Some(band) = db.lookup(frequency) {
        (band.modulation.to_string(), band.label.to_string())
    } else {
        ("fm".to_string(), "Unknown Band".to_string())
    };
    let mode_str = mode_string.as_str();

    let defaults = mode_defaults(mode_str)
        .ok_or_else(|| anyhow::anyhow!("Unknown demod mode: {mode_str}"))?;

    let sample_rate = defaults.sample_rate;
    let gain_mode =
        wavecore::util::parse_gain(&args.gain).map_err(|e| anyhow::anyhow!("{e}"))?;

    let volume = (args.volume.min(100) as f32) / 100.0;

    let config = SessionConfig {
        schema_version: 1,
        device_index,
        frequency: frequency,
        sample_rate,
        gain: gain_mode,
        ppm: 0,
        fft_size: 1024,
        pfa: 1e-4,
    };

    let registry = DecoderRegistry::new();
    let (session, events) =
        SessionManager::new(config, registry).map_err(|e| anyhow::anyhow!("{e}"))?;

    // Start demod
    let demod_config = DemodConfig {
        mode: mode_str.to_string(),
        audio_rate: args.audio_rate,
        bandwidth: None,
        bfo: None,
        squelch: args.squelch,
        deemph_us: None,
        output_wav: None,
    };

    session
        .send(Command::StartDemod(demod_config))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    session
        .send(Command::SetVolume(volume))
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let freq_str = wavecore::util::format_freq(frequency);
    println!(
        "Listening on {freq_str} | {band_name} | Mode: {} | Volume: {}%",
        mode_str.to_uppercase(),
        args.volume.min(100),
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
        cmd_tx.send(Command::StopDemod).ok();
        cmd_tx.send(Command::Shutdown).ok();
        stop_tx.send(()).ok();
    });

    let start = Instant::now();

    // Event loop
    loop {
        select! {
            recv(stop_rx) -> _ => break,
            recv(events) -> event => {
                match event {
                    Ok(Event::SpectrumReady(frame)) => {
                        let elapsed = start.elapsed().as_secs_f64();
                        let rms = frame.rms_dbfs;
                        let snr = frame.snr_db;
                        let agc = frame.agc_gain_db;

                        // Simple signal meter
                        let bars = ((rms + 50.0) / 3.0).clamp(0.0, 16.0) as usize;
                        let meter: String = "█".repeat(bars) + &"░".repeat(16 - bars);

                        print!(
                            "\r  {elapsed:5.1}s  [{meter}]  {rms:+.1} dBFS  SNR {snr:.1} dB  AGC {agc:+.1} dB ",
                        );
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                    Ok(Event::Error(e)) => {
                        eprintln!("\nError: {e}");
                    }
                    Err(_) => break,
                    _ => {}
                }
            }
        }
    }

    println!();
    session.shutdown();

    let elapsed = start.elapsed().as_secs_f64();
    println!("Listened for {elapsed:.1}s.");
    Ok(())
}
