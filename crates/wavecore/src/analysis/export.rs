//! Data export to CSV, JSON, and PNG formats.
//!
//! Exports spectrum snapshots, tracking time-series, measurement reports,
//! decoded messages, and full analysis reports. PNG exports produce
//! spectrum plots and waterfall images.

use std::io::Write;
use std::path::PathBuf;

use image::{Rgb, RgbImage};

/// Configuration for data export.
#[derive(Debug, Clone)]
pub struct ExportConfig {
    /// Output file path.
    pub path: PathBuf,
    /// Export format.
    pub format: ExportFormat,
    /// What to export.
    pub content: ExportContent,
}

/// Export file format.
#[derive(Debug, Clone, serde::Serialize)]
pub enum ExportFormat {
    /// Comma-separated values.
    Csv,
    /// JSON document.
    Json,
    /// Tab-separated values.
    Tsv,
    /// PNG image (spectrum plot or waterfall).
    Png,
}

/// Content to export.
#[derive(Debug, Clone)]
pub enum ExportContent {
    /// Current spectrum snapshot.
    Spectrum {
        spectrum_db: Vec<f32>,
        sample_rate: f64,
        center_freq: f64,
    },
    /// Tracking time-series data.
    Tracking(super::tracking::TrackingSnapshot),
    /// Measurement report.
    Measurement(super::measurement::MeasurementReport),
    /// Decoded messages log.
    DecodedMessages(Vec<DecodedMessageExport>),
    /// Detection log.
    Detections {
        detections: Vec<DetectionExport>,
        center_freq: f64,
    },
    /// Waterfall data (multiple spectrum rows, oldest first).
    Waterfall {
        rows: Vec<Vec<f32>>,
        sample_rate: f64,
        center_freq: f64,
    },
}

/// Simplified decoded message for export (avoids Instant serialization).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DecodedMessageExport {
    pub decoder: String,
    pub elapsed_ms: u64,
    pub summary: String,
    pub fields: std::collections::BTreeMap<String, String>,
}

/// Simplified detection for export.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DetectionExport {
    pub bin: usize,
    pub power_db: f32,
    pub snr_db: f32,
    pub frequency_hz: f64,
}

/// Write export data to file.
///
/// Creates parent directories if needed. Returns the absolute path on success.
pub fn export_to_file(config: &ExportConfig) -> Result<String, String> {
    if let Some(parent) = config.path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {e}"))?;
    }

    match (&config.format, &config.content) {
        (
            ExportFormat::Csv,
            ExportContent::Spectrum {
                spectrum_db,
                sample_rate,
                center_freq,
            },
        ) => export_spectrum_csv(&config.path, spectrum_db, *sample_rate, *center_freq),
        (ExportFormat::Csv, ExportContent::Tracking(snap)) => {
            export_tracking_csv(&config.path, snap)
        }
        (ExportFormat::Csv, ExportContent::DecodedMessages(msgs)) => {
            export_messages_csv(&config.path, msgs)
        }
        (
            ExportFormat::Csv,
            ExportContent::Detections {
                detections,
                center_freq,
            },
        ) => export_detections_csv(&config.path, detections, *center_freq),
        (ExportFormat::Csv, ExportContent::Measurement(report)) => {
            export_measurement_csv(&config.path, report)
        }
        (ExportFormat::Json, content) => export_json(&config.path, content),
        (
            ExportFormat::Tsv,
            ExportContent::Spectrum {
                spectrum_db,
                sample_rate,
                center_freq,
            },
        ) => export_spectrum_tsv(&config.path, spectrum_db, *sample_rate, *center_freq),
        (ExportFormat::Tsv, ExportContent::Tracking(snap)) => {
            export_tracking_tsv(&config.path, snap)
        }
        (ExportFormat::Tsv, ExportContent::DecodedMessages(msgs)) => {
            export_messages_tsv(&config.path, msgs)
        }
        (
            ExportFormat::Tsv,
            ExportContent::Detections {
                detections,
                center_freq,
            },
        ) => export_detections_tsv(&config.path, detections, *center_freq),
        (ExportFormat::Tsv, ExportContent::Measurement(report)) => {
            export_measurement_tsv(&config.path, report)
        }
        // PNG exports
        (
            ExportFormat::Png,
            ExportContent::Spectrum {
                spectrum_db,
                sample_rate,
                center_freq,
            },
        ) => export_spectrum_png(&config.path, spectrum_db, *sample_rate, *center_freq),
        (
            ExportFormat::Png,
            ExportContent::Waterfall {
                rows,
                sample_rate,
                center_freq,
            },
        ) => export_waterfall_png(&config.path, rows, *sample_rate, *center_freq),
        (ExportFormat::Png, _) => {
            Err("PNG export only supports Spectrum and Waterfall content".to_string())
        }
        // CSV/TSV don't support Waterfall
        (_, ExportContent::Waterfall { .. }) => {
            Err("Waterfall content is only supported with PNG format".to_string())
        }
    }
}

