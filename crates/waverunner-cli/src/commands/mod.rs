pub mod analyze;
pub mod bands;
pub mod bookmark;
pub mod decode;
pub mod demod;
pub mod identify;
pub mod info;
pub mod listen;
pub mod mode;
pub mod probe;
pub mod record;
pub mod recover;
pub mod scan;
pub mod tools;
pub mod tune;

use anyhow::Result;
use clap::Subcommand;

// Re-export shared parse_frequency for use as clap value_parser in subcommands.
pub use wavecore::util::parse_frequency;

#[derive(Subcommand)]
pub enum Command {
    /// Show connected SDR device information
    Info,
    /// Tune to a frequency and display real-time signal stats
    Tune(tune::TuneArgs),
    /// Scan a frequency range for active signals
    Scan(scan::ScanArgs),
    /// Record IQ samples to a file
    Record(record::RecordArgs),
    /// Demodulate a signal and output audio
    Demod(demod::DemodArgs),
    /// Tune to a frequency and listen (auto-detects mode from band)
    Listen(listen::ListenArgs),
    /// Decode a protocol (POCSAG, ADS-B, RDS)
    Decode(decode::DecodeArgs),
    /// Run a mode (auto-scan or mission profile)
    Mode(mode::ModeArgs),
    /// Analyze a recorded IQ file (measurements, burst, modulation, bitstream)
    Analyze(analyze::AnalyzeArgs),
    /// Check for or manage crash recovery checkpoints
    Recover(recover::RecoverArgs),
    /// Print environment diagnostics (OS, hardware, drivers)
    Probe(probe::ProbeArgs),
    /// Show known frequency bands and channels for your region
    Bands(bands::BandsArgs),
    /// Manage frequency bookmarks
    Bookmark(bookmark::BookmarkArgs),
    /// Identify a signal by frequency (band lookup + classifier + decoder trial)
    Identify(identify::IdentifyArgs),
    /// Show external decoder tool status and install hints
    Tools(tools::ToolsArgs),
}

pub async fn execute(cmd: Command, device_index: u32) -> Result<()> {
    match cmd {
        Command::Info => info::run(device_index).await,
        Command::Tune(args) => tune::run(args, device_index).await,
        Command::Scan(args) => scan::run(args, device_index).await,
        Command::Record(args) => record::run(args, device_index).await,
        Command::Demod(args) => demod::run(args, device_index).await,
        Command::Listen(args) => listen::run(args, device_index).await,
        Command::Decode(args) => decode::run(args, device_index).await,
        Command::Mode(args) => mode::run(args, device_index).await,
        Command::Analyze(args) => analyze::run(args, device_index).await,
        Command::Recover(args) => recover::run(args).await,
        Command::Probe(args) => probe::run(args).await,
        Command::Bands(args) => {
            bands::run(args)?;
            Ok(())
        }
        Command::Bookmark(args) => {
            bookmark::run(args)?;
            Ok(())
        }
        Command::Identify(args) => identify::run(args, device_index).await,
        Command::Tools(args) => {
            tools::run(args)?;
            Ok(())
        }
    }
}
