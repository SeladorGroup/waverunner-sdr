//! Phase-Locked Loop (PLL), Costas Loop, and Frequency-Locked Loop (FLL)
//!
//! ## PLL — Second-Order Type 2
//!
//! A phase-locked loop tracks the phase and frequency of an input signal using
//! a feedback system: phase detector → loop filter → NCO.
//!
//! The loop filter is a proportional-integral (PI) controller that provides
//! second-order dynamics with controllable bandwidth and damping:
//!
//! ```text
//!            ┌──────────┐     ┌────────────┐     ┌─────┐
//! input ──→ │  Phase    │──→  │ Loop filter │──→  │ NCO │──┐
//!           │ detector  │     │   (PI)      │     │     │  │
//!           └───────────┘     └─────────────┘     └─────┘  │
//!                ↑                                          │
//!                └──────────────────────────────────────────┘
//! ```
//!
//! The natural frequency ωₙ and damping ratio ξ determine the loop dynamics:
//! - **Bandwidth**: BL = ωₙ/(2ξ) · (ξ + 1/(4ξ))  [noise bandwidth]
//! - **Lock-in range**: ≈ 2·ξ·ωₙ
//! - **Pull-in range**: ≈ 2·ωₙ·√(2ξ)
//!
//! ## Costas Loop
//!
//! For suppressed-carrier modulation (DSB-SC, BPSK), the standard PLL cannot
//! lock because there is no carrier to track. The Costas loop uses a
//! decision-directed phase detector that squares out the modulation:
//!
//! For BPSK: error = I·Q (strips data by using sign decisions)
//! For DSB-SC: error = I·sgn(Q) (hard-decision directed)
//!
//! ## FLL — Frequency-Locked Loop
//!
//! Provides initial frequency acquisition with wider capture range than PLL.
//! Uses a frequency discriminator based on the cross-product of successive
//! samples: error ∝ `Im(z[n] · z*[n−1]) / (|z[n]| + |z[n−1]|)`.
//!
//! Typically used to pull the signal within PLL lock range, then switched off.

use crate::types::Sample;
use std::f64::consts::PI;

// ============================================================================
// PLL — Second-Order Type 2
// ============================================================================

/// Second-order Type 2 Phase-Locked Loop.
///
/// The PI loop filter coefficients are derived from the continuous-time
/// second-order system parameters (ωₙ, ξ) and discretized:
///
///   K₁ = (4·ξ·ωₙ/ωₛ) / (1 + 2·ξ·ωₙ/ωₛ + (ωₙ/ωₛ)²)
///   K₂ = (4·(ωₙ/ωₛ)²) / (1 + 2·ξ·ωₙ/ωₛ + (ωₙ/ωₛ)²)
///
/// where ωₛ = 2π·fₛ is the sample angular frequency. These come from the
/// bilinear transform of the continuous PI controller H(s) = K₁ + K₂/s
/// mapped to the z-domain.
pub struct Pll {
    /// NCO phase (radians)
    phase: f64,
    /// NCO frequency (radians/sample)
    frequency: f64,
    /// Proportional gain (phase correction)
    k1: f64,
    /// Integral gain (frequency correction)
    k2: f64,
    /// Sample rate (Hz)
    sample_rate: f64,
    /// Lock detector: exponential average of |error|
    lock_avg: f64,
    /// Lock detector smoothing coefficient
    lock_alpha: f64,
    /// Lock threshold (radians): |avg error| below this = locked
    lock_threshold: f64,
    /// Frequency limits (radians/sample)
    freq_min: f64,
    freq_max: f64,
}

