//! Waterfall renderer — scrolling time-frequency heatmap.
//!
//! Maintains a ring buffer of spectrum rows that scrolls as new data
//! arrives. On the GPU side, this maps to a 2D texture written at a
//! rotating row index, with a fragment shader that applies the
//! scroll offset and colormap lookup.

/// Configuration for the waterfall renderer.
#[derive(Debug, Clone)]
pub struct WaterfallConfig {
    /// Number of history rows to keep.
    pub history_rows: usize,
    /// Minimum dB value (maps to colormap 0.0).
    pub min_db: f32,
    /// Maximum dB value (maps to colormap 1.0).
    pub max_db: f32,
}

impl Default for WaterfallConfig {
    fn default() -> Self {
        Self {
            history_rows: 256,
            min_db: -100.0,
            max_db: 0.0,
        }
    }
}

/// CPU-side waterfall renderer state.
pub struct WaterfallRenderer {
    config: WaterfallConfig,
    /// Ring buffer of spectrum rows.
    rows: Vec<Vec<f32>>,
    /// Current write position in the ring buffer.
    write_pos: usize,
    /// Total rows written (for tracking).
    total_rows: u64,
}

impl WaterfallRenderer {
    /// Create a new waterfall renderer.
    pub fn new(config: WaterfallConfig) -> Self {
        Self {
            rows: Vec::with_capacity(config.history_rows),
            write_pos: 0,
            total_rows: 0,
            config,
        }
    }

    /// Push a new spectrum row into the ring buffer.
    pub fn push_row(&mut self, row: &[f32]) {
        if self.rows.len() < self.config.history_rows {
            self.rows.push(row.to_vec());
        } else {
            self.rows[self.write_pos] = row.to_vec();
        }
        self.write_pos = (self.write_pos + 1) % self.config.history_rows;
        self.total_rows += 1;
    }

    /// Number of rows currently stored.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Get rows in chronological order (oldest first).
    pub fn rows_ordered(&self) -> Vec<&[f32]> {
        let len = self.rows.len();
        if len < self.config.history_rows {
            // Not yet full — return in order
            self.rows.iter().map(|r| r.as_slice()).collect()
        } else {
            // Ring buffer — read from write_pos (oldest) forward
            let mut result = Vec::with_capacity(len);
            for i in 0..len {
                let idx = (self.write_pos + i) % len;
                result.push(self.rows[idx].as_slice());
            }
            result
        }
    }

    /// Get the configuration.
    pub fn config(&self) -> &WaterfallConfig {
        &self.config
    }

    /// Current write position in the ring buffer.
    pub fn write_position(&self) -> usize {
        self.write_pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waterfall_config_default() {
        let config = WaterfallConfig::default();
        assert_eq!(config.history_rows, 256);
    }

    #[test]
    fn waterfall_push_and_count() {
        let mut wf = WaterfallRenderer::new(WaterfallConfig {
            history_rows: 4,
            min_db: -100.0,
            max_db: 0.0,
        });

        assert_eq!(wf.row_count(), 0);
        wf.push_row(&[-50.0, -40.0]);
        assert_eq!(wf.row_count(), 1);
        wf.push_row(&[-45.0, -35.0]);
        assert_eq!(wf.row_count(), 2);
    }

    #[test]
    fn waterfall_ring_buffer_wraps() {
        let mut wf = WaterfallRenderer::new(WaterfallConfig {
            history_rows: 3,
            min_db: -100.0,
            max_db: 0.0,
        });

        wf.push_row(&[1.0]);
        wf.push_row(&[2.0]);
        wf.push_row(&[3.0]);
        wf.push_row(&[4.0]); // Overwrites row 0

        let ordered = wf.rows_ordered();
        assert_eq!(ordered.len(), 3);
        assert_eq!(ordered[0], &[2.0]); // Oldest surviving
        assert_eq!(ordered[1], &[3.0]);
        assert_eq!(ordered[2], &[4.0]); // Newest
    }

    #[test]
    fn waterfall_ordered_before_full() {
        let mut wf = WaterfallRenderer::new(WaterfallConfig {
            history_rows: 10,
            min_db: -100.0,
            max_db: 0.0,
        });

        wf.push_row(&[1.0]);
        wf.push_row(&[2.0]);

        let ordered = wf.rows_ordered();
        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0], &[1.0]);
        assert_eq!(ordered[1], &[2.0]);
    }
}
