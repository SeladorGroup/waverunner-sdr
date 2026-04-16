use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::Receiver;

use wavecore::analysis;
use wavecore::captures::{CaptureCatalog, CaptureSource, default_capture_path};
use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::dsp::decoders;
use wavecore::frequency_db::FrequencyDb;
use wavecore::hardware::GainMode;
use wavecore::recording::RecordingMetadata;
use wavecore::session::manager::SessionManager;
use wavecore::session::replay::ReplayDevice;
use wavecore::session::{Command, Event, RecordFormat, SessionConfig, StatusUpdate};
use wavecore::signal_identify::{self, DecoderTrialResult, IdentifyResult};
use wavecore::util::utc_timestamp_now;

use super::parse_frequency;

#[derive(clap::Args)]
pub struct IdentifyArgs {
    /// Frequency to identify (supports suffixes: k, M, G)
    #[arg(value_parser = parse_frequency)]
    pub frequency: f64,

    /// Gain in dB, or "auto" for AGC
    #[arg(short, long, default_value = "auto")]
    pub gain: String,

    /// Decoder trial duration in seconds (0 to skip)
    #[arg(long, default_value = "5")]
    pub trial_secs: u64,

    /// Capture a short IQ sample for deeper investigation
    #[arg(long, default_value = "0")]
    pub capture_secs: u64,

    /// Sample rate used for live capture investigation (defaults from band/decoder when omitted)
    #[arg(long, value_parser = parse_frequency)]
    pub capture_rate: Option<f64>,

    /// Optional output path for the investigation capture
    #[arg(long)]
    pub capture_output: Option<PathBuf>,

    /// Write the final identify report to a JSON file
    #[arg(long)]
    pub report: Option<PathBuf>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: IdentifyArgs, device_index: u32) -> Result<()> {
    let db = FrequencyDb::auto_detect();

    // Stage 1 & 2: instant identification
    let mut result = signal_identify::identify_instant(args.frequency, &db);

    if !args.json {
        println!("Identifying signal at {:.6} MHz...\n", args.frequency / 1e6);
    }

    // Print instant results
    if !args.json {
        print_instant_result(&result);
    }

    // Stage 3: decoder trial (if requested and there are candidates)
    if args.trial_secs > 0 {
        let candidates = gather_trial_candidates(&result);
        if !candidates.is_empty() {
            if !args.json {
                println!("\nRunning decoder trial ({} seconds)...", args.trial_secs);
            }

            match run_decoder_trial(
                args.frequency,
                &args.gain,
                device_index,
                &candidates,
                args.trial_secs,
            ) {
                Ok(trials) => {
                    signal_identify::add_trial_results(&mut result, trials);

                    if !args.json {
                        print_trial_results(&result);
                    }
                }
                Err(e) => {
                    if !args.json {
                        eprintln!("  Decoder trial skipped (no hardware): {e}");
                    }
                }
            }
        } else if !args.json {
            println!("\nNo decoder candidates for trial.");
        }
    }

    if args.capture_secs > 0 {
        let capture_rate = investigation_capture_rate(
            args.capture_rate,
            args.frequency,
            result.recommended_decoder.as_deref(),
            &db,
        );
        if !args.json {
            println!(
                "\nCapturing {:.1}s for investigation at {:.3} MS/s...",
                args.capture_secs as f64,
                capture_rate / 1e6,
            );
        }

        let capture_path = args.capture_output.clone().unwrap_or(
            default_capture_path("raw", Some("identify")).map_err(|e| {
                anyhow::anyhow!("Failed to determine default identify capture path: {e}")
            })?,
        );
        let investigation = run_capture_investigation(
            args.frequency,
            &args.gain,
            device_index,
            capture_rate,
            args.capture_secs,
            &capture_path,
        )?;
        signal_identify::add_investigation(&mut result, investigation);

        if !args.json {
            print_investigation_result(&result);
        }
    }

    if let Some(path) = args.report.as_ref() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(&result)?)?;
        if !args.json {
            println!("Report written to {}", path.display());
        }
    }

    if args.json {
        let json = serde_json::to_string_pretty(&result)?;
        println!("{json}");
    } else {
        println!("\n{}", "=".repeat(40));
        println!("Final confidence: {:.0}%", result.confidence * 100.0);
        if let Some(ref mode) = result.recommended_mode {
            println!("Use: waverunner listen {} --mode {mode}", args.frequency);
        }
        if let Some(ref decoder) = result.recommended_decoder {
            println!("Use: {}", decoder_command_example(decoder, args.frequency));
        }
    }

    Ok(())
}