fn export_spectrum_csv(
    path: &PathBuf,
    spectrum_db: &[f32],
    sample_rate: f64,
    center_freq: f64,
) -> Result<String, String> {
    let mut file =
        std::fs::File::create(path).map_err(|e| format!("Failed to create file: {e}"))?;

    writeln!(file, "frequency_hz,power_dbfs").map_err(|e| format!("Write error: {e}"))?;

    let n = spectrum_db.len();
    let bin_width = sample_rate / n as f64;

    for (i, &db) in spectrum_db.iter().enumerate() {
        let freq = center_freq + (i as f64 - n as f64 / 2.0) * bin_width;
        writeln!(file, "{freq:.1},{db:.2}").map_err(|e| format!("Write error: {e}"))?;
    }

    Ok(path.display().to_string())
}

fn export_tracking_csv(
    path: &PathBuf,
    snap: &super::tracking::TrackingSnapshot,
) -> Result<String, String> {
    let mut file =
        std::fs::File::create(path).map_err(|e| format!("Failed to create file: {e}"))?;

    writeln!(
        file,
        "time_s,snr_db,power_dbfs,noise_floor_db,freq_offset_hz"
    )
    .map_err(|e| format!("Write error: {e}"))?;

    let len = snap.snr.len();
    for i in 0..len {
        let t = snap.snr.get(i).map(|v| v.0).unwrap_or(0.0);
        let snr = snap.snr.get(i).map(|v| v.1).unwrap_or(0.0);
        let power = snap.power.get(i).map(|v| v.1).unwrap_or(0.0);
        let noise = snap.noise_floor.get(i).map(|v| v.1).unwrap_or(0.0);
        let freq = snap.freq_offset.get(i).map(|v| v.1).unwrap_or(0.0);
        writeln!(file, "{t:.3},{snr:.2},{power:.2},{noise:.2},{freq:.1}")
            .map_err(|e| format!("Write error: {e}"))?;
    }

    Ok(path.display().to_string())
}

fn export_messages_csv(path: &PathBuf, msgs: &[DecodedMessageExport]) -> Result<String, String> {
    let mut file =
        std::fs::File::create(path).map_err(|e| format!("Failed to create file: {e}"))?;

    writeln!(file, "elapsed_ms,decoder,summary").map_err(|e| format!("Write error: {e}"))?;

    for msg in msgs {
        // Escape commas and quotes in summary
        let escaped = msg.summary.replace('"', "\"\"");
        writeln!(
            file,
            "{},\"{}\",\"{}\"",
            msg.elapsed_ms, msg.decoder, escaped
        )
        .map_err(|e| format!("Write error: {e}"))?;
    }

    Ok(path.display().to_string())
}

fn export_detections_csv(
    path: &PathBuf,
    detections: &[DetectionExport],
    _center_freq: f64,
) -> Result<String, String> {
    let mut file =
        std::fs::File::create(path).map_err(|e| format!("Failed to create file: {e}"))?;

    writeln!(file, "frequency_hz,power_dbfs,snr_db").map_err(|e| format!("Write error: {e}"))?;

    for det in detections {
        writeln!(
            file,
            "{:.1},{:.2},{:.2}",
            det.frequency_hz, det.power_db, det.snr_db
        )
        .map_err(|e| format!("Write error: {e}"))?;
    }

    Ok(path.display().to_string())
}

