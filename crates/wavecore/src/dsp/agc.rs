//! Automatic Gain Control (AGC)
//!
//! Two AGC architectures:
//! 1. **Feedback AGC**: Classic loop with logarithmic detector, attack/decay
//!    time constants, and hang timer for pump-free operation.
//! 2. **Feed-forward AGC**: Look-ahead design with block power measurement
//!    and delay line for zero-attack-time transient response.

use crate::types::Sample;

// ============================================================================
// Feedback AGC
// ============================================================================

/// Feedback AGC with logarithmic detector and asymmetric attack/decay.
///
/// The gain update is performed in the log domain for multiplicative smoothing:
///   g\[n\] = g\[n−1\] + α · (target\_dB − power\_dB − g\[n−1\])
///
/// where α = α\_attack when signal increases (fast response) and
/// α = α\_decay when signal decreases (slow release, preventing pumping).
///
/// The hang timer holds the gain constant for a configurable period after
/// the signal drops, preventing the gain from rising into noise between
/// speech syllables or between data bursts.
pub struct Agc {
    /// Current gain in dB
    gain_db: f64,
    /// Target output power in dBFS
    target_db: f64,
    /// Attack coefficient: α_a = 1 − exp(−1/(τ_a · fs))
    alpha_attack: f64,
    /// Decay coefficient: α_d = 1 − exp(−1/(τ_d · fs))
    alpha_decay: f64,
    /// Maximum gain to prevent noise amplification (dB)
    max_gain_db: f64,
    /// Minimum gain (dB)
    min_gain_db: f64,
    /// Hang timer: hold gain for N samples after signal drops
    hang_samples: usize,
    /// Counter for hang timer
    hang_counter: usize,
    /// Previous power measurement for hang detection
    prev_power_db: f64,
    /// Smoothed power estimate (dB)
    power_smooth_db: f64,
    /// Power smoothing coefficient
    alpha_power: f64,
}

impl Agc {
    /// Create a feedback AGC.
    ///
    /// `target_power_dbfs`: desired output power level in dBFS (e.g., −20.0)
    /// `attack_time_s`: time to reach 63% of step increase (e.g., 0.001)
    /// `decay_time_s`: time to reach 63% of step decrease (e.g., 0.1)
    /// `sample_rate`: input sample rate in Hz
    pub fn new(
        target_power_dbfs: f64,
        attack_time_s: f64,
        decay_time_s: f64,
        sample_rate: f64,
    ) -> Self {
        // Time constant to IIR coefficient: α = 1 − e^{−1/(τ·fs)}
        let alpha_attack = 1.0 - (-1.0 / (attack_time_s * sample_rate)).exp();
        let alpha_decay = 1.0 - (-1.0 / (decay_time_s * sample_rate)).exp();
        let alpha_power = 1.0 - (-1.0 / (0.001 * sample_rate)).exp(); // 1ms power smoothing

        // Hang time: ~50ms
        let hang_samples = (0.05 * sample_rate) as usize;

        Self {
            gain_db: 0.0,
            target_db: target_power_dbfs,
            alpha_attack,
            alpha_decay,
            max_gain_db: 60.0,
            min_gain_db: -40.0,
            hang_samples,
            hang_counter: 0,
            prev_power_db: -100.0,
            power_smooth_db: -100.0,
            alpha_power,
        }
    }

    /// Set maximum gain limit (dB).
    pub fn set_max_gain(&mut self, max_db: f64) {
        self.max_gain_db = max_db;
    }

    /// Set hang time in seconds.
    pub fn set_hang_time(&mut self, seconds: f64, sample_rate: f64) {
        self.hang_samples = (seconds * sample_rate) as usize;
    }

    /// Process a block of IQ samples in-place, applying automatic gain control.
    pub fn process(&mut self, samples: &mut [Sample]) {
        for sample in samples.iter_mut() {
            // Instantaneous power in dB
            let power = sample.norm_sqr();
            let power_db = if power > 1e-20 {
                10.0 * (power as f64).log10()
            } else {
                -100.0
            };

            // Smooth power estimate (exponential moving average in dB domain)
            self.power_smooth_db += self.alpha_power * (power_db - self.power_smooth_db);

            // Determine if signal is increasing or decreasing
            let error = self.target_db - self.power_smooth_db - self.gain_db;

            if error < 0.0 {
                // Signal too strong: fast attack (reduce gain quickly)
                self.gain_db += self.alpha_attack * error;
                self.hang_counter = self.hang_samples; // Reset hang timer
            } else if self.hang_counter > 0 {
                // In hang period: hold gain constant
                self.hang_counter -= 1;
            } else {
                // Signal too weak: slow decay (increase gain slowly)
                self.gain_db += self.alpha_decay * error;
            }

            // Clamp gain
            self.gain_db = self.gain_db.clamp(self.min_gain_db, self.max_gain_db);

            // Apply gain (convert dB to linear)
            let gain_linear = 10.0f64.powf(self.gain_db / 20.0) as f32;
            *sample *= gain_linear;

            self.prev_power_db = power_db;
        }
    }

