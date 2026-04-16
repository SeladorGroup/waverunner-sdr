use std::collections::HashMap;
use std::io::Write;
use std::time::Instant;

use anyhow::Result;

use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::dsp::decoders;
use wavecore::session::manager::SessionManager;
use wavecore::session::{Command, DecodedMessage, Event, SessionConfig};
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
    /// Run any registered decoder by name (use `decode list` to see all)
    Run(RunArgs),
    /// List all available decoders
    List,
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

#[derive(clap::Args)]
pub struct RunArgs {
    /// Decoder name (e.g., aprs, ais, ook, rtl433, noaa-apt-15)
    pub name: String,

    /// Center frequency (supports suffixes: k, M, G)
    #[arg(short, long, value_parser = parse_frequency)]
    pub frequency: f64,
}

// ============================================================================
// Protocol defaults
// ============================================================================

/// Sample rate and decoder name for each protocol.
struct ProtocolConfig {
    decoder_name: String,
    default_sample_rate: f64,
    frequency: f64,
    description: String,
}

fn protocol_config(protocol: &DecodeProtocol) -> Option<ProtocolConfig> {
    match protocol {
        DecodeProtocol::Pocsag(args) => {
            let name = match args.baud {
                512 => "pocsag-512",
                2400 => "pocsag-2400",
                _ => "pocsag-1200",
            };
            Some(ProtocolConfig {
                decoder_name: name.to_string(),
                // POCSAG needs enough bandwidth for ±4.5 kHz FSK.
                // 22050 Hz gives ~5× oversampling at 1200 baud.
                default_sample_rate: 2_048_000.0,
                frequency: args.frequency,
                description: "POCSAG pager".to_string(),
            })
        }
        DecodeProtocol::Adsb(args) => Some(ProtocolConfig {
            decoder_name: "adsb".to_string(),
            // ADS-B PPM at 1 Mbps needs ≥2 MS/s for Nyquist
            default_sample_rate: 2_000_000.0,
            frequency: args.frequency,
            description: "ADS-B 1090 MHz".to_string(),
        }),
        DecodeProtocol::Rds(args) => Some(ProtocolConfig {
            decoder_name: "rds".to_string(),
            // RDS needs raw IQ with FM discriminator, at a rate high enough
            // for the 57 kHz subcarrier. 2.048 MS/s is standard RTL-SDR rate.
            default_sample_rate: 2_048_000.0,
            frequency: args.frequency,
            description: "RDS/RBDS".to_string(),
        }),
        DecodeProtocol::Run(args) => {
            // Use the decoder's declared requirements for sample rate
            let mut registry = DecoderRegistry::new();
            decoders::register_all(&mut registry);
            let decoder = registry.create(&args.name)?;
            let req = decoder.requirements();
            Some(ProtocolConfig {
                decoder_name: args.name.clone(),
                default_sample_rate: req.sample_rate.max(250_000.0),
                frequency: args.frequency,
                description: args.name.clone(),
            })
        }
        DecodeProtocol::List => None,
    }
}

// ============================================================================
// Main entry point
// ============================================================================

