use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;

use wavecore::analysis::report::{ScanDetection, ScanReport, export_scan_report};
use wavecore::bookmarks::{Bookmark, BookmarkStore};
use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::dsp::detection::{
    CfarConfig, CfarMethod, cfar_detect, db_to_linear, noise_floor_sigma_clip,
};
use wavecore::dsp::estimation::snr_spectral;
use wavecore::frequency_db::FrequencyDb;
use wavecore::mode::profile::{FrequencyEntry, MissionProfile};
use wavecore::session::manager::SessionManager;
use wavecore::session::{Command, Event, SessionConfig};
use wavecore::signal_identify;
use wavecore::util::{format_freq, utc_timestamp_now};

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

    /// Number of full-range passes to run
    #[arg(long, default_value = "1")]
    pub passes: u32,

    /// Merge hits within this many kHz into one logical signal
    #[arg(long, default_value = "25")]
    pub dedupe_khz: f64,

    /// Ignore a frequency range written as START:END (repeatable)
    #[arg(long = "ignore-range")]
    pub ignore_ranges: Vec<String>,

    /// Keep only the strongest N signals in the final report/export
    #[arg(long)]
    pub top: Option<usize>,

    /// Save final detections as bookmarks using this prefix
    #[arg(long)]
    pub bookmark_prefix: Option<String>,

    /// Generate a mission profile TOML from the final detections
    #[arg(long)]
    pub save_profile: Option<String>,

    /// Name used when generating a mission profile
    #[arg(long)]
    pub profile_name: Option<String>,
}

#[derive(Debug, Clone)]
struct AggregatedDetection {
    frequency_hz: f64,
    bandwidth_hz: f64,
    power_db: f32,
    snr_db: f32,
    hits: u32,
    snr_sum: f32,
    first_seen_pass: u32,
    last_seen_pass: u32,
}

