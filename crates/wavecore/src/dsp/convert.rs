use crate::types::Sample;

/// Convert signed 16-bit interleaved IQ to Sample.
///
/// Used by devices like HackRF and USRP that produce i16 IQ data.
pub fn i16_iq_to_samples(raw: &[i16]) -> Vec<Sample> {
    debug_assert!(raw.len() % 2 == 0, "Raw IQ data must have even length");
    raw.chunks_exact(2)
        .map(|pair| {
            Sample::new(
                pair[0] as f32 / i16::MAX as f32,
                pair[1] as f32 / i16::MAX as f32,
            )
        })
        .collect()
}

/// Convert interleaved f32 IQ to Sample.
///
/// Used by devices that produce float IQ data natively.
pub fn f32_iq_to_samples(raw: &[f32]) -> Vec<Sample> {
    debug_assert!(raw.len() % 2 == 0, "Raw IQ data must have even length");
    raw.chunks_exact(2)
        .map(|pair| Sample::new(pair[0], pair[1]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i16_conversion() {
        let raw = [i16::MAX, i16::MIN, 0, 0];
        let samples = i16_iq_to_samples(&raw);
        assert_eq!(samples.len(), 2);
        assert!((samples[0].re - 1.0).abs() < 0.001);
        assert!((samples[0].im - (-1.0)).abs() < 0.001);
        assert!(samples[1].re.abs() < 0.001);
        assert!(samples[1].im.abs() < 0.001);
    }

    #[test]
    fn f32_conversion() {
        let raw = [0.5f32, -0.5, 1.0, 0.0];
        let samples = f32_iq_to_samples(&raw);
        assert_eq!(samples.len(), 2);
        assert!((samples[0].re - 0.5).abs() < 1e-6);
        assert!((samples[0].im - (-0.5)).abs() < 1e-6);
    }
}
