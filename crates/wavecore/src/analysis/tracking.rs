//! Time-series tracking for signal parameters.
//!
//! `SignalTracker` accumulates SNR, power, noise floor, frequency offset,
//! and spectral flatness over time using fixed-size circular buffers.
//! `push()` is O(1) zero-allocation, safe for the real-time processing loop.

/// Fixed-size circular buffer for time-series data.
#[derive(Debug, Clone)]
pub struct TimeSeriesBuffer {
    data: Vec<f32>,
    timestamps: Vec<f64>,
    write_idx: usize,
    count: usize,
    capacity: usize,
}

impl TimeSeriesBuffer {
    /// Create a new buffer with given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![0.0; capacity],
            timestamps: vec![0.0; capacity],
            write_idx: 0,
            count: 0,
            capacity,
        }
    }

    /// Push a value. O(1), no allocations.
    pub fn push(&mut self, value: f32, timestamp: f64) {
        self.data[self.write_idx] = value;
        self.timestamps[self.write_idx] = timestamp;
        self.write_idx = (self.write_idx + 1) % self.capacity;
        if self.count < self.capacity {
            self.count += 1;
        }
    }

    /// Number of values stored.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Extract time-series as (timestamp, value) pairs in chronological order.
    pub fn to_vec(&self) -> Vec<(f64, f32)> {
        if self.count == 0 {
            return Vec::new();
        }
        let mut result = Vec::with_capacity(self.count);
        let start = if self.count < self.capacity {
            0
        } else {
            self.write_idx
        };
        for i in 0..self.count {
            let idx = (start + i) % self.capacity;
            result.push((self.timestamps[idx], self.data[idx]));
        }
        result
    }

    /// Compute mean of stored values.
    pub fn mean(&self) -> f32 {
        if self.count == 0 {
            return 0.0;
        }
        let sum: f64 = if self.count < self.capacity {
            self.data[..self.count].iter().map(|&v| v as f64).sum()
        } else {
            self.data.iter().map(|&v| v as f64).sum()
        };
        (sum / self.count as f64) as f32
    }

    /// Compute min of stored values.
    pub fn min(&self) -> f32 {
        if self.count == 0 {
            return 0.0;
        }
        if self.count < self.capacity {
            self.data[..self.count]
                .iter()
                .cloned()
                .fold(f32::INFINITY, f32::min)
        } else {
            self.data.iter().cloned().fold(f32::INFINITY, f32::min)
        }
    }

    /// Compute max of stored values.
    pub fn max(&self) -> f32 {
        if self.count == 0 {
            return 0.0;
        }
        if self.count < self.capacity {
            self.data[..self.count]
                .iter()
                .cloned()
                .fold(f32::NEG_INFINITY, f32::max)
        } else {
            self.data.iter().cloned().fold(f32::NEG_INFINITY, f32::max)
        }
    }
}

/// Tracks multiple signal parameters over time.
#[derive(Debug, Clone)]
pub struct SignalTracker {
    pub snr_history: TimeSeriesBuffer,
    pub power_history: TimeSeriesBuffer,
    pub noise_floor_history: TimeSeriesBuffer,
    pub freq_offset_history: TimeSeriesBuffer,
    pub spectral_flatness_history: TimeSeriesBuffer,
    /// Total blocks tracked.
    pub sample_count: u64,
}

impl SignalTracker {
    /// Create a new tracker with given history capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            snr_history: TimeSeriesBuffer::new(capacity),
            power_history: TimeSeriesBuffer::new(capacity),
            noise_floor_history: TimeSeriesBuffer::new(capacity),
            freq_offset_history: TimeSeriesBuffer::new(capacity),
            spectral_flatness_history: TimeSeriesBuffer::new(capacity),
            sample_count: 0,
        }
    }

    /// Push one set of measurements. O(1), zero allocations.
    pub fn push(
        &mut self,
        snr: f32,
        power: f32,
        noise_floor: f32,
        freq_offset: f32,
        spectral_flatness: f32,
        elapsed_secs: f64,
    ) {
        self.snr_history.push(snr, elapsed_secs);
        self.power_history.push(power, elapsed_secs);
        self.noise_floor_history.push(noise_floor, elapsed_secs);
        self.freq_offset_history.push(freq_offset, elapsed_secs);
        self.spectral_flatness_history.push(spectral_flatness, elapsed_secs);
        self.sample_count += 1;
    }

    /// Produce a snapshot for UI consumption.
    pub fn snapshot(&self) -> TrackingSnapshot {
        let snr = self.snr_history.to_vec();
        let power = self.power_history.to_vec();
        let noise_floor = self.noise_floor_history.to_vec();
        let freq_offset = self.freq_offset_history.to_vec();

        // Compute frequency drift via linear regression on freq_offset
        let drift = linear_regression_slope(&freq_offset);

        // Stability score: based on variance of SNR (low variance = stable)
        let snr_variance = if self.snr_history.len() > 1 {
            let mean = self.snr_history.mean();
            let var: f64 = self.snr_history.to_vec().iter()
                .map(|(_, v)| (*v as f64 - mean as f64).powi(2))
                .sum::<f64>() / self.snr_history.len() as f64;
            var.sqrt() as f32
        } else {
            0.0
        };
        // Map variance to 0-1 score: 0 dB std → 1.0, >10 dB std → 0.0
        let stability = (1.0 - snr_variance / 10.0).clamp(0.0, 1.0);

        let duration = if let (Some(first), Some(last)) = (snr.first(), snr.last()) {
            last.0 - first.0
        } else {
            0.0
        };

        TrackingSnapshot {
            snr,
            power,
            noise_floor,
            freq_offset,
            summary: TrackingSummary {
                duration_secs: duration,
                snr_mean: self.snr_history.mean(),
                snr_min: self.snr_history.min(),
                snr_max: self.snr_history.max(),
                power_mean: self.power_history.mean(),
                freq_drift_hz_per_sec: drift,
                stability_score: stability,
            },
        }
    }
}