fn export_measurement_csv(
    path: &PathBuf,
    report: &super::measurement::MeasurementReport,
) -> Result<String, String> {
    let mut file =
        std::fs::File::create(path).map_err(|e| format!("Failed to create file: {e}"))?;

    writeln!(file, "metric,value,unit").map_err(|e| format!("Write error: {e}"))?;
    writeln!(file, "bandwidth_3db,{:.1},Hz", report.bandwidth_3db_hz)
        .map_err(|e| format!("Write error: {e}"))?;
    writeln!(file, "bandwidth_6db,{:.1},Hz", report.bandwidth_6db_hz)
        .map_err(|e| format!("Write error: {e}"))?;
    writeln!(file, "occupied_bw,{:.1},Hz", report.occupied_bw_hz)
        .map_err(|e| format!("Write error: {e}"))?;
    writeln!(file, "obw_percent,{:.1},%", report.obw_percent)
        .map_err(|e| format!("Write error: {e}"))?;
    writeln!(file, "channel_power,{:.2},dBFS", report.channel_power_dbfs)
        .map_err(|e| format!("Write error: {e}"))?;
    writeln!(file, "acpr_lower,{:.2},dBc", report.acpr_lower_dbc)
        .map_err(|e| format!("Write error: {e}"))?;
    writeln!(file, "acpr_upper,{:.2},dBc", report.acpr_upper_dbc)
        .map_err(|e| format!("Write error: {e}"))?;
    writeln!(file, "papr,{:.2},dB", report.papr_db).map_err(|e| format!("Write error: {e}"))?;

    Ok(path.display().to_string())
}

// ============================================================================
// TSV exports (tab-delimited mirrors of CSV)
// ============================================================================

fn export_spectrum_tsv(
    path: &PathBuf,
    spectrum_db: &[f32],
    sample_rate: f64,
    center_freq: f64,
) -> Result<String, String> {
    let mut file =
        std::fs::File::create(path).map_err(|e| format!("Failed to create file: {e}"))?;

    writeln!(file, "frequency_hz\tpower_dbfs").map_err(|e| format!("Write error: {e}"))?;

    let n = spectrum_db.len();
    let bin_width = sample_rate / n as f64;

    for (i, &db) in spectrum_db.iter().enumerate() {
        let freq = center_freq + (i as f64 - n as f64 / 2.0) * bin_width;
        writeln!(file, "{freq:.1}\t{db:.2}").map_err(|e| format!("Write error: {e}"))?;
    }

    Ok(path.display().to_string())
}

fn export_tracking_tsv(
    path: &PathBuf,
    snap: &super::tracking::TrackingSnapshot,
) -> Result<String, String> {
    let mut file =
        std::fs::File::create(path).map_err(|e| format!("Failed to create file: {e}"))?;

    writeln!(
        file,
        "time_s\tsnr_db\tpower_dbfs\tnoise_floor_db\tfreq_offset_hz"
    )
    .map_err(|e| format!("Write error: {e}"))?;

    let len = snap.snr.len();
    for i in 0..len {
        let t = snap.snr.get(i).map(|v| v.0).unwrap_or(0.0);
        let snr = snap.snr.get(i).map(|v| v.1).unwrap_or(0.0);
        let power = snap.power.get(i).map(|v| v.1).unwrap_or(0.0);
        let noise = snap.noise_floor.get(i).map(|v| v.1).unwrap_or(0.0);
        let freq = snap.freq_offset.get(i).map(|v| v.1).unwrap_or(0.0);
        writeln!(file, "{t:.3}\t{snr:.2}\t{power:.2}\t{noise:.2}\t{freq:.1}")
            .map_err(|e| format!("Write error: {e}"))?;
    }

    Ok(path.display().to_string())
}

fn export_messages_tsv(path: &PathBuf, msgs: &[DecodedMessageExport]) -> Result<String, String> {
    let mut file =
        std::fs::File::create(path).map_err(|e| format!("Failed to create file: {e}"))?;

    writeln!(file, "elapsed_ms\tdecoder\tsummary").map_err(|e| format!("Write error: {e}"))?;

    for msg in msgs {
        writeln!(file, "{}\t{}\t{}", msg.elapsed_ms, msg.decoder, msg.summary)
            .map_err(|e| format!("Write error: {e}"))?;
    }

    Ok(path.display().to_string())
}

fn export_detections_tsv(
    path: &PathBuf,
    detections: &[DetectionExport],
    _center_freq: f64,
) -> Result<String, String> {
    let mut file =
        std::fs::File::create(path).map_err(|e| format!("Failed to create file: {e}"))?;

    writeln!(file, "frequency_hz\tpower_dbfs\tsnr_db").map_err(|e| format!("Write error: {e}"))?;

    for det in detections {
        writeln!(
            file,
            "{:.1}\t{:.2}\t{:.2}",
            det.frequency_hz, det.power_db, det.snr_db
        )
        .map_err(|e| format!("Write error: {e}"))?;
    }

    Ok(path.display().to_string())
}

