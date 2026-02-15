use crate::types::Sample;

/// DC offset removal using exponential moving average.
///
/// Implements the first-order IIR highpass:
///   y[n] = x[n] - dc[n]
///   dc[n] = α·x[n] + (1-α)·dc[n-1]
///
/// where α controls the adaptation rate. Smaller α tracks slower DC changes
/// but removes the DC component more cleanly. The -3dB cutoff of this
/// highpass is at f = -fs/(2π) · ln(1-α) ≈ α·fs/(2π) for small α.
///
/// Typical values: α = 0.001 (~325 Hz cutoff at 2.048 MS/s)
pub struct DcRemover {
    alpha: f32,
    dc_i: f32,
    dc_q: f32,
}

impl DcRemover {
    /// Create a new DC remover.
    ///
    /// `alpha`: adaptation rate. 0.001 is a good default.
    /// Lower values → slower tracking, cleaner removal.
    pub fn new(alpha: f32) -> Self {
        Self {
            alpha,
            dc_i: 0.0,
            dc_q: 0.0,
        }
    }

    /// Design DC remover from desired cutoff frequency.
    ///
    /// `cutoff_hz`: -3 dB frequency of the highpass
    /// `sample_rate`: sample rate in Hz
    pub fn from_cutoff(cutoff_hz: f64, sample_rate: f64) -> Self {
        // α = 1 - exp(-2π·fc/fs)
        let alpha = 1.0 - (-2.0 * std::f64::consts::PI * cutoff_hz / sample_rate).exp();
        Self::new(alpha as f32)
    }

    /// Process a block of samples in-place, removing DC offset.
    pub fn process(&mut self, samples: &mut [Sample]) {
        for s in samples.iter_mut() {
            self.dc_i += self.alpha * (s.re - self.dc_i);
            self.dc_q += self.alpha * (s.im - self.dc_q);
            s.re -= self.dc_i;
            s.im -= self.dc_q;
        }
    }

    /// Process a block, returning new samples (non-mutating).
    pub fn process_copy(&mut self, samples: &[Sample]) -> Vec<Sample> {
        let mut out = samples.to_vec();
        self.process(&mut out);
        out
    }

    /// Current estimated DC offset.
    pub fn dc_estimate(&self) -> Sample {
        Sample::new(self.dc_i, self.dc_q)
    }

    /// Reset the DC estimate.
    pub fn reset(&mut self) {
        self.dc_i = 0.0;
        self.dc_q = 0.0;
    }
}

/// IQ imbalance correction using Gram-Schmidt orthogonalization.
///
/// Real SDR hardware produces imperfect IQ signals with:
/// - Amplitude imbalance: |Q| ≠ |I| (gain mismatch between channels)
/// - Phase imbalance: ∠(I,Q) ≠ 90° (quadrature error)
///
/// This manifests as an image at -f for a signal at +f, with the image
/// level depending on the imbalance magnitude.
///
/// The Gram-Schmidt method estimates and corrects both simultaneously:
///   I_corr = I
///   Q_corr = (Q - μ·I) / √(1 - μ²)
///
/// where μ = E[I·Q] / E[I²] estimates the correlation between I and Q
/// (which should be zero for perfect quadrature).
///
/// The correction is adaptive, continuously tracking slow parameter drift.
pub struct IqCorrector {
    /// Correlation estimate: E[I·Q]
    mu: f32,
    /// I power estimate: E[I²]
    power_i: f32,
    /// Q power estimate: E[Q²]
    power_q: f32,
    /// Adaptation rate
    alpha: f32,
    /// Residual correlation after correction (for IRR reporting)
    residual_mu: f32,
}

impl IqCorrector {
    /// Create a new IQ imbalance corrector.
    ///
    /// `alpha`: adaptation rate (0.001 typical). Lower = slower but more accurate.
    pub fn new(alpha: f32) -> Self {
        Self {
            mu: 0.0,
            power_i: 1.0,
            power_q: 1.0,
            alpha,
            residual_mu: 0.0,
        }
    }

    /// Process a block of samples in-place, correcting IQ imbalance.
    pub fn process(&mut self, samples: &mut [Sample]) {
        for s in samples.iter_mut() {
            // Update I power and cross-correlation estimates from input
            self.power_i += self.alpha * (s.re * s.re - self.power_i);
            self.power_q += self.alpha * (s.im * s.im - self.power_q);
            let iq_corr = s.re * s.im;
            self.mu += self.alpha * (iq_corr / self.power_i.max(1e-20) - self.mu);

            // Gram-Schmidt correction: remove I-Q correlation
            let q_corrected = s.im - self.mu * s.re;

            // Amplitude balance: scale corrected Q to match I power
            // Corrected Q power = E[Q^2] - mu^2 * E[I^2] (variance after decorrelation)
            let corrected_q_power = (self.power_q - self.mu * self.mu * self.power_i).max(1e-20);
            let gain_correction = (self.power_i / corrected_q_power).sqrt();
            s.im = q_corrected * gain_correction;

            // Track residual correlation of corrected output for IRR reporting
            let residual_corr = s.re * s.im;
            self.residual_mu +=
                self.alpha * (residual_corr / self.power_i.max(1e-20) - self.residual_mu);
        }
    }