/// Snapshot of tracked data for UI display.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TrackingSnapshot {
    /// SNR over time (seconds, dB).
    pub snr: Vec<(f64, f32)>,
    /// RMS power over time (seconds, dBFS).
    pub power: Vec<(f64, f32)>,
    /// Noise floor over time (seconds, dB).
    pub noise_floor: Vec<(f64, f32)>,
    /// Frequency offset over time (seconds, Hz).
    pub freq_offset: Vec<(f64, f32)>,
    /// Statistics summary.
    pub summary: TrackingSummary,
}

/// Summary statistics from tracking data.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TrackingSummary {
    /// Total tracking duration in seconds.
    pub duration_secs: f64,
    /// Mean SNR in dB.
    pub snr_mean: f32,
    /// Minimum SNR in dB.
    pub snr_min: f32,
    /// Maximum SNR in dB.
    pub snr_max: f32,
    /// Mean power in dBFS.
    pub power_mean: f32,
    /// Frequency drift rate in Hz/sec (linear regression slope).
    pub freq_drift_hz_per_sec: f64,
    /// Stability score (0.0 = unstable, 1.0 = rock solid).
    pub stability_score: f32,
}

/// Linear regression slope on (x, y) pairs.
fn linear_regression_slope(data: &[(f64, f32)]) -> f64 {
    if data.len() < 2 {
        return 0.0;
    }
    let n = data.len() as f64;
    let sum_x: f64 = data.iter().map(|(x, _)| x).sum();
    let sum_y: f64 = data.iter().map(|(_, y)| *y as f64).sum();
    let sum_xy: f64 = data.iter().map(|(x, y)| x * *y as f64).sum();
    let sum_x2: f64 = data.iter().map(|(x, _)| x * x).sum();

    let denom = n * sum_x2 - sum_x * sum_x;
    if denom.abs() < 1e-12 {
        return 0.0;
    }
    (n * sum_xy - sum_x * sum_y) / denom
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_push_and_snapshot() {
        let mut tracker = SignalTracker::new(100);
        for i in 0..10 {
            tracker.push(15.0, -40.0, -55.0, 0.0, 0.5, i as f64);
        }
        let snap = tracker.snapshot();
        assert_eq!(snap.snr.len(), 10);
        assert!((snap.summary.snr_mean - 15.0).abs() < 0.01);
        assert_eq!(tracker.sample_count, 10);
    }

    #[test]
    fn tracker_circular_buffer_wrap() {
        let mut buf = TimeSeriesBuffer::new(5);
        for i in 0..10 {
            buf.push(i as f32, i as f64);
        }
        assert_eq!(buf.len(), 5);
        let v = buf.to_vec();
        assert_eq!(v.len(), 5);
        // Should contain the last 5 values: 5,6,7,8,9
        assert_eq!(v[0], (5.0, 5.0));
        assert_eq!(v[4], (9.0, 9.0));
    }

    #[test]
    fn tracker_frequency_drift_linear() {
        let mut tracker = SignalTracker::new(100);
        // 10 Hz/sec drift
        for i in 0..100 {
            let t = i as f64 * 0.1;
            tracker.push(15.0, -40.0, -55.0, (t * 10.0) as f32, 0.5, t);
        }
        let snap = tracker.snapshot();
        assert!(
            (snap.summary.freq_drift_hz_per_sec - 10.0).abs() < 0.5,
            "Expected ~10 Hz/s drift, got {}",
            snap.summary.freq_drift_hz_per_sec
        );
    }

    #[test]
    fn tracker_stability_score() {
        // Stable signal: constant SNR → score near 1.0
        let mut stable = SignalTracker::new(100);
        for i in 0..50 {
            stable.push(20.0, -30.0, -50.0, 0.0, 0.5, i as f64);
        }
        let snap = stable.snapshot();
        assert!(snap.summary.stability_score > 0.9, "Stable signal score should be high, got {}", snap.summary.stability_score);

        // Unstable signal: wildly varying SNR → score near 0.0
        let mut unstable = SignalTracker::new(100);
        for i in 0..50 {
            let snr = if i % 2 == 0 { 30.0 } else { 0.0 };
            unstable.push(snr, -30.0, -50.0, 0.0, 0.5, i as f64);
        }
        let snap = unstable.snapshot();
        assert!(snap.summary.stability_score < 0.1, "Unstable signal score should be low, got {}", snap.summary.stability_score);
    }

    #[test]
    fn tracker_empty() {
        let tracker = SignalTracker::new(100);
        let snap = tracker.snapshot();
        assert!(snap.snr.is_empty());
        assert_eq!(snap.summary.duration_secs, 0.0);
    }

    #[test]
    fn tracker_summary_statistics() {
        let mut tracker = SignalTracker::new(100);
        tracker.push(10.0, -40.0, -55.0, 0.0, 0.5, 0.0);
        tracker.push(20.0, -30.0, -50.0, 0.0, 0.5, 1.0);
        tracker.push(30.0, -20.0, -45.0, 0.0, 0.5, 2.0);

        let snap = tracker.snapshot();
        assert!((snap.summary.snr_mean - 20.0).abs() < 0.01);
        assert!((snap.summary.snr_min - 10.0).abs() < 0.01);
        assert!((snap.summary.snr_max - 30.0).abs() < 0.01);
        assert!((snap.summary.duration_secs - 2.0).abs() < 0.01);
    }
}
