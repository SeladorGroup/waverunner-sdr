use std::path::Path;

use anyhow::Result;

use wavecore::captures::{
    CaptureCatalog, CaptureImportOptions, CaptureMetadataSource, default_capture_path,
    delete_capture_artifacts, import_capture, inspect_capture_input, latest_capture,
    sync_catalog_metadata,
};
use wavecore::util::format_freq;

use super::parse_frequency;

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
    /// Inspect one capture and show the replay-ready path and inferred metadata
    Inspect {
        /// Capture path, metadata sidecar, .sigmf-meta file, or SigMF stem
        input: String,
        #[arg(long)]
        json: bool,
    },
    /// Show the newest indexed capture from the local library catalog
    Latest {
        #[arg(long)]
        json: bool,
    },
    /// Import an existing capture into the local library catalog
    Import {
        /// Capture path, metadata sidecar, .sigmf-meta file, or SigMF stem
        input: String,
        /// Sample rate in S/s for raw captures that lack metadata
        #[arg(short, long, value_parser = parse_frequency)]
        sample_rate: Option<f64>,
        /// Center frequency in Hz when it cannot be inferred
        #[arg(short, long, value_parser = parse_frequency)]
        frequency: Option<f64>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
    },
    /// Edit catalog metadata for one indexed capture
    Edit {
        /// `latest`, capture id, capture path, or metadata path
        selector: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        clear_label: bool,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long)]
        clear_notes: bool,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        replace_tags: bool,
        #[arg(long)]
        clear_tags: bool,
    },
    /// Remove a capture from the catalog, optionally deleting its files
    Remove {
        /// `latest`, capture id, capture path, or metadata path
        selector: String,
        #[arg(long)]
        delete_files: bool,
    },
}

pub fn run(args: LibraryArgs) -> Result<()> {
    match args.action {
        LibraryAction::List { limit, json } => list_captures(limit, json)?,
        LibraryAction::Prune => prune_captures()?,
        LibraryAction::DefaultPath { format, label } => {
            let path = default_capture_path(&format, label.as_deref())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{}", path.display());
        }
        LibraryAction::Inspect { input, json } => inspect_capture(&input, json)?,
        LibraryAction::Latest { json } => show_latest(json)?,
        LibraryAction::Import {
            input,
            sample_rate,
            frequency,
            label,
            notes,
            tags,
        } => import_existing_capture(&input, sample_rate, frequency, label, notes, tags)?,
        LibraryAction::Edit {
            selector,
            label,
            clear_label,
            notes,
            clear_notes,
            tags,
            replace_tags,
            clear_tags,
        } => edit_capture(
            selector,
            label,
            clear_label,
            notes,
            clear_notes,
            tags,
            replace_tags,
            clear_tags,
        )?,
        LibraryAction::Remove {
            selector,
            delete_files,
        } => remove_capture(selector, delete_files)?,
    }

    Ok(())
}

fn list_captures(limit: usize, json: bool) -> Result<()> {
    let (catalog, removed) = load_catalog_pruned()?;
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
        "{:<24} {:>14} {:>10} {:<12} Path",
        "Created", "Frequency", "Duration", "Format"
    );
    println!("{}", "-".repeat(104));
    if removed > 0 {
        println!("Pruned {removed} missing capture(s) before listing.\n");
    }
    for capture in captures {
        let duration = capture
            .duration_secs
            .map(|secs| format!("{secs:.1}s"))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<24} {:>14} {:>10} {:<12} {}",
            capture.created_at,
            format_capture_frequency(capture.center_freq),
            duration,
            capture.format,
            capture.path,
        );
    }
    Ok(())
}

fn prune_captures() -> Result<()> {
    let mut catalog = CaptureCatalog::load();
    let removed = catalog.prune_missing();
    catalog.save().map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Pruned {removed} missing capture(s).");
    Ok(())
}

fn inspect_capture(input: &str, json: bool) -> Result<()> {
    let info = inspect_capture_input(Path::new(input)).map_err(|e| anyhow::anyhow!("{e}"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&info)?);
        return Ok(());
    }

    println!("Resolved data path: {}", info.data_path);
    if let Some(path) = info.metadata_path {
        println!("Metadata path:      {path}");
    }
    if let Some(source) = info.metadata_source {
        println!(
            "Metadata source:    {}",
            match source {
                CaptureMetadataSource::RecordingSidecar => "recording sidecar",
                CaptureMetadataSource::SigMf => "sigmf",
            }
        );
    }
    if let Some(rate) = info.sample_rate {
        println!("Sample rate:        {:.3} MS/s", rate / 1e6);
    } else {
        println!("Sample rate:        (unknown)");
    }
    if let Some(freq) = info.center_freq {
        println!("Center frequency:   {}", format_capture_frequency(freq));
    } else {
        println!("Center frequency:   (unknown)");
    }
    Ok(())
}