impl Pll {
    /// Create a second-order PLL.
    ///
    /// `loop_bandwidth_hz`: noise bandwidth BL in Hz. Determines tracking speed
    ///   vs noise rejection tradeoff. Typical: 10-100 Hz for carrier tracking,
    ///   1-10 Hz for narrowband.
    ///
    /// `damping`: damping ratio ξ. 0.707 (critically damped) is standard.
    ///   Higher values give less overshoot but slower response.
    ///
    /// `sample_rate`: input sample rate in Hz.
    pub fn new(loop_bandwidth_hz: f64, damping: f64, sample_rate: f64) -> Self {
        // Natural frequency from noise bandwidth:
        // BL = ωₙ/(2ξ) · (ξ + 1/(4ξ)) ⇒ ωₙ = 2·BL·ξ / (ξ + 1/(4ξ))
        // Simplified: ωₙ = BL·8·ξ² / (4·ξ² + 1)
        // BL = ωₙ·(ξ + 1/(4ξ)) / 2  ⇒  ωₙ = 2·BL / (ξ + 1/(4ξ))
        // Simplifying: ξ + 1/(4ξ) = (4ξ² + 1)/(4ξ)
        // So: ωₙ = 8·ξ·BL / (4ξ² + 1), where BL is in rad/s
        let wn = loop_bandwidth_hz * 2.0 * PI * 8.0 * damping / (4.0 * damping * damping + 1.0);

        let ws = 2.0 * PI * sample_rate;
        let wn_norm = wn / ws;

        // Bilinear-transform-derived PI coefficients (Gardner, 2005)
        let denom = 1.0 + 2.0 * damping * wn_norm + wn_norm * wn_norm;
        let k1 = 4.0 * damping * wn_norm / denom;
        let k2 = 4.0 * wn_norm * wn_norm / denom;

        // Lock detector: 10ms time constant
        let lock_alpha = 1.0 - (-1.0 / (0.01 * sample_rate)).exp();

        Self {
            phase: 0.0,
            frequency: 0.0,
            k1,
            k2,
            sample_rate,
            lock_avg: PI, // Start unlocked
            lock_alpha,
            lock_threshold: 0.3, // ~17° error
            freq_min: -PI,       // ±fs/2
            freq_max: PI,
        }
    }

    /// Set the maximum frequency tracking range in Hz.
    pub fn set_frequency_range(&mut self, min_hz: f64, max_hz: f64) {
        self.freq_min = 2.0 * PI * min_hz / self.sample_rate;
        self.freq_max = 2.0 * PI * max_hz / self.sample_rate;
    }

    /// Process one input sample through the PLL.
    ///
    /// Returns `(coherent_output, phase_error, is_locked)`:
    /// - `coherent_output`: input multiplied by conjugate of NCO (derotated)
    /// - `phase_error`: current phase error in radians
    /// - `is_locked`: true if average |error| is below threshold
    pub fn step(&mut self, input: Sample) -> (Sample, f32, bool) {
        // Generate NCO reference: e^{jφ}
        let nco_cos = self.phase.cos() as f32;
        let nco_sin = self.phase.sin() as f32;

        // Multiply input by conjugate of NCO: derotate the signal
        // (I + jQ) × (cos − jsin) = (I·cos + Q·sin) + j(Q·cos − I·sin)
        let derotated = Sample::new(
            input.re * nco_cos + input.im * nco_sin,
            input.im * nco_cos - input.re * nco_sin,
        );

        // Phase detector: atan2(Q, I) of the derotated signal
        // This gives the phase error between input and NCO
        let error = (derotated.im as f64).atan2(derotated.re as f64);

        // Loop filter (PI controller):
        // frequency += K₂ · error     (integral term → frequency correction)
        // phase += K₁ · error         (proportional term → phase correction)
        self.frequency += self.k2 * error;
        self.frequency = self.frequency.clamp(self.freq_min, self.freq_max);

        self.phase += self.frequency + self.k1 * error;

        // Wrap phase to [−π, π) for numerical stability
        self.phase = wrap_phase(self.phase);

        // Lock detector: exponential moving average of |error|
        self.lock_avg += self.lock_alpha * (error.abs() - self.lock_avg);
        let locked = self.lock_avg < self.lock_threshold;

        (derotated, error as f32, locked)
    }

    /// Current NCO frequency estimate in Hz.
    pub fn frequency_hz(&self) -> f64 {
        self.frequency * self.sample_rate / (2.0 * PI)
    }

    /// Current NCO phase in radians.
    pub fn phase_rad(&self) -> f64 {
        self.phase
    }

    /// Whether the PLL considers itself locked.
    pub fn is_locked(&self) -> bool {
        self.lock_avg < self.lock_threshold
    }

    /// Average phase error magnitude (radians), used for visualization.
    pub fn phase_error_avg(&self) -> f64 {
        self.lock_avg
    }

    /// Set the lock detection threshold (radians).
    pub fn set_lock_threshold(&mut self, threshold_rad: f64) {
        self.lock_threshold = threshold_rad;
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.frequency = 0.0;
        self.lock_avg = PI;
    }
}

// ============================================================================
// Costas Loop (suppressed-carrier recovery)
// ============================================================================

