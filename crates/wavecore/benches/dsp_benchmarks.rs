//! Criterion benchmarks for wavecore DSP functions.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use num_complex::Complex;

use wavecore::dsp::detection::{CfarConfig, cfar_detect};
use wavecore::dsp::demod::am::{AmDemod, AmMode};
use wavecore::dsp::demod::fm::{FmDemod, FmMode};
use wavecore::dsp::demod::Demodulator;
use wavecore::dsp::fft::SpectrumAnalyzer;
use wavecore::dsp::filter_design::FirFilter;
use wavecore::dsp::preprocess::DcRemover;
use wavecore::dsp::statistics::signal_statistics;
use wavecore::types::Sample;

/// Generate a test signal: complex sinusoid at `freq_hz` with given sample rate.
fn test_signal(num_samples: usize, freq_hz: f64, sample_rate: f64) -> Vec<Sample> {
    (0..num_samples)
        .map(|i| {
            let t = i as f64 / sample_rate;
            let phase = 2.0 * std::f64::consts::PI * freq_hz * t;
            Complex::new(phase.cos() as f32, phase.sin() as f32)
        })
        .collect()
}

fn bench_fft(c: &mut Criterion) {
    let mut group = c.benchmark_group("fft");
    for &size in &[512, 1024, 2048, 4096, 8192] {
        let signal = test_signal(size, 1000.0, 48000.0);
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &signal, |b, signal| {
            let mut analyzer = SpectrumAnalyzer::new(size).unwrap();
            b.iter(|| analyzer.compute_spectrum(signal));
        });
    }
    group.finish();
}

fn bench_cfar(c: &mut Criterion) {
    let mut group = c.benchmark_group("cfar");
    let config = CfarConfig::default();
    for &size in &[512, 1024, 2048, 4096] {
        // Generate linear-power spectrum (not dB)
        let signal = test_signal(size, 1000.0, 48000.0);
        let mut analyzer = SpectrumAnalyzer::new(size).unwrap();
        let spectrum_db = analyzer.compute_spectrum(&signal);
        let spectrum_linear: Vec<f32> = spectrum_db
            .iter()
            .map(|&db| 10.0f32.powf(db / 10.0))
            .collect();

        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &spectrum_linear,
            |b, spectrum| {
                b.iter(|| cfar_detect(spectrum, &config, 48000.0));
            },
        );
    }
    group.finish();
}

fn bench_demod_fm(c: &mut Criterion) {
    let mut group = c.benchmark_group("demod_fm");
    for &size in &[1024, 4096, 16384, 65536] {
        let signal = test_signal(size, 1000.0, 48000.0);
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &signal, |b, signal| {
            let mut demod = FmDemod::new(FmMode::Narrow, 48000.0, 75.0);
            b.iter(|| demod.process(signal));
        });
    }
    group.finish();
}

fn bench_demod_am(c: &mut Criterion) {
    let mut group = c.benchmark_group("demod_am");
    for &size in &[1024, 4096, 16384] {
        let signal = test_signal(size, 1000.0, 16000.0);
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &signal, |b, signal| {
            let mut demod = AmDemod::new(AmMode::Envelope, 16000.0, 5000.0);
            b.iter(|| demod.process(signal));
        });
    }
    group.finish();
}

fn bench_statistics(c: &mut Criterion) {
    let mut group = c.benchmark_group("statistics");
    for &size in &[1024, 4096, 16384, 65536] {
        let signal = test_signal(size, 1000.0, 48000.0);
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &signal, |b, signal| {
            b.iter(|| signal_statistics(signal));
        });
    }
    group.finish();
}

fn bench_fir_filter(c: &mut Criterion) {
    let mut group = c.benchmark_group("fir_filter");
    let signal = test_signal(4096, 1000.0, 48000.0);
    for &num_taps in &[31, 63, 127, 255] {
        // Simple lowpass-like coefficients
        let coeffs: Vec<f64> = (0..num_taps)
            .map(|i| {
                let n = i as f64 - (num_taps - 1) as f64 / 2.0;
                if n == 0.0 {
                    0.25
                } else {
                    (0.25 * std::f64::consts::PI * n).sin() / (std::f64::consts::PI * n)
                }
            })
            .collect();

        group.throughput(Throughput::Elements(4096));
        group.bench_with_input(
            BenchmarkId::new("taps", num_taps),
            &coeffs,
            |b, coeffs| {
                let mut filter = FirFilter::new(coeffs);
                b.iter(|| filter.process_block(&signal));
            },
        );
    }
    group.finish();
}

fn bench_dc_removal(c: &mut Criterion) {
    let mut group = c.benchmark_group("dc_removal");
    for &size in &[1024, 4096, 16384, 65536] {
        let signal = test_signal(size, 1000.0, 48000.0);
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &signal, |b, signal| {
            let mut dc = DcRemover::new(0.999);
            b.iter(|| {
                let mut buf = signal.clone();
                dc.process(&mut buf);
                buf
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_fft,
    bench_cfar,
    bench_demod_fm,
    bench_demod_am,
    bench_statistics,
    bench_fir_filter,
    bench_dc_removal,
);
criterion_main!(benches);