fn export_measurement_tsv(
    path: &PathBuf,
    report: &super::measurement::MeasurementReport,
) -> Result<String, String> {
    let mut file =
        std::fs::File::create(path).map_err(|e| format!("Failed to create file: {e}"))?;

    writeln!(file, "metric\tvalue\tunit").map_err(|e| format!("Write error: {e}"))?;
    writeln!(file, "bandwidth_3db\t{:.1}\tHz", report.bandwidth_3db_hz)
        .map_err(|e| format!("Write error: {e}"))?;
    writeln!(file, "bandwidth_6db\t{:.1}\tHz", report.bandwidth_6db_hz)
        .map_err(|e| format!("Write error: {e}"))?;
    writeln!(file, "occupied_bw\t{:.1}\tHz", report.occupied_bw_hz)
        .map_err(|e| format!("Write error: {e}"))?;
    writeln!(file, "obw_percent\t{:.1}\t%", report.obw_percent)
        .map_err(|e| format!("Write error: {e}"))?;
    writeln!(
        file,
        "channel_power\t{:.2}\tdBFS",
        report.channel_power_dbfs
    )
    .map_err(|e| format!("Write error: {e}"))?;
    writeln!(file, "acpr_lower\t{:.2}\tdBc", report.acpr_lower_dbc)
        .map_err(|e| format!("Write error: {e}"))?;
    writeln!(file, "acpr_upper\t{:.2}\tdBc", report.acpr_upper_dbc)
        .map_err(|e| format!("Write error: {e}"))?;
    writeln!(file, "papr\t{:.2}\tdB", report.papr_db).map_err(|e| format!("Write error: {e}"))?;

    Ok(path.display().to_string())
}

// ============================================================================
// PNG exports (spectrum plot and waterfall image)
// ============================================================================

/// Color-map a dB value to an RGB color (dark blue → cyan → yellow → red).
fn db_to_color(db: f32, min_db: f32, max_db: f32) -> Rgb<u8> {
    let range = (max_db - min_db).max(1.0);
    let t = ((db - min_db) / range).clamp(0.0, 1.0);

    // 4-stop gradient: dark blue → cyan → yellow → red
    let (r, g, b) = if t < 0.33 {
        let s = t / 0.33;
        (0.0, s * 0.8, 0.3 + s * 0.7) // dark blue → cyan
    } else if t < 0.66 {
        let s = (t - 0.33) / 0.33;
        (s, 0.8 + s * 0.2, 1.0 - s) // cyan → yellow
    } else {
        let s = (t - 0.66) / 0.34;
        (1.0, 1.0 - s * 0.7, 0.0) // yellow → red
    };

    Rgb([(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8])
}

/// Simple 3x5 pixel digit glyphs for axis labels (no font crate needed).
const DIGIT_GLYPHS: [[u8; 15]; 11] = [
    // 0
    [1, 1, 1, 1, 0, 1, 1, 0, 1, 1, 0, 1, 1, 1, 1],
    // 1
    [0, 1, 0, 1, 1, 0, 0, 1, 0, 0, 1, 0, 1, 1, 1],
    // 2
    [1, 1, 1, 0, 0, 1, 1, 1, 1, 1, 0, 0, 1, 1, 1],
    // 3
    [1, 1, 1, 0, 0, 1, 1, 1, 1, 0, 0, 1, 1, 1, 1],
    // 4
    [1, 0, 1, 1, 0, 1, 1, 1, 1, 0, 0, 1, 0, 0, 1],
    // 5
    [1, 1, 1, 1, 0, 0, 1, 1, 1, 0, 0, 1, 1, 1, 1],
    // 6
    [1, 1, 1, 1, 0, 0, 1, 1, 1, 1, 0, 1, 1, 1, 1],
    // 7
    [1, 1, 1, 0, 0, 1, 0, 1, 0, 0, 1, 0, 0, 1, 0],
    // 8
    [1, 1, 1, 1, 0, 1, 1, 1, 1, 1, 0, 1, 1, 1, 1],
    // 9
    [1, 1, 1, 1, 0, 1, 1, 1, 1, 0, 0, 1, 1, 1, 1],
    // . (dot)
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0],
];