fn print_instant_result(result: &IdentifyResult) {
    if let Some(ref band) = result.band_name {
        println!("  Band:       {band}");
    } else {
        println!("  Band:       (unknown)");
    }

    if let Some(ref modulation) = result.modulation_estimate {
        println!("  Modulation: {}", modulation.to_uppercase());
    }

    if let Some(ref cm) = result.classifier_match {
        println!(
            "  Classifier: {} ({:.0}% confidence)",
            cm.name,
            cm.confidence * 100.0,
        );
        if let Some(ref decoder) = cm.decoder {
            println!("  Decoder:    {decoder}");
        }
    }

    if let Some(ref mode) = result.recommended_mode {
        println!("  Mode:       {}", mode.to_uppercase());
    }
}

fn decoder_command_example(decoder: &str, frequency: f64) -> String {
    match decoder {
        "adsb" => format!("waverunner decode adsb -f {frequency}"),
        "rds" => format!("waverunner decode rds -f {frequency}"),
        "pocsag" | "pocsag-1200" => format!("waverunner decode pocsag -f {frequency} --baud 1200"),
        "pocsag-512" => format!("waverunner decode pocsag -f {frequency} --baud 512"),
        "pocsag-2400" => format!("waverunner decode pocsag -f {frequency} --baud 2400"),
        _ => format!("waverunner decode run {decoder} -f {frequency}"),
    }
}

fn print_trial_results(result: &IdentifyResult) {
    for trial in &result.decoder_trials {
        if trial.messages_decoded > 0 {
            println!(
                "  {} decoded {} message(s) in {}ms",
                trial.decoder, trial.messages_decoded, trial.trial_duration_ms,
            );
        } else {
            println!(
                "  {} — no messages in {}ms",
                trial.decoder, trial.trial_duration_ms,
            );
        }
    }
}

fn print_investigation_result(result: &IdentifyResult) {
    if let Some(ref investigation) = result.investigation {
        println!(
            "\nInvestigation capture: {} ({:.1}s)",
            investigation.capture_path, investigation.capture_duration_secs
        );
        if let Some(ref measurement) = investigation.measurement {
            println!(
                "  Occupied BW: {:.1} kHz | PAPR: {:.1} dB",
                measurement.occupied_bw_hz / 1e3,
                measurement.papr_db,
            );
        }
        if let Some(ref burst) = investigation.burst {
            println!(
                "  Bursts: {} | Mean width: {:.1} us",
                burst.burst_count, burst.mean_pulse_width_us,
            );
        }
        if let Some(ref modulation) = investigation.modulation {
            println!(
                "  Modulation: {} ({:.0}% confidence)",
                modulation.modulation_type,
                modulation.confidence * 100.0,
            );
        }
    }
}

/// Pick which decoders to trial based on the instant identification.
fn gather_trial_candidates(result: &IdentifyResult) -> Vec<String> {
    let mut candidates = Vec::new();

    // Add the recommended decoder
    if let Some(ref decoder) = result.recommended_decoder {
        // Skip rtl433 variants (external subprocess)
        if !decoder.starts_with("rtl433") {
            candidates.push(decoder.clone());
        }
    }

    // Add classifier decoder if different
    if let Some(ref cm) = result.classifier_match {
        if let Some(ref decoder) = cm.decoder {
            if !decoder.starts_with("rtl433") && !candidates.contains(decoder) {
                candidates.push(decoder.clone());
            }
        }
    }

    candidates
}

