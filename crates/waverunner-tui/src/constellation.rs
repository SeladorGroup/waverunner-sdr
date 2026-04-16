//! Constellation diagram widget using Unicode Braille characters.
//!
//! Renders an IQ scatter plot where each point represents a complex sample
//! after AGC, showing modulation characteristics:
//! - AM: points along the real axis
//! - FM: ring around the origin (constant envelope)
//! - QAM/PSK: clusters at constellation points
//!
//! Uses the same braille technique as the waterfall for sub-cell resolution.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

/// Braille dot bit positions.
/// Left column (col 0): dots 1,2,3,7 → bits 0,1,2,6
/// Right column (col 1): dots 4,5,6,8 → bits 3,4,5,7
const BRAILLE_DOT_BITS: [[u8; 4]; 2] = [[0, 1, 2, 6], [3, 4, 5, 7]];

const BRAILLE_BASE: u32 = 0x2800;

/// IQ constellation diagram widget.
pub struct ConstellationWidget<'a> {
    /// IQ points as (I, Q) pairs.
    pub points: &'a [(f32, f32)],
    /// Symmetric axis range: display covers [-range, +range] on both axes.
    pub range: f32,
}

impl<'a> Widget for ConstellationWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 4 || area.height < 4 || self.range <= 0.0 {
            return;
        }

        let width = area.width as usize;
        let height = area.height as usize;

        // Braille grid resolution: 2× width, 4× height
        let grid_w = width * 2;
        let grid_h = height * 4;

        // Accumulate braille dot patterns per character cell
        let mut cells = vec![0u8; width * height];

        // Map each IQ point to braille grid coordinates
        for &(i_val, q_val) in self.points {
            // I → x (left to right), Q → y (top to bottom, but Q+ is up)
            let norm_x = (i_val + self.range) / (2.0 * self.range);
            let norm_y = (-q_val + self.range) / (2.0 * self.range); // flip Q axis

            let gx = (norm_x * grid_w as f32) as isize;
            let gy = (norm_y * grid_h as f32) as isize;

            if gx < 0 || gx >= grid_w as isize || gy < 0 || gy >= grid_h as isize {
                continue;
            }

            let gx = gx as usize;
            let gy = gy as usize;

            // Which character cell and which dot within it
            let cx = gx / 2;
            let cy = gy / 4;
            let dot_col = gx % 2;
            let dot_row = gy % 4;

            if cx < width && cy < height {
                cells[cy * width + cx] |= 1 << BRAILLE_DOT_BITS[dot_col][dot_row];
            }
        }

        // Render braille characters
        let point_style = Style::default().fg(Color::Cyan);
        for cy in 0..height {
            for cx in 0..width {
                let bits = cells[cy * width + cx];
                if bits != 0 {
                    let ch = char::from_u32(BRAILLE_BASE + bits as u32).unwrap_or(' ');
                    buf.set_string(
                        area.x + cx as u16,
                        area.y + cy as u16,
                        ch.to_string(),
                        point_style,
                    );
                }
            }
        }

        // Draw axis crosshair
        let center_x = width / 2;
        let center_y = height / 2;

        let axis_style = Style::default().fg(Color::DarkGray);

        // Horizontal axis (I)
        for cx in 0..width {
            let x = area.x + cx as u16;
            let y = area.y + center_y as u16;
            let cell = buf.cell_mut((x, y));
            if let Some(cell) = cell {
                if cell.symbol() == " " {
                    let ch = if cx == center_x { "┼" } else { "─" };
                    buf.set_string(x, y, ch, axis_style);
                }
            }
        }

        // Vertical axis (Q)
        for cy in 0..height {
            let x = area.x + center_x as u16;
            let y = area.y + cy as u16;
            let cell = buf.cell_mut((x, y));
            if let Some(cell) = cell {
                if cell.symbol() == " " || cell.symbol() == "─" {
                    let ch = if cy == center_y { "┼" } else { "│" };
                    buf.set_string(x, y, ch, axis_style);
                }
            }
        }

        // Label axes
        if width > 6 && height > 2 {
            buf.set_string(
                area.x + width as u16 - 2,
                area.y + center_y as u16,
                "I",
                axis_style,
            );
            buf.set_string(area.x + center_x as u16 + 1, area.y, "Q", axis_style);
        }
    }
}
