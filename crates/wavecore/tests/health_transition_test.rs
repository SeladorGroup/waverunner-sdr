// Health status transition integration tests.
//
// Verifies that HealthStatus transitions emit HealthChanged events and that
// latency breakdown fields are populated during normal processing.

use std::f32::consts::PI;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::dsp::decoders;
use wavecore::hardware::GainMode;
use wavecore::session::manager::SessionManager;
use wavecore::session::replay::ReplayDevice;
use wavecore::session::{Event, HealthStatus, SessionConfig, StatusUpdate};

/// Generate a cf32 file with a synthetic tone.
fn generate_test_cf32(tag: &str, sample_rate: f64, duration_secs: f64) -> (PathBuf, usize) {
    let num_samples = (sample_rate * duration_secs) as usize;
    let tone_freq = 50_000.0f32;

    let path = PathBuf::from(format!("/tmp/waverunner_health_{tag}.cf32"));
    let mut file = std::fs::File::create(&path).expect("create test cf32 file");

    for i in 0..num_samples {
        let t = i as f32 / sample_rate as f32;
        let phase = 2.0 * PI * tone_freq * t;
        let re = phase.cos() * 0.3;
        let im = phase.sin() * 0.3;
        file.write_all(&re.to_le_bytes()).unwrap();
        file.write_all(&im.to_le_bytes()).unwrap();
    }

    file.flush().unwrap();
    (path, num_samples)
}

/// Verify that Stats events contain populated latency breakdown fields.
///
/// After processing several blocks, all latency fields should be non-zero
/// since each DSP stage takes at least some measurable time.
#[test]
fn latency_breakdown_populated() {
    let sample_rate = 2_048_000.0;
    let (path, _) = generate_test_cf32("latency", sample_rate, 2.0);

    let device = ReplayDevice::open(&path, sample_rate).expect("open replay device");

    let config = SessionConfig {
        schema_version: 1,
        device_index: 0,
        frequency: 100e6,
        sample_rate,
        gain: GainMode::Auto,
        ppm: 0,
        fft_size: 2048,
        pfa: 1e-4,
    };

    let mut registry = DecoderRegistry::new();
    decoders::register_all(&mut registry);

    let (session, event_rx) =
        SessionManager::new_with_device(config, device, registry).expect("start session");

    let mut found_populated = false;
    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        if Instant::now() > deadline {
            break;
        }
        match event_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::Stats(stats)) => {
                // After a few blocks, latency should be populated
                if stats.blocks_processed >= 3 {
                    // total_us must be nonzero (processing always takes time)
                    if stats.latency.total_us > 0 {
                        found_populated = true;
                        // fft_us should be nonzero since FFT is always run
                        assert!(
                            stats.latency.fft_us > 0,
                            "FFT latency should be non-zero, got 0"
                        );
                        assert!(
                            stats.latency.total_us >= stats.latency.dc_removal_us,
                            "Total should be >= dc_removal"
                        );
                        break;
                    }
                }
            }
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !session.is_running() {
                    break;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    session.shutdown();
    std::fs::remove_file(&path).ok();

    assert!(
        found_populated,
        "Never received Stats with populated latency breakdown"
    );
}

/// Verify that initial health status is Normal under light load.
#[test]
fn initial_health_is_normal() {
    let sample_rate = 2_048_000.0;
    let (path, _) = generate_test_cf32("health_init", sample_rate, 2.0);

    let device = ReplayDevice::open(&path, sample_rate).expect("open replay device");

    let config = SessionConfig {
        schema_version: 1,
        device_index: 0,
        frequency: 100e6,
        sample_rate,
        gain: GainMode::Auto,
        ppm: 0,
        fft_size: 2048,
        pfa: 1e-4,
    };

    let mut registry = DecoderRegistry::new();
    decoders::register_all(&mut registry);

    let (session, event_rx) =
        SessionManager::new_with_device(config, device, registry).expect("start session");

    let mut observed_health = None;
    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        if Instant::now() > deadline {
            break;
        }
        match event_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::Stats(stats)) => {
                if stats.blocks_processed >= 3 {
                    observed_health = Some(stats.health);
                    break;
                }
            }
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !session.is_running() {
                    break;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    session.shutdown();
    std::fs::remove_file(&path).ok();

    assert_eq!(
        observed_health,
        Some(HealthStatus::Normal),
        "Expected Normal health under light load, got {observed_health:?}"
    );
}

/// Verify that no spurious HealthChanged events fire under normal processing.
///
/// Under light load (replay at normal rate), health should stay Normal
/// and we should NOT see any HealthChanged status events.
#[test]
fn no_spurious_health_transitions() {
    let sample_rate = 2_048_000.0;
    let (path, _) = generate_test_cf32("health_stable", sample_rate, 3.0);

    let device = ReplayDevice::open(&path, sample_rate).expect("open replay device");

    let config = SessionConfig {
        schema_version: 1,
        device_index: 0,
        frequency: 100e6,
        sample_rate,
        gain: GainMode::Auto,
        ppm: 0,
        fft_size: 2048,
        pfa: 1e-4,
    };

    let mut registry = DecoderRegistry::new();
    decoders::register_all(&mut registry);

    let (session, event_rx) =
        SessionManager::new_with_device(config, device, registry).expect("start session");

    let mut health_changed_count = 0u32;
    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        if Instant::now() > deadline {
            break;
        }
        match event_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::Status(StatusUpdate::HealthChanged(_))) => {
                health_changed_count += 1;
            }
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !session.is_running() {
                    break;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    session.shutdown();
    std::fs::remove_file(&path).ok();

    // Under normal load, health starts Normal and stays Normal, so no transitions
    assert_eq!(
        health_changed_count, 0,
        "Expected zero HealthChanged events under normal load, got {health_changed_count}"
    );
}
