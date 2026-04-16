use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;

use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::dsp::decoders;
use wavecore::frequency_db::FrequencyDb;
use wavecore::session::manager::SessionManager;
use wavecore::session::{Command, Event, SessionConfig};
use wavecore::signal_identify::{self, DecoderTrialResult, IdentifyResult};

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

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: IdentifyArgs, device_index: u32) -> Result<()> {
    let db = FrequencyDb::auto_detect();

    // Stage 1 & 2: instant identification
    let mut result = signal_identify::identify_instant(args.frequency, &db);

    println!("Identifying signal at {:.6} MHz...\n", args.frequency / 1e6);

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
            println!("Use: waverunner decode {decoder} -f {}", args.frequency,);
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

    let config = SessionConfig {
        schema_version: 1,
        device_index,
        frequency,
        sample_rate: 2_048_000.0,
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