pub async fn run(args: ScanArgs, device_index: u32) -> Result<()> {
    let step = args.step.unwrap_or(args.sample_rate / 2.0);
    let dedupe_hz = args.dedupe_khz.max(1.0) * 1_000.0;
    let ignore_ranges = parse_ignore_ranges(&args.ignore_ranges)?;
    let gain_mode = wavecore::util::parse_gain(&args.gain).map_err(|e| anyhow::anyhow!("{e}"))?;
    let freq_db = FrequencyDb::auto_detect();

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
    let (session, events) =
        SessionManager::new(config, registry).map_err(|e| anyhow::anyhow!("{e}"))?;

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
    let mut raw_hits = 0u32;
    let mut aggregated = Vec::<AggregatedDetection>::new();

    for pass_idx in 1..=args.passes.max(1) {
        let mut freq = args.start;
        let mut current_step = 0usize;

        while freq <= args.end && running.load(Ordering::SeqCst) {
            current_step += 1;

            eprint!(
                "\r  [pass {}/{} | {}/{}] {:.3} MHz ...",
                pass_idx,
                args.passes.max(1),
                current_step,
                num_steps,
                freq / 1e6,
            );
            use std::io::Write;
            std::io::stderr().flush().ok();

            if current_step > 1 || pass_idx > 1 {
                session
                    .send(Command::Tune(freq))
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            }

            std::thread::sleep(settle);
            while events.try_recv().is_ok() {}

            let deadline = Instant::now() + dwell;
            let mut spectra_linear: Vec<Vec<f32>> = Vec::new();

            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match events.recv_timeout(remaining) {
                    Ok(Event::SpectrumReady(frame)) => {
                        spectra_linear.push(db_to_linear(&frame.spectrum_db));
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }

            if spectra_linear.is_empty() {
                freq += step;
                continue;
            }

            let n = spectra_linear.len();
            let fft_size = spectra_linear[0].len();
            let mut avg_linear = vec![0.0f32; fft_size];
            for spec in &spectra_linear {
                for (i, &v) in spec.iter().enumerate() {
                    avg_linear[i] += v / n as f32;
                }
            }

            let avg_db: Vec<f32> = avg_linear
                .iter()
                .map(|&v| 10.0 * v.max(1e-30).log10())
                .collect();
            let noise_floor = noise_floor_sigma_clip(&avg_db, 3, 2.5);
            let detections = cfar_detect(&avg_linear, &cfar_config, args.sample_rate);

            for det in &detections {
                if det.snr_db < args.min_snr {
                    continue;
                }

                let det_freq = freq + det.freq_offset_hz;
                if is_ignored(det_freq, &ignore_ranges) {
                    continue;
                }

                raw_hits += 1;

                let bw_bins = estimate_signal_bandwidth(&avg_db, det.bin, noise_floor + 3.0);
                let bw_hz = bw_bins as f64 * args.sample_rate / fft_size as f64;
                let spectral_snr = snr_spectral(&avg_db, det.bin, bw_bins.max(3), 20);
                let reported_snr = if spectral_snr > 0.0 {
                    spectral_snr
                } else {
                    det.snr_db
                };

                merge_detection(
                    &mut aggregated,
                    det_freq,
                    bw_hz,
                    det.power_db,
                    reported_snr,
                    pass_idx,
                    dedupe_hz,
                );

                let bar_len = (reported_snr as usize / 2).min(30);
                let bar: String = "|".repeat(bar_len);

                eprint!("\r{}\r", " ".repeat(60));
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
    }

    let mut scan_detections = finalize_detections(aggregated, &freq_db);
    scan_detections.sort_by(|a, b| {
        b.peak_snr_db
            .partial_cmp(&a.peak_snr_db)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.frequency_hz
                    .partial_cmp(&b.frequency_hz)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    if let Some(top) = args.top {
        scan_detections.truncate(top);
    }

    // Clear progress line
    eprint!("\r{}\r", " ".repeat(50));
    println!("{}", "=".repeat(85));
    println!(
        "Scan complete. {} unique signal(s) found across {} hit(s).",
        scan_detections.len(),
        raw_hits,
    );

    for det in &scan_detections {
        let label = det.label.as_deref().unwrap_or("Unlabeled");
        let decoder = det.suggested_decoder.as_deref().unwrap_or("-");
        println!(
            "  {:>14}  {:>5} hits  peak {:>5.1} dB  {:<18} {}",
            format_freq(det.frequency_hz),
            det.hits,
            det.peak_snr_db,
            label,
            decoder,
        );
    }

    if let Some(prefix) = args.bookmark_prefix.as_deref() {
        let mut store = BookmarkStore::load();
        for (idx, det) in scan_detections.iter().enumerate() {
            let suffix = det
                .label
                .clone()
                .unwrap_or_else(|| format!("{:.3}M", det.frequency_hz / 1e6));
            let name = format!("{prefix} {} {}", idx + 1, suffix);
            store.add(Bookmark {
                name,
                frequency_hz: det.frequency_hz,
                mode: det.suggested_mode.clone(),
                decoder: det.suggested_decoder.clone(),
                notes: det.service.clone(),
            });
        }
        store.save().map_err(|e| anyhow::anyhow!("{e}"))?;
        println!(
            "Saved {} bookmark(s) with prefix \"{prefix}\".",
            scan_detections.len()
        );
    }

    if let Some(path) = args.save_profile.as_deref() {
        let profile_name = args
            .profile_name
            .clone()
            .unwrap_or_else(|| format!("scan-{}-{}", args.start as u64, args.end as u64));
        save_profile(path, &profile_name, &scan_detections)?;
        println!("Profile written to {path}");
    }

    // Export scan results if --output specified
    if let Some(ref output_path) = args.output {
        let report = ScanReport {
            generated_at: utc_timestamp_now(),
            start_freq: args.start,
            end_freq: args.end,
            step_hz: step,
            dwell_ms: args.dwell_ms,
            passes: args.passes.max(1),
            signals_found: scan_detections.len() as u32,
            region: Some(freq_db.region.label().to_string()),
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

fn parse_ignore_ranges(values: &[String]) -> Result<Vec<(f64, f64)>> {
    values
        .iter()
        .map(|value| {
            let (start, end) = value.split_once(':').ok_or_else(|| {
                anyhow::anyhow!("Invalid ignore range '{value}', expected START:END")
            })?;
            let start = parse_frequency(start).map_err(|e| anyhow::anyhow!("{e}"))?;
            let end = parse_frequency(end).map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok((start.min(end), start.max(end)))
        })
        .collect()
}

fn is_ignored(freq_hz: f64, ranges: &[(f64, f64)]) -> bool {
    ranges
        .iter()
        .any(|(start, end)| freq_hz >= *start && freq_hz <= *end)
}

fn merge_detection(
    detections: &mut Vec<AggregatedDetection>,
    frequency_hz: f64,
    bandwidth_hz: f64,
    power_db: f32,
    snr_db: f32,
    pass: u32,
    dedupe_hz: f64,
) {
    if let Some(existing) = detections
        .iter_mut()
        .find(|entry| (entry.frequency_hz - frequency_hz).abs() <= dedupe_hz)
    {
        existing.frequency_hz = (existing.frequency_hz * f64::from(existing.hits) + frequency_hz)
            / f64::from(existing.hits + 1);
        existing.bandwidth_hz = (existing.bandwidth_hz * f64::from(existing.hits) + bandwidth_hz)
            / f64::from(existing.hits + 1);
        existing.power_db = existing.power_db.max(power_db);
        existing.snr_db = existing.snr_db.max(snr_db);
        existing.hits += 1;
        existing.snr_sum += snr_db;
        existing.last_seen_pass = pass;
        return;
    }

    detections.push(AggregatedDetection {
        frequency_hz,
        bandwidth_hz,
        power_db,
        snr_db,
        hits: 1,
        snr_sum: snr_db,
        first_seen_pass: pass,
        last_seen_pass: pass,
    });
}

fn finalize_detections(
    detections: Vec<AggregatedDetection>,
    freq_db: &FrequencyDb,
) -> Vec<ScanDetection> {
    detections
        .into_iter()
        .map(|det| {
            let id = signal_identify::identify_instant(det.frequency_hz, freq_db);
            let service = freq_db
                .service(det.frequency_hz)
                .map(|service| service.to_string());
            ScanDetection {
                frequency_hz: det.frequency_hz,
                power_db: det.power_db,
                snr_db: det.snr_db,
                bandwidth_hz: det.bandwidth_hz,
                hits: det.hits,
                peak_power_db: det.power_db,
                peak_snr_db: det.snr_db,
                avg_snr_db: det.snr_sum / det.hits as f32,
                first_seen_pass: det.first_seen_pass,
                last_seen_pass: det.last_seen_pass,
                label: id
                    .band_name
                    .clone()
                    .or_else(|| id.classifier_match.as_ref().map(|cm| cm.name.clone())),
                service,
                suggested_mode: id.recommended_mode,
                suggested_decoder: id.recommended_decoder,
            }
        })
        .collect()
}

fn save_profile(path: &str, profile_name: &str, detections: &[ScanDetection]) -> Result<()> {
    let profile = MissionProfile {
        schema_version: 1,
        name: profile_name.to_string(),
        description: format!(
            "Generated from a WaveRunner scan on {}",
            utc_timestamp_now()
        ),
        frequencies: detections
            .iter()
            .map(|det| FrequencyEntry {
                freq_hz: det.frequency_hz,
                label: det
                    .label
                    .clone()
                    .unwrap_or_else(|| format_freq(det.frequency_hz)),
                monitor: false,
                dwell_ms: Some(3_000),
                mode: det.suggested_mode.clone(),
                decoder: det.suggested_decoder.clone(),
                priority: det.hits > 1 || det.peak_snr_db >= 20.0,
                locked_out: false,
                notes: det.service.clone(),
            })
            .collect(),
        decoders: Vec::new(),
        sample_rate: suggested_profile_sample_rate(detections),
        gain: Some("auto".to_string()),
        demod: None,
        auto_record: None,
    };

    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    let toml = toml::to_string_pretty(&profile).map_err(|e| anyhow::anyhow!("{e}"))?;
    std::fs::write(path, toml).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

fn suggested_profile_sample_rate(detections: &[ScanDetection]) -> Option<f64> {
    let inferred = detections
        .iter()
        .filter_map(|det| {
            suggested_capture_rate(
                det.suggested_mode.as_deref(),
                det.suggested_decoder.as_deref(),
            )
        })
        .fold(None::<f64>, |best, rate| {
            Some(best.map_or(rate, |current| current.max(rate)))
        });

    inferred.or(Some(2_048_000.0))
}

fn suggested_capture_rate(mode: Option<&str>, decoder: Option<&str>) -> Option<f64> {
    match decoder {
        Some("adsb") => Some(wavecore::dsp::decoders::adsb::ADSB_SAMPLE_RATE_HZ),
        Some("rds") => Some(wavecore::dsp::decoders::rds::RDS_CAPTURE_SAMPLE_RATE_HZ),
        Some(name) => {
            let mut registry = DecoderRegistry::new();
            wavecore::dsp::decoders::register_all(&mut registry);
            registry
                .create(name)
                .map(|decoder| decoder.requirements().sample_rate.max(250_000.0))
        }
        None => match mode {
            Some("wfm") | Some("wfm-stereo") => Some(1_024_000.0),
            Some("am") | Some("fm") | Some("usb") | Some("lsb") | Some("cw") => Some(2_048_000.0),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggested_profile_rate_prefers_highest_decoder_requirement() {
        let detections = vec![
            ScanDetection {
                frequency_hz: 94_900_000.0,
                power_db: -20.0,
                snr_db: 25.0,
                bandwidth_hz: 180_000.0,
                hits: 3,
                peak_power_db: -20.0,
                peak_snr_db: 25.0,
                avg_snr_db: 24.0,
                first_seen_pass: 1,
                last_seen_pass: 2,
                label: Some("FM Broadcast".to_string()),
                service: Some("FM Broadcast".to_string()),
                suggested_mode: Some("wfm".to_string()),
                suggested_decoder: Some("rds".to_string()),
            },
            ScanDetection {
                frequency_hz: 1_090_000_000.0,
                power_db: -18.0,
                snr_db: 18.0,
                bandwidth_hz: 2_000_000.0,
                hits: 1,
                peak_power_db: -18.0,
                peak_snr_db: 18.0,
                avg_snr_db: 18.0,
                first_seen_pass: 1,
                last_seen_pass: 1,
                label: Some("ADS-B".to_string()),
                service: Some("Aviation".to_string()),
                suggested_mode: None,
                suggested_decoder: Some("adsb".to_string()),
            },
        ];

        assert_eq!(
            suggested_profile_sample_rate(&detections),
            Some(wavecore::dsp::decoders::adsb::ADSB_SAMPLE_RATE_HZ)
        );
    }

    #[test]
    fn suggested_profile_rate_falls_back_for_plain_audio_modes() {
        let detections = vec![ScanDetection {
            frequency_hz: 121_500_000.0,
            power_db: -30.0,
            snr_db: 12.0,
            bandwidth_hz: 8_000.0,
            hits: 1,
            peak_power_db: -30.0,
            peak_snr_db: 12.0,
            avg_snr_db: 12.0,
            first_seen_pass: 1,
            last_seen_pass: 1,
            label: Some("Airband".to_string()),
            service: Some("Aviation".to_string()),
            suggested_mode: Some("am".to_string()),
            suggested_decoder: None,
        }];

        assert_eq!(
            suggested_profile_sample_rate(&detections),
            Some(2_048_000.0)
        );
    }
}
