mod commands;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "waverunner",
    version,
    about = "SDR Signal Intelligence Platform"
)]
struct Cli {
    #[command(subcommand)]
    command: commands::Command,

    /// Verbosity level (-v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// SDR device index
    #[arg(short, long, default_value = "0", global = true)]
    device: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let filter = match cli.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(filter)),
        )
        .init();

    commands::execute(cli.command, cli.device).await
}
