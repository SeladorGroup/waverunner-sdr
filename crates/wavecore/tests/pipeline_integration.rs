use wavecore::buffer::{PipelineConfig, sample_pipeline};
use wavecore::dsp::fft::SpectrumAnalyzer;
use wavecore::dsp::power::{noise_floor, peak_frequency, rms_power_dbfs};
use wavecore::types::{Sample, SampleBlock};

use std::f32::consts::PI;

/// Full pipeline test: synthetic samples → pipeline → spectrum analysis
#[test]
fn synthetic_signal_through_pipeline() {
    let (producer, consumer) = sample_pipeline(PipelineConfig::default());
    let fft_size = 1024;
    let sample_rate = 2_048_000.0;

    // Generate a tone at 100 kHz offset from center
    let tone_freq = 100_000.0f32;
    let num_samples = fft_size * 4; // 4 FFT windows worth

    let samples: Vec<Sample> = (0..num_samples)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            let phase = 2.0 * PI * tone_freq * t;
            Sample::new(phase.cos() * 0.5, phase.sin() * 0.5)
        })
        .collect();

    // Push through pipeline in chunks
    for (seq, chunk) in samples.chunks(fft_size).enumerate() {
        let block = SampleBlock {
            samples: chunk.to_vec(),
            center_freq: 433.92e6,
            sample_rate,
            sequence: seq as u64,
            timestamp_ns: 0,
        };
        producer.send(block).unwrap();
    }

    // Consume and analyze
    let mut analyzer = SpectrumAnalyzer::new(fft_size).unwrap();
    let mut block_count = 0;

    while let Some(block) = consumer.try_recv() {
        let rms = rms_power_dbfs(&block.samples);
        let spectrum = analyzer.compute_spectrum(&block.samples);
        let (peak_offset, peak_power) = peak_frequency(&spectrum, sample_rate);
        let floor = noise_floor(&spectrum);

        // RMS of amplitude 0.5 complex sinusoid = 0.25 power = -6 dBFS
        assert!(
            (rms - (-6.0)).abs() < 1.0,
            "Expected RMS ~-6 dBFS, got {rms}"
        );

        // Peak should be near 100 kHz offset
        assert!(
            (peak_offset - 100_000.0).abs() < 5_000.0,
            "Expected peak near 100 kHz offset, got {peak_offset}"
        );

        // Peak should be well above noise floor
        assert!(
            peak_power - floor > 20.0,
            "Peak ({peak_power:.1}) should be >20 dB above floor ({floor:.1})"
        );

        block_count += 1;
    }

    assert_eq!(block_count, 4);
}

/// Test that the pipeline handles empty blocks gracefully
#[test]
fn empty_block_handling() {
    let mut analyzer = SpectrumAnalyzer::new(256).unwrap();
    let spectrum = analyzer.compute_spectrum(&[]);
    assert_eq!(spectrum.len(), 256);
    // All values should be at the floor (-200 dBFS)
    for val in &spectrum {
        assert!(*val <= -100.0);
    }
}
