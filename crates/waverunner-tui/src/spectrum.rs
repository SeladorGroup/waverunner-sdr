//! Spectrum display widget.
//!
//! Renders a power spectrum as a vertical bar chart using Unicode block
//! characters for sub-cell resolution:
//!
//! ```text
//! █ = 8/8   ▇ = 7/8   ▆ = 6/8   ▅ = 5/8
//! ▄ = 4/8   ▃ = 3/8   ▂ = 2/8   ▁ = 1/8
//! ```
//!
//! Each column maps to one or more FFT bins (decimated by averaging
//! when the FFT size exceeds terminal width). The mapping from dBFS
//! to bar height uses a linear scale between configurable min/max
//! bounds with the transfer function:
//!
//!   height = (dB - min_db) / (max_db - min_db) × total_cells
//!
//! Frequency axis labels are placed at bin boundaries corresponding
//! to the center frequency ± sample_rate/2.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

/// Unicode block elements for 1/8 vertical resolution.
/// Index 0 = empty, 1 = ▁, ..., 8 = █
const BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Color gradient for spectrum magnitude.
/// Maps normalized amplitude [0, 1] → color.
/// Uses a perceptually-motivated gradient: blue → cyan → green → yellow → red.
fn amplitude_color(normalized: f32) -> Color {
    let n = normalized.clamp(0.0, 1.0);
    if n < 0.25 {
        // Blue → Cyan
        let t = n / 0.25;
        Color::Rgb(0, (t * 255.0) as u8, 255)
    } else if n < 0.5 {
        // Cyan → Green
        let t = (n - 0.25) / 0.25;
        Color::Rgb(0, 255, (255.0 * (1.0 - t)) as u8)
    } else if n < 0.75 {
        // Green → Yellow
        let t = (n - 0.5) / 0.25;
        Color::Rgb((t * 255.0) as u8, 255, 0)
    } else {
        // Yellow → Red
        let t = (n - 0.75) / 0.25;
        Color::Rgb(255, (255.0 * (1.0 - t)) as u8, 0)
    }
}

/// Spectrum bar chart widget.
pub struct SpectrumWidget<'a> {
    /// Power spectrum in dBFS (FFT-shifted, DC at center).
    pub spectrum: &'a [f32],
    /// Minimum dB value (bottom of display).
    pub min_db: f32,
    /// Maximum dB value (top of display).
    pub max_db: f32,
    /// Noise floor marker in dB (drawn as horizontal line).
    pub noise_floor: Option<f32>,
    /// Peak hold envelope in dB (drawn as dim markers above spectrum).
    pub peak_hold: Option<&'a [f32]>,
}

impl<'a> Widget for SpectrumWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 4 || area.height < 2 || self.spectrum.is_empty() {
            return;
        }

        let width = area.width as usize;
        let height = area.height as usize;
        let db_range = self.max_db - self.min_db;
        if db_range <= 0.0 {
            return;
        }

        // Bin spectrum down to terminal width by averaging groups of bins.
        // Each column represents a frequency span of sample_rate / width Hz.
        let bins = self.spectrum.len();
        let col_values: Vec<f32> = (0..width)
            .map(|col| {
                let start = col * bins / width;
                let end = ((col + 1) * bins / width).max(start + 1);
                let sum: f32 = self.spectrum[start..end.min(bins)].iter().sum();
                sum / (end - start) as f32
            })
            .collect();

        // Render columns bottom-up using block characters.
        // total_eighths = normalized_height × (height × 8)
        for (col, &db_val) in col_values.iter().enumerate() {
            let normalized = ((db_val - self.min_db) / db_range).clamp(0.0, 1.0);
            let total_eighths = (normalized * (height as f32) * 8.0) as usize;
            let full_cells = total_eighths / 8;
            let remainder = total_eighths % 8;

            let color = amplitude_color(normalized);
            let style = Style::default().fg(color);

            // Fill full cells from bottom
            for row in 0..full_cells.min(height) {
                let y = area.y + (height - 1 - row) as u16;
                let x = area.x + col as u16;
                buf.set_string(x, y, "█", style);
            }

            // Partial cell
            if remainder > 0 && full_cells < height {
                let y = area.y + (height - 1 - full_cells) as u16;
                let x = area.x + col as u16;
                buf.set_string(x, y, BLOCKS[remainder].to_string(), style);
            }
        }

        // Draw peak hold markers
        if let Some(peaks) = self.peak_hold {
            let peak_values: Vec<f32> = (0..width)
                .map(|col| {
                    let start = col * peaks.len() / width;
                    let end = ((col + 1) * peaks.len() / width).max(start + 1);
                    let sum: f32 = peaks[start..end.min(peaks.len())].iter().sum();
                    sum / (end - start) as f32
                })
                .collect();

            for (col, &db_val) in peak_values.iter().enumerate() {
                let normalized = ((db_val - self.min_db) / db_range).clamp(0.0, 1.0);
                let row_from_bottom = (normalized * height as f32) as usize;
                if row_from_bottom > 0 && row_from_bottom <= height {
                    let y = area.y + (height - row_from_bottom) as u16;
                    let x = area.x + col as u16;
                    let cell = buf.cell_mut((x, y));
                    if let Some(cell) = cell {
                        if cell.symbol() == " " {
                            buf.set_string(x, y, "▔", Style::default().fg(Color::DarkGray));
                        }
                    }
                }
            }
        }

        // Draw noise floor line if provided
        if let Some(floor) = self.noise_floor {
            let normalized = ((floor - self.min_db) / db_range).clamp(0.0, 1.0);
            let row_from_bottom = (normalized * height as f32) as usize;
            if row_from_bottom < height {
                let y = area.y + (height - 1 - row_from_bottom) as u16;
                for col in 0..width {
                    let x = area.x + col as u16;
                    let cell = buf.cell_mut((x, y));
                    if let Some(cell) = cell {
                        if cell.symbol() == " " {
                            buf.set_string(x, y, "─", Style::default().fg(Color::DarkGray));
                        }
                    }
                }
            }
        }
    }
}

/// Frequency axis widget — renders labels below the spectrum.
pub struct FreqAxisWidget {
    pub center_freq: f64,
    pub sample_rate: f64,
}

impl Widget for FreqAxisWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 20 || area.height < 1 {
            return;
        }

        let width = area.width as usize;
        let style = Style::default().fg(Color::DarkGray);

        // Place 5 labels evenly across the width
        let num_labels = 5;
        let half_bw = self.sample_rate / 2.0;
        let freq_start = self.center_freq - half_bw;
        let freq_end = self.center_freq + half_bw;

        for i in 0..num_labels {
            let frac = i as f64 / (num_labels - 1) as f64;
            let freq = freq_start + frac * (freq_end - freq_start);
            let label = format_short_freq(freq);
            let col = (frac * (width - label.len()) as f64) as u16;
            buf.set_string(area.x + col, area.y, &label, style);
        }
    }
}

/// Short frequency format for axis labels.
fn format_short_freq(hz: f64) -> String {
    if hz.abs() >= 1e9 {
        format!("{:.3}G", hz / 1e9)
    } else if hz.abs() >= 1e6 {
        format!("{:.3}M", hz / 1e6)
    } else if hz.abs() >= 1e3 {
        format!("{:.1}k", hz / 1e3)
    } else {
        format!("{:.0}", hz)
    }
}
