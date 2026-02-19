use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::analysis::report::{ScanDetection, ScanReport, export_scan_report};
use wavecore::dsp::detection::{
    CfarConfig, CfarMethod, cfar_detect, db_to_linear, noise_floor_sigma_clip,
};
use wavecore::dsp::estimation::snr_spectral;
use wavecore::session::manager::SessionManager;
use wavecore::session::{Command, Event, SessionConfig};

use super::parse_frequency;

#[derive(clap::Args)]
pub struct ScanArgs {
    /// Start frequency (supports suffixes: k, M, G)
    #[arg(value_parser = parse_frequency)]
    pub start: f64,

    /// End frequency
    #[arg(value_parser = parse_frequency)]
    pub end: f64,

    /// Step size (default: sample rate / 2)
    #[arg(short = 'S', long, value_parser = parse_frequency)]
    pub step: Option<f64>,

    /// Sample rate in S/s
    #[arg(short = 'r', long, default_value = "2048000", value_parser = parse_frequency)]
    pub sample_rate: f64,

    /// Dwell time per step in milliseconds
    #[arg(short = 'D', long, default_value = "100")]
    pub dwell_ms: u64,

    /// CFAR false alarm probability (lower = fewer false detections)
    #[arg(long, default_value = "1e-5")]
    pub pfa: f64,

    /// Gain in dB, or "auto" for AGC
    #[arg(short, long, default_value = "auto")]
    pub gain: String,

    /// FFT size
    #[arg(long, default_value = "2048")]
    pub fft_size: usize,

    /// Minimum SNR to report a signal (dB)
    #[arg(long, default_value = "6")]
    pub min_snr: f32,

    /// CFAR method: ca (cell-averaging), go (greatest-of), os (ordered-statistic)
    #[arg(long, default_value = "ca")]
    pub cfar: String,

    /// Output file for scan results (JSON or CSV)
    #[arg(short, long)]
    pub output: Option<String>,

    /// Output format: json (default) or csv
    #[arg(long, default_value = "json")]
    pub format: String,
}

