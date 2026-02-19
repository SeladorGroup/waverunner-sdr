use anyhow::Result;

use wavecore::session::checkpoint;

#[derive(clap::Args)]
pub struct RecoverArgs {
    /// Show checkpoint details as JSON
    #[arg(long)]
    pub show: bool,

    /// Clear stale checkpoint
    #[arg(long)]
    pub clear: bool,
}

pub async fn run(args: RecoverArgs) -> Result<()> {
    if args.clear {
        checkpoint::clear_checkpoint();
        println!("Checkpoint cleared.");
        return Ok(());
    }

    let path = checkpoint::checkpoint_path();

    match checkpoint::load_checkpoint() {
        Some(cp) => {
            if args.show {
                let json = serde_json::to_string_pretty(&cp)
                    .map_err(|e| anyhow::anyhow!("Serialize error: {e}"))?;
                println!("{json}");
            } else {
                println!("Found checkpoint at: {}", path.display());
                println!();
                println!("  Timestamp:    {}", cp.timestamp);
                println!("  Frequency:    {:.6} MHz", cp.frequency / 1e6);
                println!("  Sample rate:  {:.3} MS/s", cp.config.sample_rate / 1e6);
                println!("  Gain:         {:?}", cp.gain);
                println!("  Blocks:       {}", cp.blocks_processed);
                println!("  Events lost:  {}", cp.events_dropped);
                if !cp.active_decoders.is_empty() {
                    println!("  Decoders:     {}", cp.active_decoders.join(", "));
                }
                if let Some(ref rec) = cp.recording_path {
                    println!("  Recording:    {rec}");
                }
                println!();
                println!("Use --show for full JSON, --clear to remove.");
            }
        }
        None => {
            if path.exists() {
                println!("Checkpoint exists but is corrupt or from a newer version.");
                println!("Use --clear to remove: {}", path.display());
            } else {
                println!("No checkpoint found.");
            }
        }
    }

    Ok(())
}
