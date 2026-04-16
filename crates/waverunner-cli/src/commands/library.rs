use anyhow::Result;

use wavecore::captures::{CaptureCatalog, default_capture_path};
use wavecore::util::format_freq;

#[derive(clap::Args)]
pub struct LibraryArgs {
    #[command(subcommand)]
    pub action: LibraryAction,
}

#[derive(clap::Subcommand)]
pub enum LibraryAction {
    /// List recent captures
    List {
        #[arg(long, default_value = "10")]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Remove catalog entries whose files no longer exist
    Prune,
    /// Print the default capture path that would be used for a new recording
    DefaultPath {
        #[arg(long, default_value = "raw", value_parser = ["raw", "wav", "sigmf"])]
        format: String,
        #[arg(long)]
        label: Option<String>,
    },
}

pub fn run(args: LibraryArgs) -> Result<()> {
    match args.action {
        LibraryAction::List { limit, json } => {
            let mut catalog = CaptureCatalog::load();
            let removed = catalog.prune_missing();
            if removed > 0 {
                catalog.save().map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            let captures = catalog.list_recent(limit);
            if json {
                println!("{}", serde_json::to_string_pretty(&captures)?);
                return Ok(());
            }

            if captures.is_empty() {
                println!("No recent captures.");
                return Ok(());
            }

            println!(
                "{:<24} {:>14} {:>10} {:<8} Path",
                "Created", "Frequency", "Duration", "Format"
            );
            println!("{}", "-".repeat(96));
            if removed > 0 {
                println!("Pruned {removed} missing capture(s) before listing.\n");
            }
            for capture in captures {
                let duration = capture
                    .duration_secs
                    .map(|secs| format!("{secs:.1}s"))
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "{:<24} {:>14} {:>10} {:<8} {}",
                    capture.created_at,
                    format_freq(capture.center_freq),
                    duration,
                    capture.format,
                    capture.path,
                );
            }
        }
        LibraryAction::Prune => {
            let mut catalog = CaptureCatalog::load();
            let removed = catalog.prune_missing();
            catalog.save().map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("Pruned {removed} missing capture(s).");
        }
        LibraryAction::DefaultPath { format, label } => {
            let path = default_capture_path(&format, label.as_deref())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{}", path.display());
        }
    }

    Ok(())
}