    /// Image rejection ratio in dB (higher = better correction).
    ///
    /// Based on the residual I-Q correlation after correction.
    /// IRR = -20·log10(|μ_residual|) approximately.
    /// Typical uncorrected: 25-40 dB. After correction: >60 dB.
    pub fn image_rejection_db(&self) -> f32 {
        if self.residual_mu.abs() < 1e-20 {
            200.0
        } else {
            -20.0 * self.residual_mu.abs().log10()
        }
    }

    /// Current amplitude imbalance in dB.
    pub fn amplitude_imbalance_db(&self) -> f32 {
        10.0 * (self.power_q / self.power_i.max(1e-20)).log10()
    }

    /// Current phase imbalance in degrees.
    pub fn phase_imbalance_deg(&self) -> f32 {
        self.mu.asin().to_degrees()
    }
}

/// Complete preprocessing chain applied to raw IQ samples.
///
/// Order matters:
/// 1. DC removal (removes hardware DC offset)
/// 2. IQ correction (fixes amplitude/phase imbalance)
pub struct Preprocessor {
    pub dc_remover: DcRemover,
    pub iq_corrector: IqCorrector,
    dc_removal_enabled: bool,
    iq_correction_enabled: bool,
}

impl Preprocessor {
    /// Create a preprocessor with default settings.
    ///
    /// DC removal: enabled, α=0.001
    /// IQ correction: enabled, α=0.0005
    pub fn new(sample_rate: f64) -> Self {
        Self {
            dc_remover: DcRemover::from_cutoff(100.0, sample_rate),
            iq_corrector: IqCorrector::new(0.0005),
            dc_removal_enabled: true,
            iq_correction_enabled: true,
        }
    }

    /// Process a block of samples through the full preprocessing chain.
    pub fn process(&mut self, samples: &mut [Sample]) {
        if self.dc_removal_enabled {
            self.dc_remover.process(samples);
        }
        if self.iq_correction_enabled {
            self.iq_corrector.process(samples);
        }
    }

    pub fn set_dc_removal(&mut self, enabled: bool) {
        self.dc_removal_enabled = enabled;
    }

    pub fn set_iq_correction(&mut self, enabled: bool) {
        self.iq_correction_enabled = enabled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_remover_removes_offset() {
        let mut dc = DcRemover::new(0.01);
        // Samples with DC offset of 0.5
        let mut samples: Vec<Sample> = (0..10000).map(|_| Sample::new(0.5, -0.3)).collect();

        dc.process(&mut samples);

        // After convergence, samples near end should be near zero
        let last = &samples[9000..];
        let mean_re: f32 = last.iter().map(|s| s.re).sum::<f32>() / last.len() as f32;
        let mean_im: f32 = last.iter().map(|s| s.im).sum::<f32>() / last.len() as f32;
        assert!(
            mean_re.abs() < 0.05,
            "DC not removed on I: mean = {mean_re}"
        );
        assert!(
            mean_im.abs() < 0.05,
            "DC not removed on Q: mean = {mean_im}"
        );
    }

    #[test]
    fn dc_remover_preserves_signal() {
        let mut dc = DcRemover::new(0.001);
        let freq = 10000.0; // Well above DC cutoff
        let fs = 2_048_000.0;

        let mut samples: Vec<Sample> = (0..10000)
            .map(|i| {
                let t = i as f32 / fs as f32;
                let phase = 2.0 * std::f32::consts::PI * freq as f32 * t;
                Sample::new(phase.cos() + 0.5, phase.sin() - 0.3) // Signal + DC
            })
            .collect();

        dc.process(&mut samples);

        // Signal power should be preserved (after initial transient)
        let signal_power: f32 = samples[5000..].iter().map(|s| s.norm_sqr()).sum::<f32>() / 5000.0;
        // Unit complex sinusoid has power 1.0
        assert!(
            (signal_power - 1.0).abs() < 0.1,
            "Signal distorted: power = {signal_power}"
        );
    }

    #[test]
    fn iq_corrector_reduces_imbalance() {
        let mut iq = IqCorrector::new(0.005);

        // Simulate IQ imbalance: 2 dB amplitude, 5° phase
        let gain_imbalance = 10.0f32.powf(2.0 / 20.0); // 2 dB
        let phase_error = 5.0f32.to_radians();

        let mut samples: Vec<Sample> = (0..50000)
            .map(|i| {
                let t = i as f32 / 2_048_000.0;
                let phase = 2.0 * std::f32::consts::PI * 100_000.0 * t;
                let i_ch = phase.cos();
                let q_ch = gain_imbalance * (phase + phase_error).sin();
                Sample::new(i_ch, q_ch)
            })
            .collect();

        iq.process(&mut samples);

        // After correction, power ratio should be closer to 1
        let irr = iq.image_rejection_db();
        assert!(irr > 30.0, "IRR should be >30 dB, got {irr}");
    }

    #[test]
    fn preprocessor_chain() {
        let mut pp = Preprocessor::new(2_048_000.0);
        // Need enough samples for the DC remover to converge.
        // With cutoff=100 Hz at fs=2.048 MHz, alpha ≈ 3e-4, so
        // the time constant is ~3260 samples. Use 20000 samples
        // (~6 time constants) for solid convergence.
        let mut samples: Vec<Sample> = (0..20000).map(|_| Sample::new(0.7, -0.2)).collect();

        pp.process(&mut samples);

        // After convergence, DC should be well removed
        let tail = &samples[18000..];
        let mean: f32 = tail.iter().map(|s| s.re).sum::<f32>() / tail.len() as f32;
        assert!(mean.abs() < 0.1, "DC not removed: mean = {mean}");
    }
}