fn show_latest(json: bool) -> Result<()> {
    let capture = latest_capture().map_err(|e| anyhow::anyhow!("{e}"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&capture)?);
        return Ok(());
    }

    print_capture_summary("Newest capture", &capture);
    Ok(())
}

fn import_existing_capture(
    input: &str,
    sample_rate: Option<f64>,
    frequency: Option<f64>,
    label: Option<String>,
    notes: Option<String>,
    tags: Vec<String>,
) -> Result<()> {
    let record = import_capture(
        Path::new(input),
        CaptureImportOptions {
            sample_rate,
            center_freq: frequency,
            label,
            notes,
            tags,
        },
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut catalog = CaptureCatalog::load();
    catalog.upsert(record.clone());
    catalog.save().map_err(|e| anyhow::anyhow!("{e}"))?;

    print_capture_summary("Imported capture", &record);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn edit_capture(
    selector: String,
    label: Option<String>,
    clear_label: bool,
    notes: Option<String>,
    clear_notes: bool,
    tags: Vec<String>,
    replace_tags: bool,
    clear_tags: bool,
) -> Result<()> {
    if clear_label && label.is_some() {
        anyhow::bail!("Use either --label or --clear-label, not both.");
    }
    if clear_notes && notes.is_some() {
        anyhow::bail!("Use either --notes or --clear-notes, not both.");
    }
    if clear_tags && !tags.is_empty() {
        anyhow::bail!("Use either --tag or --clear-tags, not both.");
    }

    let mut catalog = CaptureCatalog::load();
    let record = catalog
        .select_mut(&selector)
        .ok_or_else(|| anyhow::anyhow!("No capture matches selector '{selector}'."))?;

    if clear_label {
        record.label = None;
    } else if let Some(label) = label {
        record.label = Some(label);
    }

    if clear_notes {
        record.notes = None;
    } else if let Some(notes) = notes {
        record.notes = Some(notes);
    }

    if clear_tags {
        record.tags.clear();
    } else if !tags.is_empty() {
        if replace_tags {
            record.tags = tags;
        } else {
            for tag in tags {
                if !record.tags.contains(&tag) {
                    record.tags.push(tag);
                }
            }
        }
    }

    sync_catalog_metadata(record).map_err(|e| anyhow::anyhow!("{e}"))?;
    let updated = record.clone();
    catalog.save().map_err(|e| anyhow::anyhow!("{e}"))?;

    print_capture_summary("Updated capture", &updated);
    Ok(())
}

fn remove_capture(selector: String, delete_files: bool) -> Result<()> {
    let mut catalog = CaptureCatalog::load();
    let record = catalog
        .remove_selected(&selector)
        .ok_or_else(|| anyhow::anyhow!("No capture matches selector '{selector}'."))?;

    if delete_files {
        delete_capture_artifacts(&record).map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    catalog.save().map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("Removed capture: {}", record.path);
    if delete_files {
        println!("Deleted associated files.");
    }
    Ok(())
}

fn load_catalog_pruned() -> Result<(CaptureCatalog, usize)> {
    let mut catalog = CaptureCatalog::load();
    let removed = catalog.prune_missing();
    if removed > 0 {
        catalog.save().map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    Ok((catalog, removed))
}

fn print_capture_summary(header: &str, capture: &wavecore::captures::CaptureRecord) {
    println!("{header}:     {}", capture.path);
    if let Some(path) = capture.metadata_path.as_deref() {
        println!("Metadata path:      {path}");
    }
    println!("Created:            {}", capture.created_at);
    println!(
        "Center frequency:   {}",
        format_capture_frequency(capture.center_freq)
    );
    println!("Sample rate:        {:.3} MS/s", capture.sample_rate / 1e6);
    println!("Format:             {}", capture.format);
    if let Some(duration) = capture.duration_secs {
        println!("Duration:           {:.1}s", duration);
    }
    if let Some(label) = capture.label.as_deref() {
        println!("Label:              {label}");
    }
    if let Some(notes) = capture.notes.as_deref() {
        println!("Notes:              {notes}");
    }
    if !capture.tags.is_empty() {
        println!("Tags:               {}", capture.tags.join(", "));
    }
}

fn format_capture_frequency(freq: f64) -> String {
    if freq > 0.0 {
        format_freq(freq)
    } else {
        "(unknown)".to_string()
    }
}
