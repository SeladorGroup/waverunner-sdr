use anyhow::Result;
use crossbeam_channel::select;

use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::session::manager::SessionManager;
use wavecore::session::{Command, Event, SessionConfig};
use wavecore::util::format_freq;

use super::parse_frequency;

#[derive(clap::Args)]
pub struct TuneArgs {
    /// Center frequency (supports suffixes: k, M, G)
    #[arg(value_parser = parse_frequency)]
    pub frequency: f64,

    /// Sample rate in S/s (default: 2.048M)
    #[arg(short, long, default_value = "2048000", value_parser = parse_frequency)]
    pub sample_rate: f64,

    /// Gain in dB, or "auto" for AGC
    #[arg(short, long, default_value = "auto")]
    pub gain: String,

    /// FFT size for spectrum analysis
    #[arg(long, default_value = "2048")]
    pub fft_size: usize,

    /// PPM frequency correction
    #[arg(long, default_value = "0")]
    pub ppm: i32,

    /// CFAR false alarm probability (lower = fewer false detections)
    #[arg(long, default_value = "1e-4")]
    pub pfa: f64,
}

pub async fn run(args: TuneArgs, device_index: u32) -> Result<()> {
    let gain_mode = wavecore::util::parse_gain(&args.gain)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let config = SessionConfig {
        device_index,
        frequency: args.frequency,
        sample_rate: args.sample_rate,
        gain: gain_mode,
        ppm: args.ppm,
        fft_size: args.fft_size,
        pfa: args.pfa,
    };

    let registry = DecoderRegistry::new();
    let (session, events) = SessionManager::new(config, registry)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!(
        "Tuned to {:.6} MHz | Rate: {:.3} MS/s | Gain: {} | FFT: {} | P_fa: {:.0e}",
        args.frequency / 1e6,
        args.sample_rate / 1e6,
        args.gain,
        args.fft_size,
        args.pfa,
    );
    println!("{}", "=".repeat(90));
    println!(
        "{:>10}  {:>10}  {:>10}  {:>10}  {:>10}  {:>10}  {:>10}",
        "RMS dBFS", "Floor dB", "SNR dB", "Peak Freq", "Peak dB", "Flatness", "Kurtosis"
    );
    println!("{}", "-".repeat(90));

    // Ctrl+C handler sends Shutdown command
    let cmd_tx = session.cmd_sender();
    let (stop_tx, stop_rx) = crossbeam_channel::bounded::<()>(1);
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        println!("\nStopping...");
        cmd_tx.send(Command::Shutdown).ok();
        stop_tx.send(()).ok();
    });

    let sample_rate = args.sample_rate;

    // Event consumption loop — all DSP happens in SessionManager's processing thread
    loop {
        select! {
            recv(stop_rx) -> _ => break,
            recv(events) -> event => {
                match event {
                    Ok(Event::SpectrumReady(frame)) => {
                        // Find strongest peak in spectrum
                        let spectrum = &frame.spectrum_db;
                        let peak_bin = spectrum
                            .iter()
                            .enumerate()
                            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        let peak_offset = (peak_bin as f64 - spectrum.len() as f64 / 2.0)
                            * sample_rate / spectrum.len() as f64;
                        let peak_freq_abs = args.frequency + peak_offset;
                        let peak_db = spectrum[peak_bin];

                        print!(
                            "\r{:>10.1}  {:>10.1}  {:>10.1}  {:>10}  {:>10.1}  {:>10.4}  {:>10.2}",
                            frame.rms_dbfs,
                            frame.noise_floor_db,
                            frame.snr_db,
                            format_freq(peak_freq_abs),
                            peak_db,
                            frame.spectral_flatness,
                            frame.signal_stats.excess_kurtosis,
                        );
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                    Ok(Event::Detections(detections)) => {
                        if !detections.is_empty() {
                            println!();
                            println!(
                                "  {} CFAR detection(s):",
                                detections.len()
                            );
                            for det in &detections {
                                let freq_abs = args.frequency + det.freq_offset_hz;
                                println!(
                                    "    {} : {:.1} dB (SNR {:.1} dB, floor {:.1} dB)",
                                    format_freq(freq_abs),
                                    det.power_db,
                                    det.snr_db,
                                    det.noise_floor_db,
                                );
                            }
                        }
                    }
                    Ok(Event::Error(e)) => {
                        eprintln!("\nError: {e}");
                    }
                    Err(_) => break, // Channel closed
                    _ => {}
                }
            }
        }
    }

    session.shutdown();
    println!("Done.");
    Ok(())
}