/// Draw a small text string at (x, y) using 3x5 glyphs.
fn draw_text(img: &mut RgbImage, x: u32, y: u32, text: &str, color: Rgb<u8>) {
    let mut cx = x;
    for ch in text.chars() {
        let glyph_idx = match ch {
            '0'..='9' => (ch as u8 - b'0') as usize,
            '.' => 10,
            '-' => {
                // Draw a minus sign inline
                for dx in 0..3u32 {
                    if y + 2 < img.height() && cx + dx < img.width() {
                        img.put_pixel(cx + dx, y + 2, color);
                    }
                }
                cx += 4;
                continue;
            }
            ' ' => {
                cx += 4;
                continue;
            }
            _ => {
                cx += 4;
                continue;
            }
        };
        let glyph = &DIGIT_GLYPHS[glyph_idx];
        for row in 0..5u32 {
            for col in 0..3u32 {
                if glyph[(row * 3 + col) as usize] == 1 {
                    let px = cx + col;
                    let py = y + row;
                    if px < img.width() && py < img.height() {
                        img.put_pixel(px, py, color);
                    }
                }
            }
        }
        cx += 4;
    }
}

fn export_spectrum_png(
    path: &PathBuf,
    spectrum_db: &[f32],
    sample_rate: f64,
    center_freq: f64,
) -> Result<String, String> {
    let n = spectrum_db.len();
    if n == 0 {
        return Err("Empty spectrum".to_string());
    }

    let width: u32 = 1024;
    let height: u32 = 512;
    let margin_left: u32 = 50;
    let margin_bottom: u32 = 30;
    let margin_top: u32 = 20;
    let margin_right: u32 = 10;
    let plot_w = width - margin_left - margin_right;
    let plot_h = height - margin_top - margin_bottom;

    let bg = Rgb([20u8, 20, 30]);
    let grid_color = Rgb([50, 50, 60]);
    let trace_color = Rgb([0, 220, 220]);
    let text_color = Rgb([180, 180, 180]);

    let mut img = RgbImage::from_pixel(width, height, bg);

    // Compute dB range
    let min_db = spectrum_db.iter().copied().fold(f32::INFINITY, f32::min);
    let max_db = spectrum_db
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let db_range = (max_db - min_db).max(1.0);
    let db_floor = min_db - db_range * 0.05;
    let db_ceil = max_db + db_range * 0.05;
    let effective_range = db_ceil - db_floor;

    // Draw grid lines (horizontal = power, vertical = frequency)
    for i in 0..=5 {
        let y = margin_top + (plot_h as f32 * i as f32 / 5.0) as u32;
        for x in margin_left..width - margin_right {
            if x < img.width() && y < img.height() {
                img.put_pixel(x, y, grid_color);
            }
        }
        // Power label
        let db_val = db_ceil - (effective_range * i as f32 / 5.0);
        draw_text(
            &mut img,
            2,
            y.saturating_sub(2),
            &format!("{db_val:.0}"),
            text_color,
        );
    }

    for i in 0..=4 {
        let x = margin_left + (plot_w as f32 * i as f32 / 4.0) as u32;
        for y in margin_top..height - margin_bottom {
            if x < img.width() && y < img.height() {
                img.put_pixel(x, y, grid_color);
            }
        }
        // Frequency label
        let freq = center_freq - sample_rate / 2.0 + sample_rate * i as f64 / 4.0;
        let label = format!("{:.1}", freq / 1e6);
        draw_text(
            &mut img,
            x.saturating_sub(10),
            height - margin_bottom + 5,
            &label,
            text_color,
        );
    }

    // Draw spectrum trace
    for px in 0..plot_w {
        let bin = (px as f64 / plot_w as f64 * n as f64) as usize;
        let bin = bin.min(n - 1);
        let db = spectrum_db[bin];
        let normalized = ((db - db_floor) / effective_range).clamp(0.0, 1.0);
        let y = margin_top + ((1.0 - normalized) * plot_h as f32) as u32;
        let x = margin_left + px;

        // Fill below trace with dim color
        for fill_y in y..margin_top + plot_h {
            if x < img.width() && fill_y < img.height() {
                let fill_color = Rgb([0, 40, 50]);
                img.put_pixel(x, fill_y, fill_color);
            }
        }

        // Bright trace line
        if x < img.width() && y < img.height() {
            img.put_pixel(x, y, trace_color);
            if y + 1 < img.height() {
                img.put_pixel(x, y + 1, trace_color);
            }
        }
    }

    img.save(path).map_err(|e| format!("PNG save error: {e}"))?;
    Ok(path.display().to_string())
}