/// Run a live decoder trial: tune to freq, enable decoders, count messages.
fn run_decoder_trial(
    frequency: f64,
    gain: &str,
    device_index: u32,
    candidates: &[String],
    trial_secs: u64,
) -> Result<Vec<DecoderTrialResult>> {
    let gain_mode = wavecore::util::parse_gain(gain).map_err(|e| anyhow::anyhow!("{e}"))?;
    let sample_rate = decoder_trial_sample_rate(candidates);

    let config = SessionConfig {
        schema_version: 1,
        device_index,
        frequency,
        sample_rate,
        gain: gain_mode,
        ppm: 0,
        fft_size: 1024,
        pfa: 1e-4,
    };

    let mut registry = DecoderRegistry::new();
    decoders::register_all(&mut registry);
    let (session, events) =
        SessionManager::new(config, registry).map_err(|e| anyhow::anyhow!("{e}"))?;

    // Enable candidate decoders
    for decoder in candidates {
        session
            .send(Command::EnableDecoder(decoder.clone()))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
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

    // Count decoded messages per decoder
    let mut counts: std::collections::HashMap<String, usize> =
        candidates.iter().map(|d| (d.clone(), 0)).collect();

    let start = Instant::now();
    let deadline = Duration::from_secs(trial_secs);

    while start.elapsed() < deadline && running.load(Ordering::SeqCst) {
        let remaining = deadline.saturating_sub(start.elapsed());
        match events.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(Event::DecodedMessage(msg)) => {
                if let Some(count) = counts.get_mut(&msg.decoder) {
                    *count += 1;
                }
            }
            Ok(Event::Error(e)) => {
                eprintln!("  Trial error: {e}");
            }
            _ => {}
        }
    }

    // Disable decoders
    for decoder in candidates {
        session.send(Command::DisableDecoder(decoder.clone())).ok();
    }

    session.shutdown();

    let elapsed_ms = start.elapsed().as_millis() as u64;
    let trials = candidates
        .iter()
        .map(|d| DecoderTrialResult {
            decoder: d.clone(),
            messages_decoded: counts.get(d).copied().unwrap_or(0),
            trial_duration_ms: elapsed_ms,
        })
        .collect();

    Ok(trials)
}

fn decoder_trial_sample_rate(candidates: &[String]) -> f64 {
    candidates
        .iter()
        .filter_map(|decoder| decoder_capture_rate(decoder))
        .fold(250_000.0, f64::max)
}

fn decoder_capture_rate(decoder: &str) -> Option<f64> {
    match decoder {
        "adsb" => Some(wavecore::dsp::decoders::adsb::ADSB_SAMPLE_RATE_HZ),
        "rds" => Some(wavecore::dsp::decoders::rds::RDS_CAPTURE_SAMPLE_RATE_HZ),
        _ => {
            let mut registry = DecoderRegistry::new();
            decoders::register_all(&mut registry);
            let plugin = registry.create(decoder)?;
            Some(plugin.requirements().sample_rate.max(250_000.0))
        }
    }
}

fn investigation_capture_rate(
    requested_rate: Option<f64>,
    frequency: f64,
    decoder: Option<&str>,
    db: &FrequencyDb,
) -> f64 {
    requested_rate
        .or_else(|| decoder.and_then(decoder_capture_rate))
        .or_else(|| db.sample_rate_hint(frequency))
        .unwrap_or(2_048_000.0)
}

