pub mod decode;
pub mod demod;
pub mod info;
pub mod record;
pub mod scan;
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
    /// Decode a protocol (POCSAG, ADS-B, RDS)
    Decode(decode::DecodeArgs),
}

pub async fn execute(cmd: Command, device_index: u32) -> Result<()> {
    match cmd {
        Command::Info => info::run(device_index).await,
        Command::Tune(args) => tune::run(args, device_index).await,
        Command::Scan(args) => scan::run(args, device_index).await,
        Command::Record(args) => record::run(args, device_index).await,
        Command::Demod(args) => demod::run(args, device_index).await,
        Command::Decode(args) => decode::run(args, device_index).await,
    }
}
