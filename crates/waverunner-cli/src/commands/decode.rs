use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use crossbeam_channel::select;

use wavecore::buffer::{PipelineConfig, sample_pipeline};
use wavecore::dsp::decoder::{DecoderHandle, DecoderRegistry};
use wavecore::dsp::decoders;
use wavecore::dsp::preprocess::DcRemover;
use wavecore::hardware::rtlsdr::RtlSdrDevice;
use wavecore::hardware::DeviceEnumerator;
use wavecore::session::DecodedMessage;
use wavecore::types::{Sample, SampleBlock};
use wavecore::util::format_freq;

use super::parse_frequency;

// ============================================================================
// CLI argument types
// ============================================================================

#[derive(clap::Args)]
pub struct DecodeArgs {
    #[command(subcommand)]
    pub protocol: DecodeProtocol,

    /// Gain in dB, or "auto" for AGC
    #[arg(short, long, default_value = "auto", global = true)]
    pub gain: String,

    /// PPM frequency correction
    #[arg(long, default_value = "0", global = true)]
    pub ppm: i32,

    /// Sample rate override in S/s
    #[arg(short, long, value_parser = parse_frequency, global = true)]
    pub sample_rate: Option<f64>,

    /// Output format: "text" (default) or "json"
    #[arg(short = 'F', long, default_value = "text", global = true)]
    pub format: OutputFormat,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(clap::Subcommand)]
pub enum DecodeProtocol {
    /// Decode POCSAG pager messages
    Pocsag(PocsagArgs),
    /// Decode ADS-B aircraft transponder messages
    Adsb(AdsbArgs),
    /// Decode RDS/RBDS radio data from FM broadcasts
    Rds(RdsArgs),
}

#[derive(clap::Args)]
pub struct PocsagArgs {
    /// Center frequency (supports suffixes: k, M, G). Default: 929.6125 MHz
    #[arg(short, long, value_parser = parse_frequency, default_value = "929.6125M")]
    pub frequency: f64,

    /// Baud rate: 512, 1200, or 2400
    #[arg(short, long, default_value = "1200")]
    pub baud: u32,
}

#[derive(clap::Args)]
pub struct AdsbArgs {
    /// Center frequency (default: 1090 MHz)
    #[arg(short, long, value_parser = parse_frequency, default_value = "1090M")]
    pub frequency: f64,
}

#[derive(clap::Args)]
pub struct RdsArgs {
    /// FM station frequency (supports suffixes: k, M, G)
    #[arg(short, long, value_parser = parse_frequency)]
    pub frequency: f64,
}

// ============================================================================
// Protocol defaults
// ============================================================================

/// Sample rate and decoder name for each protocol.
struct ProtocolConfig {
    decoder_name: &'static str,
    default_sample_rate: f64,
    frequency: f64,
    description: &'static str,
}

fn protocol_config(protocol: &DecodeProtocol) -> ProtocolConfig {
    match protocol {
        DecodeProtocol::Pocsag(args) => {
            let name = match args.baud {
                512 => "pocsag-512",
                2400 => "pocsag-2400",
                _ => "pocsag-1200",
            };
            ProtocolConfig {
                decoder_name: name,
                // POCSAG needs enough bandwidth for ±4.5 kHz FSK.
                // 22050 Hz gives ~5× oversampling at 1200 baud.
                default_sample_rate: 2_048_000.0,
                frequency: args.frequency,
                description: "POCSAG pager",
            }
        }
        DecodeProtocol::Adsb(args) => ProtocolConfig {
            decoder_name: "adsb",
            // ADS-B PPM at 1 Mbps needs ≥2 MS/s for Nyquist
            default_sample_rate: 2_000_000.0,
            frequency: args.frequency,
            description: "ADS-B 1090 MHz",
        },
        DecodeProtocol::Rds(args) => ProtocolConfig {
            decoder_name: "rds",
            // RDS 57 kHz subcarrier needs ≥114 kHz sample rate.
            // 228 kHz is 4× oversampling of the subcarrier.
            default_sample_rate: 228_000.0,
            frequency: args.frequency,
            description: "RDS/RBDS",
        },
    }
}

// ============================================================================
// Main entry point
// ============================================================================

