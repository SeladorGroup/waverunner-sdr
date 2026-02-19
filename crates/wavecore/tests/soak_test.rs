// Soak / regression test for pipeline overflow.
//
// Uses ReplayDevice to feed samples at real-time rate through a full
// SessionManager. Asserts zero dropped blocks under normal processing load.

use std::f32::consts::PI;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::dsp::decoders;
use wavecore::hardware::GainMode;
use wavecore::session::manager::SessionManager;
use wavecore::session::replay::ReplayDevice;
use wavecore::session::{Event, SessionConfig, SessionStats};
use wavecore::slo::Slo;

/// Generate a cf32 file containing a synthetic signal.
///
/// Returns the path (in /tmp/) and sample count.
fn generate_test_cf32(tag: &str, sample_rate: f64, duration_secs: f64) -> (PathBuf, usize) {
    let num_samples = (sample_rate * duration_secs) as usize;
    let tone_freq = 50_000.0f32; // 50 kHz offset from center

    let path = PathBuf::from(format!("/tmp/waverunner_soak_{tag}.cf32"));
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

/// Soak test: replay at real-time rate, verify zero drops under normal load.
///
/// This catches processing throughput regressions — if per-block DSP cost
/// exceeds the hardware block interval, this test fails.
#[test]
fn soak_no_drops_under_normal_load() {
    let sample_rate = 2_048_000.0;
    let (path, _) = generate_test_cf32("soak", sample_rate, 1.5);

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

    // Drain events until the session finishes (replay device exhausts file)
    let mut last_stats: Option<SessionStats> = None;
    let deadline = Instant::now() + Duration::from_secs(10); // safety timeout

    loop {
        if Instant::now() > deadline {
            break;
        }
        match event_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::Stats(stats)) => {
                last_stats = Some(stats);
            }
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                // Check if session has ended (replay exhausted)
                if !session.is_running() {
                    break;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    session.shutdown();

    // Clean up
    std::fs::remove_file(&path).ok();

    // Assertions
    let stats = last_stats.expect("Should have received at least one Stats event");
    assert!(
        stats.blocks_processed >= 5,
        "Expected at least 5 blocks processed, got {}",
        stats.blocks_processed
    );
    assert_eq!(
        stats.blocks_dropped, 0,
        "Expected zero dropped blocks, got {} out of {} processed",
        stats.blocks_dropped, stats.blocks_processed
    );

    // SLO drop-budget check (latency/throughput SLOs only valid in release builds)
    let slo = Slo::load();
    let violations = slo.check_drop_budget(&stats);
    assert!(
        violations.is_empty(),
        "SLO violations: {:?}",
        violations
    );
}

/// Soak test with decoder enabled — verifies decode + DSP chain keeps up.
#[test]
fn soak_with_decoder_no_drops() {
    let sample_rate = 2_048_000.0;
    let (path, _) = generate_test_cf32("soak_dec", sample_rate, 1.5);

    let device = ReplayDevice::open(&path, sample_rate).expect("open replay device");

    let config = SessionConfig {
        schema_version: 1,
        device_index: 0,
        frequency: 929.6125e6,
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

    // Enable a decoder to add processing load
    session
        .send(wavecore::session::Command::EnableDecoder(
            "pocsag-1200".to_string(),
        ))
        .expect("enable decoder");

    let mut last_stats: Option<SessionStats> = None;
    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        if Instant::now() > deadline {
            break;
        }
        match event_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::Stats(stats)) => {
                last_stats = Some(stats);
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

    let stats = last_stats.expect("Should have received at least one Stats event");
    assert!(
        stats.blocks_processed >= 5,
        "Expected at least 5 blocks processed, got {}",
        stats.blocks_processed
    );
    assert_eq!(
        stats.blocks_dropped, 0,
        "Expected zero dropped blocks with decoder, got {}",
        stats.blocks_dropped
    );

    // SLO drop-budget check (latency/throughput SLOs only valid in release builds)
    let slo = Slo::load();
    let violations = slo.check_drop_budget(&stats);
    assert!(
        violations.is_empty(),
        "SLO violations: {:?}",
        violations
    );
}

// --- Extended soak tests (run via `cargo test -- --ignored`) ---

/// 60-second soak test at 2.048 MS/s with all decoders registered.
///
/// Validates sustained throughput with zero drops over a longer window
/// that exercises buffer recycling, statistics cadence, and load shedder stability.
#[test]
#[ignore]
fn soak_60s_no_drops() {
    let sample_rate = 2_048_000.0;
    let duration = 60.0;
    let (path, _) = generate_test_cf32("soak60", sample_rate, duration);

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

    let mut blocks_processed = 0u64;
    let mut blocks_dropped = 0u64;
    let mut events_dropped = 0u64;
    let deadline = Instant::now() + Duration::from_secs(90); // generous timeout

    loop {
        if Instant::now() > deadline {
            break;
        }
        match event_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Event::Stats(stats)) => {
                blocks_processed = stats.blocks_processed;
                blocks_dropped = stats.blocks_dropped;
                events_dropped = stats.events_dropped;
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
        blocks_processed >= 100,
        "Expected at least 100 blocks in 60s soak, got {blocks_processed}"
    );
    assert_eq!(
        blocks_dropped, 0,
        "60s soak: {blocks_dropped} dropped out of {blocks_processed}"
    );
    assert_eq!(
        events_dropped, 0,
        "60s soak: {events_dropped} events dropped"
    );
}

/// 60-second soak with pocsag + adsb decoders enabled.
///
/// Adds real decoder processing load on top of DSP pipeline.
#[test]
#[ignore]
fn soak_60s_with_decoders() {
    let sample_rate = 2_048_000.0;
    let duration = 60.0;
    let (path, _) = generate_test_cf32("soak60dec", sample_rate, duration);

    let device = ReplayDevice::open(&path, sample_rate).expect("open replay device");

    let config = SessionConfig {
        schema_version: 1,
        device_index: 0,
        frequency: 929.6125e6,
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

    // Enable decoders to stress the pipeline
    session
        .send(wavecore::session::Command::EnableDecoder(
            "pocsag-1200".to_string(),
        ))
        .expect("enable pocsag");
    session
        .send(wavecore::session::Command::EnableDecoder(
            "adsb".to_string(),
        ))
        .expect("enable adsb");

    let mut blocks_processed = 0u64;
    let mut blocks_dropped = 0u64;
    let mut events_dropped = 0u64;
    let deadline = Instant::now() + Duration::from_secs(90);

    loop {
        if Instant::now() > deadline {
            break;
        }
        match event_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Event::Stats(stats)) => {
                blocks_processed = stats.blocks_processed;
                blocks_dropped = stats.blocks_dropped;
                events_dropped = stats.events_dropped;
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
        blocks_processed >= 100,
        "60s decoder soak: only {blocks_processed} blocks"
    );
    assert_eq!(
        blocks_dropped, 0,
        "60s decoder soak: {blocks_dropped} dropped out of {blocks_processed}"
    );
    assert_eq!(
        events_dropped, 0,
        "60s decoder soak: {events_dropped} events dropped"
    );
}

/// Multi-rate soak: test at 1.024, 2.048, and 2.4 MS/s in sequence.
///
/// Validates that different sample rates all sustain zero drops.
#[test]
#[ignore]
fn soak_multi_rate() {
    let rates = [1_024_000.0, 2_048_000.0, 2_400_000.0];
    let duration = 20.0; // 20s per rate

    for (i, &rate) in rates.iter().enumerate() {
        let tag = format!("soak_mr{i}");
        let (path, _) = generate_test_cf32(&tag, rate, duration);

        let device = ReplayDevice::open(&path, rate).expect("open replay device");

        let config = SessionConfig {
            schema_version: 1,
            device_index: 0,
            frequency: 100e6,
            sample_rate: rate,
            gain: GainMode::Auto,
            ppm: 0,
            fft_size: 2048,
            pfa: 1e-4,
        };

        let mut registry = DecoderRegistry::new();
        decoders::register_all(&mut registry);

        let (session, event_rx) =
            SessionManager::new_with_device(config, device, registry).expect("start session");

        let mut blocks_processed = 0u64;
        let mut blocks_dropped = 0u64;
        let deadline = Instant::now() + Duration::from_secs(40);

        loop {
            if Instant::now() > deadline {
                break;
            }
            match event_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Event::Stats(stats)) => {
                    blocks_processed = stats.blocks_processed;
                    blocks_dropped = stats.blocks_dropped;
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
            blocks_processed >= 10,
            "Rate {rate}: only {blocks_processed} blocks"
        );
        assert_eq!(
            blocks_dropped, 0,
            "Rate {rate}: {blocks_dropped} dropped out of {blocks_processed}"
        );
    }
}
