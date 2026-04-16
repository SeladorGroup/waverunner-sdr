//! Waterfall display widget using Unicode Braille characters.
//!
//! Each braille character (U+2800 to U+28FF) encodes a 2×4 dot matrix,
//! giving an effective resolution of 2·W × 4·H pixels within a W×H
//! character grid. This is significantly higher than block elements.
//!
//! ## Braille dot numbering and bit encoding
//!
//! The Unicode braille pattern maps dots to bits as follows:
//!
//! ```text
//!   ╔═══╦═══╗
//!   ║ 1 ║ 4 ║   bit 0  bit 3
//!   ║ 2 ║ 5 ║   bit 1  bit 4
//!   ║ 3 ║ 6 ║   bit 2  bit 5
//!   ║ 7 ║ 8 ║   bit 6  bit 7
//!   ╚═══╩═══╝
//! ```
//!
//! Character = U+2800 + dot_bits
//!
//! ## Amplitude-to-dot mapping
//!
//! Within each 2×4 cell, we threshold the power level at 4 quantization
//! levels. Each row of the 4-row column lights up if the normalized
//! amplitude exceeds the threshold for that row:
//!
//!   dot_on = (amplitude > row_threshold)
//!
//! where row_threshold ∈ {0.0, 0.25, 0.5, 0.75} from bottom to top.
//! This gives a density-based rendering where brighter = more dots lit.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

/// Bit positions for braille dots.
/// Left column (col 0): dots 1,2,3,7 → bits 0,1,2,6
/// Right column (col 1): dots 4,5,6,8 → bits 3,4,5,7
const BRAILLE_DOT_BITS: [[u8; 4]; 2] = [
    [0, 1, 2, 6], // Left column, rows 0-3 (top to bottom)
    [3, 4, 5, 7], // Right column, rows 0-3 (top to bottom)
];

/// Base codepoint for braille patterns.
const BRAILLE_BASE: u32 = 0x2800;

/// Color palette for waterfall — maps normalized power to color.
/// Uses a "turbo"-inspired colormap for maximum perceptual resolution.
///
/// The colormap is designed to be perceptually uniform: equal dB steps
/// produce roughly equal perceived color change. Based on the observation
/// that human color discrimination peaks around green-yellow.
fn waterfall_color(normalized: f32) -> Color {
    let n = normalized.clamp(0.0, 1.0);
    if n < 0.1 {
        // Black → deep blue
        let t = n / 0.1;
        Color::Rgb(0, 0, (t * 80.0) as u8)
    } else if n < 0.3 {
        // Deep blue → blue
        let t = (n - 0.1) / 0.2;
        Color::Rgb(0, 0, 80 + (t * 175.0) as u8)
    } else if n < 0.5 {
        // Blue → cyan → green
        let t = (n - 0.3) / 0.2;
        Color::Rgb(0, (t * 255.0) as u8, (255.0 * (1.0 - t)) as u8)
    } else if n < 0.7 {
        // Green → yellow
        let t = (n - 0.5) / 0.2;
        Color::Rgb((t * 255.0) as u8, 255, 0)
    } else if n < 0.9 {
        // Yellow → orange → red
        let t = (n - 0.7) / 0.2;
        Color::Rgb(255, (255.0 * (1.0 - t)) as u8, 0)
    } else {
        // Red → white (hot)
        let t = (n - 0.9) / 0.1;
        Color::Rgb(255, (t * 255.0) as u8, (t * 255.0) as u8)
    }
}

/// Waterfall display widget.
///
/// Takes a 2D array of spectrum rows (time × frequency) and renders
/// them using braille characters. Each terminal character cell represents
/// 2 frequency bins × 4 time rows.
pub struct WaterfallWidget<'a> {
    /// Waterfall rows in chronological order (oldest first).
    /// Each row is a power spectrum in dBFS.
    pub rows: &'a [&'a [f32]],
    /// Minimum dB value (maps to no dots / dark).
    pub min_db: f32,
    /// Maximum dB value (maps to all dots / bright).
    pub max_db: f32,
}

impl<'a> Widget for WaterfallWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 2 || area.height < 1 || self.rows.is_empty() {
            return;
        }

        let char_cols = area.width as usize;
        let char_rows = area.height as usize;
        let db_range = self.max_db - self.min_db;
        if db_range <= 0.0 {
            return;
        }

        // Each character covers 2 frequency bins and 4 time rows
        let time_rows_needed = char_rows * 4;
        let freq_bins_needed = char_cols * 2;

        // Take the most recent rows that fit
        let available = self.rows.len();
        let start_row = available.saturating_sub(time_rows_needed);
        let rows = &self.rows[start_row..];

        for cy in 0..char_rows {
            for cx in 0..char_cols {
                let mut dot_bits: u8 = 0;
                let mut total_power: f32 = 0.0;
                let mut count: u32 = 0;

                // Iterate over the 2×4 dot matrix within this character
                for (dot_col, col_bits) in BRAILLE_DOT_BITS.iter().enumerate() {
                    for (dot_row, &bit_pos) in col_bits.iter().enumerate() {
                        let time_idx = cy * 4 + dot_row;
                        let freq_idx = cx * 2 + dot_col;

                        // Get the power value for this pixel
                        let power = if time_idx < rows.len() {
                            let row = rows[time_idx];
                            if row.is_empty() {
                                -100.0
                            } else {
                                // Map freq_idx to actual bin
                                let bin = freq_idx * row.len() / freq_bins_needed;
                                if bin < row.len() { row[bin] } else { -100.0 }
                            }
                        } else {
                            -100.0
                        };

                        let normalized = ((power - self.min_db) / db_range).clamp(0.0, 1.0);
                        total_power += normalized;
                        count += 1;

                        // Threshold: light the dot if power exceeds 25%
                        // This gives a density effect — stronger signals fill more dots
                        if normalized > 0.25 {
                            dot_bits |= 1 << bit_pos;
                        }
                    }
                }

                // Color based on average power in this cell
                let avg_power = if count > 0 {
                    total_power / count as f32
                } else {
                    0.0
                };
                let color = waterfall_color(avg_power);
                let ch = char::from_u32(BRAILLE_BASE + dot_bits as u32).unwrap_or(' ');

                buf.set_string(
                    area.x + cx as u16,
                    area.y + cy as u16,
                    ch.to_string(),
                    Style::default().fg(color),
                );
            }
        }
    }
}