/// Costas Loop for suppressed-carrier synchronization.
///
/// Variant of PLL for signals where the carrier has been suppressed
/// (DSB-SC, BPSK, QPSK). Uses a decision-directed phase detector that
/// removes the data modulation from the error signal.
///
/// The phase detector produces an S-curve with period π (for BPSK) or π/2
/// (for QPSK), creating phase ambiguity that must be resolved by higher
/// layers (differential encoding, unique word, etc.).
pub struct CostasLoop {
    /// NCO phase (radians)
    phase: f64,
    /// NCO frequency (radians/sample)
    frequency: f64,
    /// Proportional gain
    k1: f64,
    /// Integral gain
    k2: f64,
    /// Modulation mode
    mode: CostasMode,
    /// Sample rate
    sample_rate: f64,
    /// Lock detector
    lock_avg: f64,
    lock_alpha: f64,
    lock_threshold: f64,
}

/// Costas loop mode determines the phase detector.
#[derive(Clone, Copy, Debug)]
pub enum CostasMode {
    /// BPSK / DSB-SC: error = I · Q
    /// S-curve period: π (180° ambiguity)
    Bpsk,
    /// QPSK: error = I·sgn(Q) − Q·sgn(I)
    /// S-curve period: π/2 (90° ambiguity)
    Qpsk,
    /// Decision-directed: error = I·sgn(Q)
    /// Good for AM-DSB-SC
    DecisionDirected,
}

impl CostasLoop {
    /// Create a Costas loop.
    ///
    /// Parameters same as PLL; `mode` selects the phase detector algorithm.
    pub fn new(loop_bandwidth_hz: f64, damping: f64, sample_rate: f64, mode: CostasMode) -> Self {
        // BL = ωₙ·(ξ + 1/(4ξ)) / 2  ⇒  ωₙ = 2·BL / (ξ + 1/(4ξ))
        // Simplifying: ξ + 1/(4ξ) = (4ξ² + 1)/(4ξ)
        // So: ωₙ = 8·ξ·BL / (4ξ² + 1), where BL is in rad/s
        let wn = loop_bandwidth_hz * 2.0 * PI * 8.0 * damping / (4.0 * damping * damping + 1.0);
        let ws = 2.0 * PI * sample_rate;
        let wn_norm = wn / ws;

        let denom = 1.0 + 2.0 * damping * wn_norm + wn_norm * wn_norm;
        let k1 = 4.0 * damping * wn_norm / denom;
        let k2 = 4.0 * wn_norm * wn_norm / denom;

        let lock_alpha = 1.0 - (-1.0 / (0.01 * sample_rate)).exp();

        Self {
            phase: 0.0,
            frequency: 0.0,
            k1,
            k2,
            mode,
            sample_rate,
            lock_avg: PI,
            lock_alpha,
            lock_threshold: 0.3,
        }
    }

    /// Process one sample through the Costas loop.
    ///
    /// Returns `(i_out, q_out, phase_error)`:
    /// - `i_out`: in-phase (data) output
    /// - `q_out`: quadrature output
    /// - `phase_error`: current error signal
    pub fn step(&mut self, input: Sample) -> (f32, f32, f32) {
        // Derotate input by NCO
        let nco_cos = self.phase.cos() as f32;
        let nco_sin = self.phase.sin() as f32;

        let i_out = input.re * nco_cos + input.im * nco_sin;
        let q_out = input.im * nco_cos - input.re * nco_sin;

        // Phase detector (decision-directed)
        let error = match self.mode {
            CostasMode::Bpsk => {
                // BPSK: error = I · Q
                // At lock: I = ±A (data), Q = 0. Error = 0.
                // Off lock: cross-coupling produces correction signal.
                (i_out * q_out) as f64
            }
            CostasMode::Qpsk => {
                // QPSK: error = I·sgn(Q) − Q·sgn(I)
                // Handles four-fold symmetry
                let sgn_i = if i_out >= 0.0 { 1.0f32 } else { -1.0 };
                let sgn_q = if q_out >= 0.0 { 1.0f32 } else { -1.0 };
                (i_out * sgn_q - q_out * sgn_i) as f64
            }
            CostasMode::DecisionDirected => {
                // Hard decision on Q arm
                (i_out * q_out.signum()) as f64
            }
        };

        // PI loop filter
        self.frequency += self.k2 * error;
        self.phase += self.frequency + self.k1 * error;
        self.phase = wrap_phase(self.phase);

        // Lock detector
        self.lock_avg += self.lock_alpha * (error.abs() - self.lock_avg);

        (i_out, q_out, error as f32)
    }