fn run_capture_investigation(
    frequency: f64,
    gain: &str,
    device_index: u32,
    sample_rate: f64,
    capture_secs: u64,
    capture_path: &Path,
) -> Result<wavecore::signal_identify::SignalInvestigation> {
    let gain_mode = wavecore::util::parse_gain(gain).map_err(|e| anyhow::anyhow!("{e}"))?;
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

    let registry = DecoderRegistry::new();
    let (session, events) =
        SessionManager::new(config, registry).map_err(|e| anyhow::anyhow!("{e}"))?;

    session
        .send(Command::StartRecord {
            path: capture_path.to_path_buf(),
            format: RecordFormat::RawCf32,
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let cmd_tx = session.cmd_sender();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(capture_secs));
        cmd_tx.send(Command::StopRecord).ok();
    });

    let start = Instant::now();
    let mut samples_written = 0u64;
    let deadline = Instant::now() + Duration::from_secs(capture_secs + 10);
    loop {
        if Instant::now() > deadline {
            anyhow::bail!("Timed out while capturing investigation sample");
        }

        match events.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::Status(StatusUpdate::RecordingStopped(samples))) => {
                samples_written = samples;
                break;
            }
            Ok(Event::Error(err)) => anyhow::bail!(err),
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !session.is_running() {
                    anyhow::bail!("Investigation capture stopped before recording completed");
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    session.shutdown();

    let elapsed = start.elapsed().as_secs_f64();
    if samples_written == 0 {
        anyhow::bail!("Investigation capture completed without any samples");
    }
    let metadata = RecordingMetadata {
        schema_version: 1,
        center_freq: frequency,
        sample_rate,
        gain: gain.to_string(),
        format: "cf32".to_string(),
        timestamp: utc_timestamp_now(),
        duration_secs: Some(elapsed),
        device: "rtlsdr".to_string(),
        samples_written,
        label: Some("identify".to_string()),
        notes: Some("Auto-captured by identify".to_string()),
        tags: vec!["identify".to_string()],
        demod_mode: None,
        decoder: None,
        timeline_path: None,
        report_path: None,
    };
    metadata.write_sidecar(capture_path)?;

    let mut catalog = CaptureCatalog::load();
    catalog.register(capture_path, &metadata, CaptureSource::LiveRecord);
    let _ = catalog.save();

    run_replay_investigation(capture_path, sample_rate, frequency, elapsed)
}

fn run_replay_investigation(
    capture_path: &Path,
    sample_rate: f64,
    frequency: f64,
    capture_duration_secs: f64,
) -> Result<wavecore::signal_identify::SignalInvestigation> {
    let device = ReplayDevice::open(capture_path, sample_rate)
        .map_err(|e| anyhow::anyhow!("Failed to open investigation capture: {e}"))?;
    let config = SessionConfig {
        schema_version: 1,
        device_index: 0,
        frequency,
        sample_rate,
        gain: GainMode::Auto,
        ppm: 0,
        fft_size: 2048,
        pfa: 1e-4,
    };

    let mut registry = DecoderRegistry::new();
    decoders::register_all(&mut registry);
    let (session, event_rx) = SessionManager::new_with_device(config, device, registry)
        .map_err(|e| anyhow::anyhow!("Failed to replay investigation capture: {e}"))?;

    warm_up_replay(&session, &event_rx)?;

    let measurement = match run_analysis_request(
        &session,
        &event_rx,
        1,
        analysis::AnalysisRequest::MeasureSignal(analysis::measurement::MeasureConfig {
            signal_center_bin: 1024,
            signal_width_bins: 100,
            adjacent_width_bins: 100,
            obw_threshold_db: 26.0,
        }),
    )? {
        analysis::AnalysisResult::Measurement(report) => Some(report),
        _ => None,
    };

    let burst = match run_analysis_request(
        &session,
        &event_rx,
        2,
        analysis::AnalysisRequest::AnalyzeBurst(analysis::burst::BurstConfig {
            threshold_db: 10.0,
            min_burst_samples: 10,
            sample_rate,
        }),
    )? {
        analysis::AnalysisResult::Burst(report) => Some(report),
        _ => None,
    };

    let modulation = match run_analysis_request(
        &session,
        &event_rx,
        3,
        analysis::AnalysisRequest::EstimateModulation(analysis::modulation::ModulationConfig {
            sample_rate,
            fft_size: 2048,
        }),
    )? {
        analysis::AnalysisResult::Modulation(report) => Some(report),
        _ => None,
    };

    session.shutdown();

    Ok(wavecore::signal_identify::SignalInvestigation {
        capture_path: capture_path.display().to_string(),
        capture_duration_secs,
        measurement,
        burst,
        modulation,
    })
}