    /// Current gain in dB.
    pub fn gain_db(&self) -> f64 {
        self.gain_db
    }

    /// Current smoothed input power in dBFS.
    pub fn input_power_db(&self) -> f64 {
        self.power_smooth_db
    }

    /// Reset AGC state.
    pub fn reset(&mut self) {
        self.gain_db = 0.0;
        self.hang_counter = 0;
        self.prev_power_db = -100.0;
        self.power_smooth_db = -100.0;
    }
}

// ============================================================================
// Feed-Forward AGC
// ============================================================================

/// Feed-forward AGC with look-ahead for zero attack delay.
///
/// Measures input power over a block, computes the required gain, then
/// applies it after a delay equal to the block size. This eliminates
/// the attack-time tradeoff of feedback AGC: transients are handled
/// perfectly because the gain is computed before the signal arrives.
///
/// The delay is the cost: latency = block\_size samples.
pub struct AgcFeedForward {
    target_db: f64,
    max_gain_db: f64,
    min_gain_db: f64,
    /// Delay line for look-ahead
    delay_line: Vec<Sample>,
    delay_pos: usize,
    /// Block size for power measurement
    block_size: usize,
    /// Current block accumulator
    block_accum: f64,
    block_count: usize,
    /// Current applied gain (dB)
    current_gain_db: f64,
    /// Gain smoothing coefficient
    alpha_smooth: f64,
}

impl AgcFeedForward {
    /// Create a feed-forward AGC.
    ///
    /// `target_power_dbfs`: desired output level (dBFS)
    /// `block_size`: look-ahead block size in samples (latency = block_size)
    /// `sample_rate`: sample rate in Hz
    pub fn new(target_power_dbfs: f64, block_size: usize, sample_rate: f64) -> Self {
        let block_size = block_size.max(16);
        // Smoothing across blocks to prevent abrupt gain changes
        let blocks_per_second = sample_rate / block_size as f64;
        let alpha_smooth = 1.0 - (-1.0 / (0.01 * blocks_per_second)).exp(); // 10ms smooth

        Self {
            target_db: target_power_dbfs,
            max_gain_db: 60.0,
            min_gain_db: -40.0,
            delay_line: vec![Sample::new(0.0, 0.0); block_size],
            delay_pos: 0,
            block_size,
            block_accum: 0.0,
            block_count: 0,
            current_gain_db: 0.0,
            alpha_smooth,
        }
    }

    /// Process samples, returning gain-controlled output.
    pub fn process(&mut self, input: &[Sample]) -> Vec<Sample> {
        let mut output = Vec::with_capacity(input.len());

        for &sample in input {
            // Read delayed sample and apply current gain
            let delayed = self.delay_line[self.delay_pos];
            let gain_linear = 10.0f64.powf(self.current_gain_db / 20.0) as f32;
            output.push(delayed * gain_linear);

            // Write current sample to delay line
            self.delay_line[self.delay_pos] = sample;
            self.delay_pos = (self.delay_pos + 1) % self.block_size;

            // Accumulate power for the current block
            self.block_accum += sample.norm_sqr() as f64;
            self.block_count += 1;

            // End of block: update gain
            if self.block_count >= self.block_size {
                let avg_power = self.block_accum / self.block_size as f64;
                let power_db = if avg_power > 1e-20 {
                    10.0 * avg_power.log10()
                } else {
                    -100.0
                };

                let desired_gain = self.target_db - power_db;
                let clamped = desired_gain.clamp(self.min_gain_db, self.max_gain_db);

                // Smooth gain transition
                self.current_gain_db += self.alpha_smooth * (clamped - self.current_gain_db);

                self.block_accum = 0.0;
                self.block_count = 0;
            }
        }

        output
    }

    /// Current gain in dB.
    pub fn gain_db(&self) -> f64 {
        self.current_gain_db
    }

