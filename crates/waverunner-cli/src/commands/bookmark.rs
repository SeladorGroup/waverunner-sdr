use anyhow::Result;
use wavecore::bookmarks::{Bookmark, BookmarkStore};

use super::parse_frequency;

#[derive(clap::Args)]
pub struct BookmarkArgs {
    #[command(subcommand)]
    pub action: BookmarkAction,
}

#[derive(clap::Subcommand)]
pub enum BookmarkAction {
    /// Add a frequency bookmark
    Add {
        /// Bookmark name
        name: String,
        /// Frequency (supports suffixes: k, M, G)
        #[arg(value_parser = parse_frequency)]
        frequency: f64,
        /// Demodulation mode
        #[arg(short, long)]
        mode: Option<String>,
        /// Decoder name
        #[arg(short, long)]
        decoder: Option<String>,
        /// Notes
        #[arg(short, long)]
        notes: Option<String>,
    },
    /// List all bookmarks
    List,
    /// Remove a bookmark by name
    Remove {
        /// Bookmark name to remove
        name: String,
    },
}

pub fn run(args: BookmarkArgs) -> Result<()> {
    let mut store = BookmarkStore::load();

    match args.action {
        BookmarkAction::Add {
            name,
            frequency,
            mode,
            decoder,
            notes,
        } => {
            let freq_str = wavecore::util::format_freq(frequency);
            store.add(Bookmark {
                name: name.clone(),
                frequency_hz: frequency,
                mode,
                decoder,
                notes,
            });
            store.save().map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("Bookmarked \"{name}\" at {freq_str}");
        }
        BookmarkAction::List => {
            let bookmarks = store.list();
            if bookmarks.is_empty() {
                println!("No bookmarks saved.");
                println!("Add one: waverunner bookmark add \"name\" <frequency>");
            } else {
                let header = format!(
                    "{:<20} {:>16}  {:<8} {:<10} Notes",
                    "Name", "Frequency", "Mode", "Decoder"
                );
                println!("{header}");
                println!("{}", "-".repeat(70));
                for bm in bookmarks {
                    let freq = wavecore::util::format_freq(bm.frequency_hz);
                    let mode = bm.mode.as_deref().unwrap_or("-");
                    let decoder = bm.decoder.as_deref().unwrap_or("-");
                    let notes = bm.notes.as_deref().unwrap_or("");
                    println!("{:<20} {:>16}  {:<8} {:<10} {}", bm.name, freq, mode, decoder, notes);
                }
                println!("\n{} bookmarks", bookmarks.len());
            }
        }
        BookmarkAction::Remove { name } => {
            if store.remove(&name) {
                store.save().map_err(|e| anyhow::anyhow!("{e}"))?;
                println!("Removed bookmark \"{name}\"");
            } else {
                println!("No bookmark named \"{name}\" found.");
            }
        }
    }

    Ok(())
}