pub async fn run(args: DecodeArgs, device_index: u32) -> Result<()> {
    // Handle `decode list` — print all registered decoders and exit
    if matches!(args.protocol, DecodeProtocol::List) {
        let tool_index: HashMap<_, _> = wavecore::dsp::decoders::tools::cached_tools()
            .iter()
            .map(|tool| (tool.name, tool))
            .collect();

        if matches!(args.format, OutputFormat::Json) {
            let rows: Vec<serde_json::Value> = decoders::DECODER_DESCRIPTORS
                .iter()
                .map(|descriptor| {
                    let tool = descriptor
                        .required_tool
                        .and_then(|tool_name| tool_index.get(tool_name).copied());
                    serde_json::json!({
                        "name": descriptor.name,
                        "backend": descriptor.backend.as_str(),
                        "required_tool": descriptor.required_tool,
                        "tool_installed": tool.map(|t| t.installed),
                        "resolved_command": tool.and_then(|t| t.resolved_command),
                        "summary": descriptor.summary,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            println!("Available decoders:");
            println!();
            println!(
                "  {:<14} {:<8} {:<18} Summary",
                "Name", "Backend", "Support"
            );
            for descriptor in decoders::DECODER_DESCRIPTORS {
                let support = match descriptor.required_tool {
                    None => "builtin".to_string(),
                    Some(tool_name) => match tool_index.get(tool_name).copied() {
                        Some(tool) if tool.installed => match tool.resolved_command {
                            Some(command) if command != tool_name => format!("ready ({command})"),
                            _ => "ready".to_string(),
                        },
                        Some(_) | None => format!("missing ({tool_name})"),
                    },
                };

                println!(
                    "  {:<14} {:<8} {:<18} {}",
                    descriptor.name,
                    descriptor.backend.as_str(),
                    support,
                    descriptor.summary,
                );
            }
        }
        return Ok(());
    }

    let pcfg = match protocol_config(&args.protocol) {
        Some(cfg) => cfg,
        None => anyhow::bail!("Unknown decoder. Use `decode list` to see available decoders."),
    };
    let sample_rate = args.sample_rate.unwrap_or(pcfg.default_sample_rate);
    let gain_mode = wavecore::util::parse_gain(&args.gain).map_err(|e| anyhow::anyhow!("{e}"))?;

    // Build decoder registry for the SessionManager
    let mut registry = DecoderRegistry::new();
    decoders::register_all(&mut registry);

    // Verify the decoder name is valid before starting hardware
    if registry.create(&pcfg.decoder_name).is_none() {
        anyhow::bail!("Unknown decoder: {}", pcfg.decoder_name);
    }

    // Create session config
    let config = SessionConfig {
        schema_version: 1,
        device_index,
        frequency: pcfg.frequency,
        sample_rate,
        gain: gain_mode,
        ppm: args.ppm,
        fft_size: 2048,
        pfa: 1e-4,
    };

    // Start SessionManager — handles hardware, pipeline, DC removal, DSP
    let (session, event_rx) = SessionManager::new(config, registry)
        .map_err(|e| anyhow::anyhow!("Failed to start session: {e}"))?;

    // Enable the decoder via SessionManager command
    session
        .send(Command::EnableDecoder(pcfg.decoder_name.to_string()))
        .map_err(|e| anyhow::anyhow!("Failed to enable decoder: {e}"))?;

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

    // Ctrl+C handler — sends Shutdown via the session's command channel
    let running = session.running_flag();
    let cmd_tx = session.cmd_sender();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        eprintln!("\nStopping...");
        running.store(false, std::sync::atomic::Ordering::Relaxed);
        cmd_tx.send(Command::Shutdown).ok();
    });

    let mut msg_count = 0u64;
    let start_time = Instant::now();

    // Drain events — we only care about DecodedMessage and Stats (for progress)
    while session.is_running() {
        match event_rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(Event::DecodedMessage(msg)) => {
                msg_count += 1;
                print_message(&msg, msg_count, &args.format);
            }
            Ok(Event::Stats(stats)) => {
                let elapsed = start_time.elapsed().as_secs_f64();
                eprint!(
                    "\r  [{:.0}s] blocks: {} | messages: {} ",
                    elapsed, stats.blocks_processed, msg_count,
                );
                std::io::stderr().flush().ok();
            }
            Ok(Event::Error(e)) => {
                eprintln!("\nError: {e}");
            }
            Ok(_) => {
                // Ignore spectrum, detections, demod-vis, status events
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Drain remaining events after shutdown
    while let Ok(event) = event_rx.try_recv() {
        if let Event::DecodedMessage(msg) = event {
            msg_count += 1;
            print_message(&msg, msg_count, &args.format);
        }
    }

    // Cleanup
    session.shutdown();

    let elapsed = start_time.elapsed().as_secs_f64();
    eprintln!("\nDone. {} messages decoded in {:.1}s.", msg_count, elapsed,);
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
    println!("━━━ #{} [{}] {} ━━━", seq, msg.decoder, age,);
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