    /// Latency in samples.
    pub fn latency(&self) -> usize {
        self.block_size
    }

    pub fn reset(&mut self) {
        self.delay_line.fill(Sample::new(0.0, 0.0));
        self.delay_pos = 0;
        self.block_accum = 0.0;
        self.block_count = 0;
        self.current_gain_db = 0.0;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agc_amplifies_weak_signal() {
        let mut agc = Agc::new(-20.0, 0.001, 0.1, 48000.0);

        // Very weak signal: -60 dBFS
        let amplitude = 0.001f32; // ~-60 dBFS
        let mut samples: Vec<Sample> = (0..48000)
            .map(|i| Sample::new(amplitude * (i as f32 * 0.1).sin(), 0.0))
            .collect();

        agc.process(&mut samples);

        // After convergence, output should be stronger
        let output_power: f32 = samples[40000..].iter().map(|s| s.norm_sqr()).sum::<f32>() / 8000.0;
        let output_db = 10.0 * (output_power as f64).log10();

        assert!(
            agc.gain_db() > 10.0,
            "AGC should increase gain for weak signal: gain = {:.1} dB",
            agc.gain_db()
        );
        assert!(
            output_db > -40.0,
            "Output should be amplified: {output_db:.1} dBFS"
        );
    }

    #[test]
    fn agc_attenuates_strong_signal() {
        let mut agc = Agc::new(-20.0, 0.001, 0.1, 48000.0);

        // Strong signal: ~0 dBFS
        let amplitude = 0.9f32;
        let mut samples: Vec<Sample> = (0..48000)
            .map(|i| Sample::new(amplitude * (i as f32 * 0.1).sin(), 0.0))
            .collect();

        agc.process(&mut samples);

        assert!(
            agc.gain_db() < -5.0,
            "AGC should reduce gain for strong signal: gain = {:.1} dB",
            agc.gain_db()
        );
    }

    #[test]
    fn agc_max_gain_limit() {
        let mut agc = Agc::new(-20.0, 0.001, 0.01, 48000.0);
        agc.set_max_gain(30.0);

        // Silence: AGC should ramp up to max gain but not beyond
        let mut samples = vec![Sample::new(0.0, 0.0); 48000];
        agc.process(&mut samples);

        assert!(
            agc.gain_db() <= 30.0 + 0.1,
            "Gain should be clamped at max: {:.1} dB",
            agc.gain_db()
        );
    }

    #[test]
    fn agc_fast_attack() {
        let mut agc = Agc::new(-20.0, 0.0001, 0.5, 48000.0);

        // Start with weak signal, then sudden loud signal
        let mut samples: Vec<Sample> = (0..48000)
            .map(|i| {
                let amp = if i < 24000 { 0.001 } else { 0.9 };
                Sample::new(amp * (i as f32 * 0.1).sin(), 0.0)
            })
            .collect();

        agc.process(&mut samples);

        // After the loud signal, gain should have dropped significantly
        assert!(
            agc.gain_db() < 0.0,
            "Fast attack should reduce gain: {:.1} dB",
            agc.gain_db()
        );
    }

    #[test]
    fn agc_feedforward_basic() {
        let mut agc = AgcFeedForward::new(-20.0, 256, 48000.0);

        let input: Vec<Sample> = (0..10000)
            .map(|i| Sample::new(0.001 * (i as f32 * 0.1).sin(), 0.0))
            .collect();

        let output = agc.process(&input);
        assert_eq!(output.len(), input.len());

        // Feed-forward should amplify weak signal
        let out_pow: f32 = output[5000..].iter().map(|s| s.norm_sqr()).sum::<f32>() / 5000.0;
        let in_pow: f32 = input[5000..].iter().map(|s| s.norm_sqr()).sum::<f32>() / 5000.0;

        assert!(
            out_pow > in_pow,
            "Feed-forward AGC should amplify: in={in_pow:.6}, out={out_pow:.6}"
        );
    }

    #[test]
    fn agc_feedforward_latency() {
        let agc = AgcFeedForward::new(-20.0, 512, 48000.0);
        assert_eq!(agc.latency(), 512);
    }

    #[test]
    fn agc_reset() {
        let mut agc = Agc::new(-20.0, 0.001, 0.1, 48000.0);

        let mut samples = vec![Sample::new(0.9, 0.0); 48000];
        agc.process(&mut samples);

        assert!(agc.gain_db() < 0.0);

        agc.reset();
        assert!((agc.gain_db()).abs() < 0.01);
    }
}