pub async fn run(args: ScanArgs, device_index: u32) -> Result<()> {
    let step = args.step.unwrap_or(args.sample_rate / 2.0);
    let gain_mode = wavecore::util::parse_gain(&args.gain)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Parse CFAR method
    let cfar_method = match args.cfar.to_lowercase().as_str() {
        "go" | "greatest-of" => CfarMethod::GreatestOf,
        "so" | "smallest-of" => CfarMethod::SmallestOf,
        "os" | "ordered-statistic" => CfarMethod::OrderedStatistic {
            rank: (0.75 * 48.0) as usize,
        },
        _ => CfarMethod::CellAveraging,
    };

    // Design CFAR threshold from P_fa using Neyman-Pearson formulation
    let cfar_config = CfarConfig {
        num_reference: 24,
        num_guard: 4,
        threshold_factor: CfarConfig::from_pfa(args.pfa, &cfar_method, 24),
        method: cfar_method,
    };

    let config = SessionConfig {
        schema_version: 1,
        device_index,
        frequency: args.start,
        sample_rate: args.sample_rate,
        gain: gain_mode,
        ppm: 0,
        fft_size: args.fft_size,
        pfa: args.pfa,
    };

    let registry = DecoderRegistry::new();
    let (session, events) = SessionManager::new(config, registry)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let num_steps = ((args.end - args.start) / step).ceil() as usize + 1;

    println!(
        "Scanning {:.3} MHz - {:.3} MHz ({} steps, {:.0} kHz step, {} ms dwell)",
        args.start / 1e6,
        args.end / 1e6,
        num_steps,
        step / 1e3,
        args.dwell_ms,
    );
    println!(
        "  CFAR: P_fa={:.0e}, method={}, {} ref cells, {} guard cells, alpha={:.2}",
        args.pfa,
        args.cfar.to_uppercase(),
        cfar_config.num_reference * 2,
        cfar_config.num_guard * 2,
        cfar_config.threshold_factor,
    );
    println!("{}", "=".repeat(85));
    println!(
        "{:>14}  {:>10}  {:>8}  {:>10}  {:>8}  Signal",
        "Frequency", "Power dB", "SNR dB", "Floor dB", "BW kHz"
    );
    println!("{}", "-".repeat(85));

    // Ctrl+C handler
    let cmd_tx = session.cmd_sender();
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = Arc::clone(&running);
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        running_clone.store(false, Ordering::SeqCst);
        cmd_tx.send(Command::Shutdown).ok();
    });

    let dwell = Duration::from_millis(args.dwell_ms);
    let settle = Duration::from_millis(10);
    let mut freq = args.start;
    let mut signals_found = 0u32;
    let mut current_step = 0usize;
    let mut scan_detections: Vec<ScanDetection> = Vec::new();

    while freq <= args.end && running.load(Ordering::SeqCst) {
        current_step += 1;

        eprint!(
            "\r  [{}/{}] {:.3} MHz ...",
            current_step, num_steps, freq / 1e6,
        );
        use std::io::Write;
        std::io::stderr().flush().ok();

        // Tune to frequency (skip first step — already set in SessionConfig)
        if current_step > 1 {
            session
                .send(Command::Tune(freq))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }

        // Wait for PLL settle
        std::thread::sleep(settle);

        // Drain stale events from settle period
        while events.try_recv().is_ok() {}

        // Collect spectrum during dwell window
        let deadline = Instant::now() + dwell;
        let mut spectra_linear: Vec<Vec<f32>> = Vec::new();

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match events.recv_timeout(remaining) {
                Ok(Event::SpectrumReady(frame)) => {
                    // Convert to linear for proper power averaging
                    spectra_linear.push(db_to_linear(&frame.spectrum_db));
                }
                Ok(_) => {} // Ignore non-spectrum events
                Err(_) => break,
            }
        }

        if spectra_linear.is_empty() {
            freq += step;
            continue;
        }

        // Average spectra in linear domain (proper power averaging)
        let n = spectra_linear.len();
        let fft_size = spectra_linear[0].len();
        let mut avg_linear = vec![0.0f32; fft_size];
        for spec in &spectra_linear {
            for (i, &v) in spec.iter().enumerate() {
                avg_linear[i] += v / n as f32;
            }
        }

        // Convert averaged power back to dB
        let avg_db: Vec<f32> = avg_linear
            .iter()
            .map(|&v| 10.0 * v.max(1e-30).log10())
            .collect();

        // Noise floor via iterative sigma-clipping
        let noise_floor = noise_floor_sigma_clip(&avg_db, 3, 2.5);

        // CFAR detection on averaged linear-power spectrum
        let detections = cfar_detect(&avg_linear, &cfar_config, args.sample_rate);

        // Report detections that pass minimum SNR filter
        for det in &detections {
            if det.snr_db < args.min_snr {
                continue;
            }

            signals_found += 1;
            let det_freq = freq + det.freq_offset_hz;

            // Estimate bandwidth for collection
            let bw_bins_pre = estimate_signal_bandwidth(&avg_db, det.bin, noise_floor + 3.0);
            let bw_hz_pre = bw_bins_pre as f64 * args.sample_rate / fft_size as f64;

            scan_detections.push(ScanDetection {
                frequency_hz: det_freq,
                power_db: det.power_db,
                snr_db: det.snr_db,
                bandwidth_hz: bw_hz_pre,
            });

            // Bandwidth estimation from contiguous bins above noise floor
            let bw_bins = estimate_signal_bandwidth(&avg_db, det.bin, noise_floor + 3.0);
            let bw_hz = bw_bins as f64 * args.sample_rate / fft_size as f64;

            // Spectral SNR using split-window method for per-signal SNR
            let spectral_snr = snr_spectral(&avg_db, det.bin, bw_bins.max(3), 20);
            let reported_snr = if spectral_snr > 0.0 {
                spectral_snr
            } else {
                det.snr_db
            };

            // Signal strength bar
            let bar_len = (reported_snr as usize / 2).min(30);
            let bar: String = "|".repeat(bar_len);

            // Clear progress line and print detection
            eprint!("\r{}\r", " ".repeat(50));
            println!(
                "{:>11.6} MHz  {:>10.1}  {:>8.1}  {:>10.1}  {:>8.1}  {}",
                det_freq / 1e6,
                det.power_db,
                reported_snr,
                noise_floor,
                bw_hz / 1e3,
                bar,
            );
        }

        freq += step;
    }

    // Clear progress line
    eprint!("\r{}\r", " ".repeat(50));
    println!("{}", "=".repeat(85));
    println!("Scan complete. {} signal(s) found.", signals_found);

    // Export scan results if --output specified
    if let Some(ref output_path) = args.output {
        let report = ScanReport {
            start_freq: args.start,
            end_freq: args.end,
            step_hz: step,
            dwell_ms: args.dwell_ms,
            signals_found,
            detections: scan_detections,
        };
        let path = std::path::Path::new(output_path);
        match export_scan_report(&report, path, &args.format) {
            Ok(p) => println!("Scan results written to {p}"),
            Err(e) => eprintln!("Failed to write scan results: {e}"),
        }
    }

    session.shutdown();
    Ok(())
}

/// Estimate signal bandwidth as number of contiguous bins above a threshold.
///
/// Starting from `center_bin`, expands outward until bins fall below `threshold_db`.
fn estimate_signal_bandwidth(spectrum: &[f32], center_bin: usize, threshold_db: f32) -> usize {
    let n = spectrum.len();
    let mut left = center_bin;
    let mut right = center_bin;

    // Expand left
    while left > 0 && spectrum[left - 1] > threshold_db {
        left -= 1;
    }

    // Expand right
    while right + 1 < n && spectrum[right + 1] > threshold_db {
        right += 1;
    }

    right - left + 1
}