fn export_waterfall_png(
    path: &PathBuf,
    rows: &[Vec<f32>],
    _sample_rate: f64,
    _center_freq: f64,
) -> Result<String, String> {
    if rows.is_empty() {
        return Err("Empty waterfall data".to_string());
    }

    let bins = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if bins == 0 {
        return Err("Empty waterfall bins".to_string());
    }

    let width = bins.min(2048) as u32;
    let height = rows.len().min(1024) as u32;

    // Compute global dB range across all rows
    let mut min_db = f32::INFINITY;
    let mut max_db = f32::NEG_INFINITY;
    for row in rows {
        for &db in row {
            if db.is_finite() {
                min_db = min_db.min(db);
                max_db = max_db.max(db);
            }
        }
    }
    if !min_db.is_finite() {
        min_db = -100.0;
    }
    if !max_db.is_finite() {
        max_db = 0.0;
    }

    let mut img = RgbImage::new(width, height);

    for (row_idx, row) in rows.iter().enumerate().take(height as usize) {
        for px in 0..width {
            let bin = (px as f64 / width as f64 * bins as f64) as usize;
            let db = row.get(bin).copied().unwrap_or(min_db);
            let color = db_to_color(db, min_db, max_db);
            img.put_pixel(px, row_idx as u32, color);
        }
    }

    img.save(path).map_err(|e| format!("PNG save error: {e}"))?;
    Ok(path.display().to_string())
}