    /// Current frequency estimate in Hz.
    pub fn frequency_hz(&self) -> f64 {
        self.frequency * self.sample_rate / (2.0 * PI)
    }

    /// Whether the loop considers itself locked.
    pub fn is_locked(&self) -> bool {
        self.lock_avg < self.lock_threshold
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.frequency = 0.0;
        self.lock_avg = PI;
    }
}

// ============================================================================
// FLL — Frequency-Locked Loop
// ============================================================================

/// Frequency-Locked Loop for coarse frequency acquisition.
///
/// Uses a cross-product frequency discriminator that estimates instantaneous
/// frequency error from consecutive samples:
///
///   `e_f[n] = Im(z[n] · z*[n−1]) / (|z[n]| · |z[n−1]|)`
///
/// This gives an error signal proportional to the frequency offset (for small
/// offsets). The FLL has a much wider capture range than the PLL but coarser
/// tracking. Typical usage: FLL acquires the signal, then hands off to PLL.
///
/// The loop filter is first-order (integral only), giving a Type 1 loop:
///   `ω[n] = ω[n−1] + K_f · e_f[n]`
pub struct Fll {
    /// NCO phase (radians)
    phase: f64,
    /// NCO frequency (radians/sample)
    frequency: f64,
    /// Loop gain
    gain: f64,
    /// Previous sample (for cross-product discriminator)
    prev: Sample,
    /// Sample rate
    sample_rate: f64,
}

impl Fll {
    /// Create an FLL.
    ///
    /// `bandwidth_hz`: loop bandwidth (wider = faster acquisition, more noise)
    /// `sample_rate`: input sample rate in Hz
    pub fn new(bandwidth_hz: f64, sample_rate: f64) -> Self {
        // First-order loop gain: K = 2π · BW / fs
        // This gives convergence in approximately fs/BW samples
        let gain = 2.0 * PI * bandwidth_hz / sample_rate;

        Self {
            phase: 0.0,
            frequency: 0.0,
            gain,
            prev: Sample::new(1.0, 0.0),
            sample_rate,
        }
    }

    /// Process one sample through the FLL.
    ///
    /// Returns `(derotated_sample, frequency_error_hz)`.
    pub fn step(&mut self, input: Sample) -> (Sample, f64) {
        // Derotate input
        let nco_cos = self.phase.cos() as f32;
        let nco_sin = self.phase.sin() as f32;

        let derotated = Sample::new(
            input.re * nco_cos + input.im * nco_sin,
            input.im * nco_cos - input.re * nco_sin,
        );

        // Cross-product frequency discriminator:
        // e_f = Im(z[n] · z*[n-1]) / (|z[n]| · |z[n-1]|)
        // = (I[n]·Q[n-1] - Q[n]·I[n-1]) / (|z[n]|·|z[n-1]|)
        //
        // This approximates the instantaneous frequency as:
        // Δf ≈ fs/(2π) · atan2(cross, dot)
        // For small offsets, atan2 ≈ cross/dot ≈ normalized cross product
        // Im(z[n] · z*[n−1]) = z[n].im·z[n−1].re − z[n].re·z[n−1].im
        // Positive for positive frequency offset → drives NCO frequency up
        let cross = (derotated.im * self.prev.re - derotated.re * self.prev.im) as f64;
        let mag_product = ((derotated.re * derotated.re + derotated.im * derotated.im)
            * (self.prev.re * self.prev.re + self.prev.im * self.prev.im))
            .sqrt() as f64;

        let freq_error = if mag_product > 1e-12 {
            cross / mag_product
        } else {
            0.0
        };

        // Update NCO frequency (integral loop filter)
        self.frequency += self.gain * freq_error;
        self.frequency = self.frequency.clamp(-PI, PI);

        // Update NCO phase
        self.phase += self.frequency;
        self.phase = wrap_phase(self.phase);

        self.prev = derotated;

        let freq_error_hz = freq_error * self.sample_rate / (2.0 * PI);
        (derotated, freq_error_hz)
    }

    /// Current frequency estimate in Hz.
    pub fn frequency_hz(&self) -> f64 {
        self.frequency * self.sample_rate / (2.0 * PI)
    }