pub async fn run(args: DecodeArgs, device_index: u32) -> Result<()> {
    let pcfg = protocol_config(&args.protocol);
    let sample_rate = args.sample_rate.unwrap_or(pcfg.default_sample_rate);
    let gain_mode = wavecore::util::parse_gain(&args.gain)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Open hardware
    let device = Arc::new(
        RtlSdrDevice::open(device_index).context("Failed to open SDR device")?,
    );
    device
        .set_frequency(pcfg.frequency)
        .context("Failed to set frequency")?;
    device
        .set_sample_rate(sample_rate)
        .context("Failed to set sample rate")?;
    device
        .set_gain(gain_mode)
        .context("Failed to set gain")?;
    if args.ppm != 0 {
        device.set_ppm(args.ppm).context("Failed to set PPM")?;
    }

    // Build decoder registry and instantiate our decoder
    let mut registry = DecoderRegistry::new();
    decoders::register_all(&mut registry);

    let decoder = registry
        .create(pcfg.decoder_name)
        .ok_or_else(|| anyhow::anyhow!("Unknown decoder: {}", pcfg.decoder_name))?;

    // Decoded message channel
    let (decoded_tx, decoded_rx) = crossbeam_channel::unbounded::<DecodedMessage>();

    // Spawn decoder in its own thread with bounded sample channel
    // Ring buffer capacity 64 blocks — decoder drops oldest if behind
    let decoder_handle = DecoderHandle::spawn(decoder, decoded_tx, 64);

    // DC removal for ADC offset
    let mut dc_remover = DcRemover::from_cutoff(100.0, sample_rate);

    // Print banner
    println!(
        "{} decoder @ {} | Rate: {:.3} MS/s | Gain: {} | Baud: {}",
        pcfg.description,
        format_freq(pcfg.frequency),
        sample_rate / 1e6,
        args.gain,
        match &args.protocol {
            DecodeProtocol::Pocsag(a) => format!("{}", a.baud),
            _ => "N/A".to_string(),
        },
    );
    println!("Press Ctrl+C to stop.\n");

    // Pipeline
    let (producer, consumer) = sample_pipeline(PipelineConfig::default());

    // Hardware reader thread
    let device_clone = Arc::clone(&device);
    let reader_handle = std::thread::spawn(move || {
        let mut sequence = 0u64;
        let start = Instant::now();
        let _ = device_clone.start_rx(Box::new(move |samples: &[Sample]| {
            let block = SampleBlock {
                samples: samples.to_vec(),
                center_freq: 0.0,
                sample_rate: 0.0,
                sequence,
                timestamp_ns: start.elapsed().as_nanos() as u64,
            };
            let _ = producer.send(block);
            sequence += 1;
        }));
    });

    // Ctrl+C handler
    let device_for_stop = Arc::clone(&device);
    let (stop_tx, stop_rx) = crossbeam_channel::bounded::<()>(1);
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        eprintln!("\nStopping...");
        device_for_stop.stop_rx().ok();
        stop_tx.send(()).ok();
    });

    let mut msg_count = 0u64;
    let mut block_count = 0u64;
    let start_time = Instant::now();

    // Main processing loop — minimal: DC removal then feed decoder
    loop {
        select! {
            recv(stop_rx) -> _ => break,
            default => {
                // Drain decoded messages
                while let Ok(msg) = decoded_rx.try_recv() {
                    msg_count += 1;
                    print_message(&msg, msg_count, &args.format);
                }

                // Process next sample block
                if let Some(block) = consumer.try_recv() {
                    let mut samples = block.samples;
                    dc_remover.process(&mut samples);
                    decoder_handle.feed(samples);
                    block_count += 1;

                    // Periodic status on stderr
                    if block_count % 50 == 0 {
                        let elapsed = start_time.elapsed().as_secs_f64();
                        eprint!(
                            "\r  [{:.0}s] blocks: {} | messages: {} ",
                            elapsed, block_count, msg_count,
                        );
                        std::io::stderr().flush().ok();
                    }
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            }
        }
    }

    // Drain remaining messages
    while let Ok(msg) = decoded_rx.try_recv() {
        msg_count += 1;
        print_message(&msg, msg_count, &args.format);
    }

    // Cleanup
    decoder_handle.stop();
    reader_handle.join().ok();

    let elapsed = start_time.elapsed().as_secs_f64();
    eprintln!(
        "\nDone. {} messages decoded in {:.1}s ({} blocks processed).",
        msg_count, elapsed, block_count,
    );
    Ok(())
}

// ============================================================================
// Output formatting
// ============================================================================

fn print_message(msg: &DecodedMessage, seq: u64, format: &OutputFormat) {
    match format {
        OutputFormat::Text => print_message_text(msg, seq),
        OutputFormat::Json => print_message_json(msg, seq),
    }
}

fn print_message_text(msg: &DecodedMessage, seq: u64) {
    let elapsed = msg.timestamp.elapsed();
    // Compute a wall-clock-relative age string
    let age = if elapsed.as_secs() < 1 {
        "now".to_string()
    } else {
        format!("{:.0}s ago", elapsed.as_secs_f64())
    };

    println!();
    println!(
        "━━━ #{} [{}] {} ━━━",
        seq, msg.decoder, age,
    );
    println!("  {}", msg.summary);

    // Print structured fields in key-value pairs
    if !msg.fields.is_empty() {
        for (key, value) in &msg.fields {
            println!("  {:>16}: {}", key, value);
        }
    }

    // Print raw bits if present (hex dump, first 32 bytes)
    if let Some(ref bits) = msg.raw_bits {
        let hex: String = bits
            .iter()
            .take(32)
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(" ");
        let suffix = if bits.len() > 32 { "..." } else { "" };
        println!("  {:>16}: {}{}", "raw", hex, suffix);
    }
}

fn print_message_json(msg: &DecodedMessage, seq: u64) {
    // Build a simple JSON object using serde_json
    let mut obj = serde_json::Map::new();
    obj.insert("seq".to_string(), serde_json::Value::from(seq));
    obj.insert(
        "decoder".to_string(),
        serde_json::Value::from(msg.decoder.clone()),
    );
    obj.insert(
        "summary".to_string(),
        serde_json::Value::from(msg.summary.clone()),
    );

    // Fields as nested object
    let fields: serde_json::Map<String, serde_json::Value> = msg
        .fields
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::from(v.clone())))
        .collect();
    obj.insert("fields".to_string(), serde_json::Value::Object(fields));

    // Raw bits as hex string
    if let Some(ref bits) = msg.raw_bits {
        let hex: String = bits.iter().map(|b| format!("{:02X}", b)).collect();
        obj.insert("raw_hex".to_string(), serde_json::Value::from(hex));
    }

    let json = serde_json::Value::Object(obj);
    println!("{}", json);
}