fn export_json(path: &PathBuf, content: &ExportContent) -> Result<String, String> {
    let json = match content {
        ExportContent::Spectrum {
            spectrum_db,
            sample_rate,
            center_freq,
        } => {
            serde_json::json!({
                "type": "spectrum",
                "center_freq_hz": center_freq,
                "sample_rate": sample_rate,
                "bins": spectrum_db.len(),
                "spectrum_dbfs": spectrum_db,
            })
        }
        ExportContent::Tracking(snap) => {
            serde_json::to_value(snap).map_err(|e| format!("Serialize error: {e}"))?
        }
        ExportContent::Measurement(report) => {
            serde_json::to_value(report).map_err(|e| format!("Serialize error: {e}"))?
        }
        ExportContent::DecodedMessages(msgs) => {
            serde_json::to_value(msgs).map_err(|e| format!("Serialize error: {e}"))?
        }
        ExportContent::Detections {
            detections,
            center_freq,
        } => {
            serde_json::json!({
                "center_freq_hz": center_freq,
                "detections": detections,
            })
        }
        ExportContent::Waterfall {
            rows,
            sample_rate,
            center_freq,
        } => {
            serde_json::json!({
                "type": "waterfall",
                "center_freq_hz": center_freq,
                "sample_rate": sample_rate,
                "rows": rows.len(),
                "bins": rows.first().map(|r| r.len()).unwrap_or(0),
            })
        }
    };

    let formatted =
        serde_json::to_string_pretty(&json).map_err(|e| format!("JSON format error: {e}"))?;
    std::fs::write(path, &formatted).map_err(|e| format!("Write error: {e}"))?;

    Ok(path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("waverunner_test_export");
        std::fs::create_dir_all(&dir).ok();
        dir.join(name)
    }

    fn cleanup(path: &PathBuf) {
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn csv_spectrum_export() {
        let path = temp_path("test_spectrum.csv");
        let spectrum: Vec<f32> = (0..100).map(|i| -80.0 + (i as f32) * 0.5).collect();
        let result = export_to_file(&ExportConfig {
            path: path.clone(),
            format: ExportFormat::Csv,
            content: ExportContent::Spectrum {
                spectrum_db: spectrum,
                sample_rate: 2.048e6,
                center_freq: 100e6,
            },
        });
        assert!(result.is_ok());
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with("frequency_hz,power_dbfs\n"));
        assert!(contents.lines().count() > 100); // header + 100 data lines
        cleanup(&path);
    }

    #[test]
    fn csv_tracking_export() {
        let path = temp_path("test_tracking.csv");
        let snap = super::super::tracking::TrackingSnapshot {
            snr: vec![(0.0, 15.0), (1.0, 16.0)],
            power: vec![(0.0, -40.0), (1.0, -39.0)],
            noise_floor: vec![(0.0, -55.0), (1.0, -55.0)],
            freq_offset: vec![(0.0, 0.0), (1.0, 5.0)],
            summary: super::super::tracking::TrackingSummary {
                duration_secs: 1.0,
                snr_mean: 15.5,
                snr_min: 15.0,
                snr_max: 16.0,
                power_mean: -39.5,
                freq_drift_hz_per_sec: 5.0,
                stability_score: 0.95,
            },
        };
        let result = export_to_file(&ExportConfig {
            path: path.clone(),
            format: ExportFormat::Csv,
            content: ExportContent::Tracking(snap),
        });
        assert!(result.is_ok());
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("time_s,snr_db"));
        cleanup(&path);
    }

    #[test]
    fn json_report_export() {
        let path = temp_path("test_report.json");
        let report = super::super::measurement::MeasurementReport {
            bandwidth_3db_hz: 10000.0,
            bandwidth_6db_hz: 15000.0,
            occupied_bw_hz: 20000.0,
            obw_percent: 99.0,
            channel_power_dbfs: -25.0,
            acpr_lower_dbc: -40.0,
            acpr_upper_dbc: -38.0,
            papr_db: 5.0,
            freq_offset_hz: 50.0,
        };
        let result = export_to_file(&ExportConfig {
            path: path.clone(),
            format: ExportFormat::Json,
            content: ExportContent::Measurement(report),
        });
        assert!(result.is_ok());
        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["bandwidth_3db_hz"], 10000.0);
        cleanup(&path);
    }

    #[test]
    fn export_empty_data() {
        let path = temp_path("test_empty.csv");
        let result = export_to_file(&ExportConfig {
            path: path.clone(),
            format: ExportFormat::Csv,
            content: ExportContent::Spectrum {
                spectrum_db: Vec::new(),
                sample_rate: 2.048e6,
                center_freq: 100e6,
            },
        });
        assert!(result.is_ok());
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with("frequency_hz,power_dbfs\n"));
        cleanup(&path);
    }

    #[test]
    fn png_spectrum_export() {
        let path = temp_path("test_spectrum.png");
        let spectrum: Vec<f32> = (0..2048)
            .map(|i| -80.0 + 40.0 * (i as f32 * 0.01).sin())
            .collect();
        let result = export_to_file(&ExportConfig {
            path: path.clone(),
            format: ExportFormat::Png,
            content: ExportContent::Spectrum {
                spectrum_db: spectrum,
                sample_rate: 2.048e6,
                center_freq: 100e6,
            },
        });
        assert!(result.is_ok());
        // Verify it's a valid PNG
        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[..4], &[0x89, b'P', b'N', b'G']);
        cleanup(&path);
    }

    #[test]
    fn png_waterfall_export() {
        let path = temp_path("test_waterfall.png");
        let rows: Vec<Vec<f32>> = (0..50)
            .map(|r| {
                (0..256)
                    .map(|i| -80.0 + 30.0 * ((i as f32 + r as f32) * 0.05).sin())
                    .collect()
            })
            .collect();
        let result = export_to_file(&ExportConfig {
            path: path.clone(),
            format: ExportFormat::Png,
            content: ExportContent::Waterfall {
                rows,
                sample_rate: 2.048e6,
                center_freq: 100e6,
            },
        });
        assert!(result.is_ok());
        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[..4], &[0x89, b'P', b'N', b'G']);
        cleanup(&path);
    }

    #[test]
    fn png_empty_spectrum_returns_error() {
        let path = temp_path("test_empty.png");
        let result = export_to_file(&ExportConfig {
            path: path.clone(),
            format: ExportFormat::Png,
            content: ExportContent::Spectrum {
                spectrum_db: Vec::new(),
                sample_rate: 2.048e6,
                center_freq: 100e6,
            },
        });
        assert!(result.is_err());
    }

    #[test]
    fn export_decoded_messages_csv() {
        let path = temp_path("test_messages.csv");
        let msgs = vec![DecodedMessageExport {
            decoder: "pocsag".to_string(),
            elapsed_ms: 1000,
            summary: "Test message with, comma".to_string(),
            fields: BTreeMap::new(),
        }];
        let result = export_to_file(&ExportConfig {
            path: path.clone(),
            format: ExportFormat::Csv,
            content: ExportContent::DecodedMessages(msgs),
        });
        assert!(result.is_ok());
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("elapsed_ms,decoder,summary"));
        cleanup(&path);
    }
}