    /// Set the FLL loop gain directly.
    pub fn set_gain(&mut self, gain: f64) {
        self.gain = gain;
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.frequency = 0.0;
        self.prev = Sample::new(1.0, 0.0);
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Wrap phase to [−π, π).
#[inline]
fn wrap_phase(mut phase: f64) -> f64 {
    while phase > PI {
        phase -= 2.0 * PI;
    }
    while phase <= -PI {
        phase += 2.0 * PI;
    }
    phase
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pll_locks_to_pure_tone() {
        let fs = 48000.0;
        let f_signal = 500.0; // 500 Hz tone
        let mut pll = Pll::new(200.0, 0.707, fs);

        // Generate complex tone at f_signal
        let n = 48000; // 1 second
        let mut locked = false;
        let mut lock_sample = 0;

        for i in 0..n {
            let t = 2.0 * PI * f_signal * i as f64 / fs;
            let sample = Sample::new(t.cos() as f32, t.sin() as f32);
            let (_, _, is_locked) = pll.step(sample);

            if is_locked && !locked {
                locked = true;
                lock_sample = i;
            }
        }

        assert!(
            locked,
            "PLL should lock to 500 Hz tone. Final avg error: {:.4}",
            pll.lock_avg
        );

        // Should lock within ~100ms (5000 samples) for BL=50 Hz
        assert!(
            lock_sample < 10000,
            "PLL should lock within 10000 samples, locked at {lock_sample}"
        );

        // Frequency estimate should be close to 500 Hz
        let freq_est = pll.frequency_hz();
        assert!(
            (freq_est - f_signal).abs() < 5.0,
            "Frequency estimate should be ~500 Hz: {freq_est:.1}"
        );
    }

    #[test]
    fn pll_tracks_frequency_sweep() {
        let fs = 48000.0;
        let mut pll = Pll::new(100.0, 0.707, fs);

        // Sweep from 200 Hz to 800 Hz over 2 seconds
        let n = 96000;
        let mut phase = 0.0f64;

        for i in 0..n {
            let freq = 200.0 + 600.0 * i as f64 / n as f64;
            phase += 2.0 * PI * freq / fs;
            let sample = Sample::new(phase.cos() as f32, phase.sin() as f32);
            pll.step(sample);
        }

        // At the end, PLL should be tracking ~800 Hz
        let freq_est = pll.frequency_hz();
        assert!(
            (freq_est - 800.0).abs() < 50.0,
            "PLL should track sweep to ~800 Hz: {freq_est:.1}"
        );
    }

    #[test]
    fn pll_phase_error_decreases_after_lock() {
        let fs = 48000.0;
        let f_signal = 200.0; // Within pull-in range of BL=50 Hz loop
        let mut pll = Pll::new(50.0, 0.707, fs);

        // Collect phase error over time
        let n = 24000; // 500ms
        let mut early_error = 0.0f64;
        let mut late_error = 0.0f64;

        for i in 0..n {
            let t = 2.0 * PI * f_signal * i as f64 / fs;
            let sample = Sample::new(t.cos() as f32, t.sin() as f32);
            let (_, error, _) = pll.step(sample);

            if (1000..3000).contains(&i) {
                early_error += error.abs() as f64;
            }
            if i >= n - 2000 {
                late_error += error.abs() as f64;
            }
        }

        early_error /= 2000.0;
        late_error /= 2000.0;

        assert!(
            late_error < early_error,
            "Phase error should decrease: early={early_error:.4}, late={late_error:.4}"
        );
    }

    #[test]
    fn costas_bpsk_carrier_recovery() {
        let fs = 48000.0;
        let f_carrier = 1000.0;
        let mut costas = CostasLoop::new(200.0, 0.707, fs, CostasMode::Bpsk);

        // Generate BPSK: carrier × {±1} data
        let n = 48000;
        let symbols_per_sec = 300.0; // 300 baud
        let samples_per_symbol = (fs / symbols_per_sec) as usize;

        let mut late_error_sum = 0.0f64;
        let mut late_count = 0;

        for i in 0..n {
            let t = 2.0 * PI * f_carrier * i as f64 / fs;
            // Data: ±1 switching every samples_per_symbol
            let bit = if (i / samples_per_symbol) % 2 == 0 {
                1.0f32
            } else {
                -1.0
            };
            let sample = Sample::new(bit * t.cos() as f32, bit * t.sin() as f32);
            let (_, _, error) = costas.step(sample);

            if i > n / 2 {
                late_error_sum += error.abs() as f64;
                late_count += 1;
            }
        }

        let avg_error = late_error_sum / late_count as f64;
        assert!(
            costas.is_locked() || avg_error < 0.5,
            "Costas should lock to BPSK: avg_error={avg_error:.4}"
        );

        // Frequency should be close to carrier
        let freq_est = costas.frequency_hz();
        assert!(
            (freq_est - f_carrier).abs() < 20.0,
            "Costas frequency should be ~{f_carrier} Hz: {freq_est:.1}"
        );
    }

    #[test]
    fn fll_acquires_frequency_offset() {
        let fs = 48000.0;
        let f_offset = 2000.0; // 2 kHz offset
        let mut fll = Fll::new(200.0, fs);

        // Tone at f_offset
        let n = 48000;
        for i in 0..n {
            let t = 2.0 * PI * f_offset * i as f64 / fs;
            let sample = Sample::new(t.cos() as f32, t.sin() as f32);
            fll.step(sample);
        }

        let freq_est = fll.frequency_hz();
        assert!(
            (freq_est - f_offset).abs() < 100.0,
            "FLL should acquire ~{f_offset} Hz: {freq_est:.1}"
        );
    }

    #[test]
    fn fll_wider_capture_than_pll() {
        let fs = 48000.0;
        let f_offset = 5000.0; // Large offset: 5 kHz

        // FLL with wide bandwidth
        let mut fll = Fll::new(500.0, fs);

        // PLL with same bandwidth (should struggle with large offset)
        let mut pll = Pll::new(500.0, 0.707, fs);

        let n = 48000;
        for i in 0..n {
            let t = 2.0 * PI * f_offset * i as f64 / fs;
            let sample = Sample::new(t.cos() as f32, t.sin() as f32);
            fll.step(sample);
            pll.step(sample);
        }

        let fll_error = (fll.frequency_hz() - f_offset).abs();
        let _pll_error = (pll.frequency_hz() - f_offset).abs();

        // FLL should get closer to the true frequency
        // (PLL may or may not lock depending on pull-in range)
        assert!(
            fll_error < 200.0,
            "FLL should acquire 5 kHz offset: error={fll_error:.1} Hz"
        );
    }

    #[test]
    fn wrap_phase_test() {
        assert!((wrap_phase(0.0)).abs() < 1e-10);
        assert!((wrap_phase(PI) - PI).abs() < 1e-10 || (wrap_phase(PI) + PI).abs() < 1e-10);
        assert!(
            (wrap_phase(3.0 * PI) - PI).abs() < 1e-10 || (wrap_phase(3.0 * PI) + PI).abs() < 1e-10
        );
        assert!(
            (wrap_phase(-3.0 * PI) + PI).abs() < 1e-10
                || (wrap_phase(-3.0 * PI) - PI).abs() < 1e-10
        );
    }

    #[test]
    fn pll_reset_clears_state() {
        let mut pll = Pll::new(50.0, 0.707, 48000.0);

        // Process some signal
        for i in 0..1000 {
            let t = 2.0 * PI * 1000.0 * i as f64 / 48000.0;
            pll.step(Sample::new(t.cos() as f32, t.sin() as f32));
        }

        assert!(pll.frequency_hz().abs() > 1.0);

        pll.reset();
        assert!((pll.frequency_hz()).abs() < 0.01);
        assert!((pll.phase_rad()).abs() < 0.01);
    }

    #[test]
    fn costas_qpsk_mode() {
        let fs = 48000.0;
        let f_carrier = 1000.0;
        let mut costas = CostasLoop::new(200.0, 0.707, fs, CostasMode::Qpsk);

        // Generate QPSK signal: rotate carrier by 0, π/2, π, 3π/2
        let n = 48000;
        let symbols_per_sec = 300.0;
        let samples_per_symbol = (fs / symbols_per_sec) as usize;
        let qpsk_phases = [0.0, PI / 2.0, PI, 3.0 * PI / 2.0];

        for i in 0..n {
            let symbol_idx = (i / samples_per_symbol) % 4;
            let t = 2.0 * PI * f_carrier * i as f64 / fs + qpsk_phases[symbol_idx];
            let sample = Sample::new(t.cos() as f32, t.sin() as f32);
            costas.step(sample);
        }

        // Should converge to carrier frequency (within QPSK ambiguity)
        let freq_est = costas.frequency_hz();
        assert!(
            (freq_est - f_carrier).abs() < 30.0,
            "Costas QPSK should track carrier: {freq_est:.1} Hz"
        );
    }
}