fn warm_up_replay(session: &SessionManager, event_rx: &Receiver<Event>) -> Result<()> {
    let mut blocks_seen = 0_u32;
    let deadline = Instant::now() + Duration::from_secs(10);

    while blocks_seen < 5 && session.is_running() {
        if Instant::now() > deadline {
            anyhow::bail!("Timed out while warming up replay analysis");
        }

        match event_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::SpectrumReady(_)) => blocks_seen += 1,
            Ok(Event::Error(err)) => anyhow::bail!(err),
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(())
}

fn run_analysis_request(
    session: &SessionManager,
    event_rx: &Receiver<Event>,
    id: u64,
    request: analysis::AnalysisRequest,
) -> Result<analysis::AnalysisResult> {
    session
        .send(Command::RunAnalysis { id, request })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if Instant::now() > deadline {
            anyhow::bail!("Timed out waiting for analysis result");
        }

        match event_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::AnalysisResult {
                id: seen_id,
                result,
            }) if seen_id == id => return Ok(result),
            Ok(Event::Error(err)) => anyhow::bail!(err),
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("Replay analysis session ended unexpectedly");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavecore::frequency_db::Region;

    #[test]
    fn decoder_command_examples_match_decoder_shape() {
        assert_eq!(
            decoder_command_example("adsb", 1_090_000_000.0),
            "waverunner decode adsb -f 1090000000"
        );
        assert_eq!(
            decoder_command_example("rds", 94_900_000.0),
            "waverunner decode rds -f 94900000"
        );
        assert_eq!(
            decoder_command_example("pocsag-512", 929_612_500.0),
            "waverunner decode pocsag -f 929612500 --baud 512"
        );
        assert_eq!(
            decoder_command_example("ais-a", 161_975_000.0),
            "waverunner decode run ais-a -f 161975000"
        );
    }

    #[test]
    fn investigation_rate_prefers_explicit_value() {
        let db = FrequencyDb::new(Region::NA);
        assert_eq!(
            investigation_capture_rate(Some(1_800_000.0), 1_090_000_000.0, Some("adsb"), &db),
            1_800_000.0
        );
    }

    #[test]
    fn investigation_rate_uses_decoder_before_band_hint() {
        let db = FrequencyDb::new(Region::NA);
        assert_eq!(
            investigation_capture_rate(None, 1_090_000_000.0, Some("adsb"), &db),
            wavecore::dsp::decoders::adsb::ADSB_SAMPLE_RATE_HZ
        );
    }

    #[test]
    fn investigation_rate_falls_back_to_band_hint() {
        let db = FrequencyDb::new(Region::NA);
        assert_eq!(
            investigation_capture_rate(None, 94_900_000.0, None, &db),
            1_024_000.0
        );
    }

    #[test]
    fn gather_trial_candidates_skips_rtl433_and_deduplicates() {
        let result = IdentifyResult {
            frequency_hz: 433_920_000.0,
            band_name: Some("70 cm ISM".to_string()),
            modulation_estimate: Some("ook".to_string()),
            recommended_decoder: Some("rtl433".to_string()),
            classifier_match: Some(signal_identify::ClassifierMatch {
                name: "Pager".to_string(),
                confidence: 0.9,
                modulation: Some("fsk".to_string()),
                decoder: Some("pocsag".to_string()),
            }),
            confidence: 0.9,
            recommended_mode: None,
            decoder_trials: Vec::new(),
            investigation: None,
        };

        assert_eq!(gather_trial_candidates(&result), vec!["pocsag".to_string()]);
    }
}
