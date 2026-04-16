// Failure injection and resilience tests.
//
// Verifies graceful degradation under adverse conditions:
// corrupted input, disk errors, malformed configs, channel disconnection.

use std::f32::consts::PI;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use wavecore::analysis::export::{ExportConfig, ExportContent, ExportFormat};
use wavecore::analysis::report::{SessionMetadata, SessionReport, export_session_report};
use wavecore::dsp::decoder::DecoderRegistry;
use wavecore::dsp::decoders;
use wavecore::hardware::GainMode;
use wavecore::mode::profile::load_user_profiles;
use wavecore::session::manager::SessionManager;
use wavecore::session::replay::ReplayDevice;
use wavecore::session::{Command, Event, SessionConfig};

fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("waverunner_resilience");
    std::fs::create_dir_all(&dir).ok();
    dir.join(name)
}

fn make_config(sample_rate: f64) -> SessionConfig {
    SessionConfig {
        schema_version: 1,
        device_index: 0,
        frequency: 100e6,
        sample_rate,
        gain: GainMode::Auto,
        ppm: 0,
        fft_size: 2048,
        pfa: 1e-4,
    }
}

/// Generate a minimal valid cf32 file.
fn generate_cf32(path: &Path, sample_rate: f64, duration_secs: f64) {
    let num_samples = (sample_rate * duration_secs) as usize;
    let mut file = std::fs::File::create(path).expect("create file");
    for i in 0..num_samples {
        let t = i as f32 / sample_rate as f32;
        let phase = 2.0 * PI * 50_000.0 * t;
        let re = phase.cos() * 0.3;
        let im = phase.sin() * 0.3;
        file.write_all(&re.to_le_bytes()).unwrap();
        file.write_all(&im.to_le_bytes()).unwrap();
    }
    file.flush().unwrap();
}

// --- Corrupted/truncated replay input ---

/// Replay a truncated cf32 file (partial final sample).
/// Should process available complete samples without panic.
#[test]
fn replay_truncated_cf32() {
    let path = temp_path("truncated.cf32");
    // Write 100 complete samples + 3 extra bytes (incomplete sample)
    {
        let mut file = std::fs::File::create(&path).unwrap();
        for i in 0..100u32 {
            let t = i as f32 / 2048000.0;
            let phase = 2.0 * PI * 50_000.0 * t;
            file.write_all(&phase.cos().to_le_bytes()).unwrap();
            file.write_all(&phase.sin().to_le_bytes()).unwrap();
        }
        // Partial sample (3 bytes, not a complete f32 pair)
        file.write_all(&[0u8, 1, 2]).unwrap();
        file.flush().unwrap();
    }

    // This should open successfully — the file has enough data
    let device = ReplayDevice::open(&path, 2_048_000.0).expect("open truncated file");
    let config = make_config(2_048_000.0);
    let registry = DecoderRegistry::new();

    let (session, event_rx) =
        SessionManager::new_with_device(config, device, registry).expect("start session");

    // Drain events briefly — session should finish without panic
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if Instant::now() > deadline {
            break;
        }
        match event_rx.recv_timeout(Duration::from_millis(200)) {
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
    // If we get here without panic, test passes
}

/// Replay an empty (zero-byte) cf32 file.
/// Should return an error from open(), not panic.
#[test]
fn replay_empty_file() {
    let path = temp_path("empty.cf32");
    std::fs::write(&path, b"").unwrap();

    let result = ReplayDevice::open(&path, 2_048_000.0);
    assert!(result.is_err(), "Empty file should fail to open");
    let err = format!("{}", result.err().unwrap());
    assert!(
        err.contains("too small"),
        "Error should mention file is too small, got: {err}"
    );

    std::fs::remove_file(&path).ok();
}

/// Attempt to replay a file with unrecognized extension.
/// Should return descriptive error.
#[test]
fn replay_unknown_extension() {
    let path = temp_path("data.xyz");
    std::fs::write(&path, b"not an IQ file").unwrap();

    let result = ReplayDevice::open(&path, 2_048_000.0);
    assert!(result.is_err(), "Unknown extension should fail");
    let err = format!("{}", result.err().unwrap());
    assert!(
        err.contains("Unknown IQ format"),
        "Error should mention unknown format, got: {err}"
    );

    std::fs::remove_file(&path).ok();
}

// --- Export error handling ---

/// Export to a read-only directory should return error, not panic.
#[test]
fn export_to_readonly_dir() {
    let config = ExportConfig {
        path: PathBuf::from("/proc/waverunner_test_export.csv"),
        format: ExportFormat::Csv,
        content: ExportContent::Spectrum {
            spectrum_db: vec![-50.0; 100],
            sample_rate: 2_048_000.0,
            center_freq: 100e6,
        },
    };

    let result = wavecore::analysis::export::export_to_file(&config);
    assert!(result.is_err(), "Should fail on /proc path");
}

/// Session report export to invalid path returns error.
#[test]
fn export_report_invalid_path() {
    let report = SessionReport {
        metadata: SessionMetadata {
            start_time: "2026-02-15T12:00:00Z".to_string(),
            duration_secs: 1.0,
            center_freq: 100e6,
            sample_rate: 2_048_000.0,
            gain: "Auto".to_string(),
            fft_size: 2048,
        },
        scan_results: None,
        decoded_messages: vec![],
        annotations: vec![],
    };

    let result = export_session_report(&report, Path::new("/proc/wr_test_report.json"), "json");
    assert!(result.is_err(), "Should fail on /proc path");
}

// --- Recording resilience ---

/// Start recording to a read-only path. Should emit Error event, not panic.
#[test]
fn record_to_readonly_path() {
    let sample_rate = 2_048_000.0;
    let cf32_path = temp_path("record_test.cf32");
    generate_cf32(&cf32_path, sample_rate, 1.0);

    let device = ReplayDevice::open(&cf32_path, sample_rate).expect("open replay");
    let config = make_config(sample_rate);
    let registry = DecoderRegistry::new();

    let (session, event_rx) =
        SessionManager::new_with_device(config, device, registry).expect("start session");

    // Try to record to a read-only path
    session
        .send(Command::StartRecord {
            path: PathBuf::from("/proc/waverunner_impossible_recording.cf32"),
            format: wavecore::session::RecordFormat::RawCf32,
        })
        .ok();

    // Should get an Error event, not a panic
    let mut got_error = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if Instant::now() > deadline {
            break;
        }
        match event_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::Error(msg)) => {
                if msg.contains("recording") || msg.contains("Failed") {
                    got_error = true;
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
    std::fs::remove_file(&cf32_path).ok();
    assert!(
        got_error,
        "Should have received Error event for bad recording path"
    );
}

// --- Malformed config/profile ---

/// Malformed TOML profile should not panic, just be skipped.
#[test]
fn malformed_toml_profile_no_panic() {
    let dir = temp_path("bad_profiles");
    std::fs::create_dir_all(&dir).ok();

    // Write invalid TOML
    std::fs::write(dir.join("broken.toml"), "this is not { valid toml !!!").unwrap();

    // Write TOML with missing required fields
    std::fs::write(dir.join("incomplete.toml"), "[metadata]\nfoo = \"bar\"\n").unwrap();

    // Should load without panic (just log warnings)
    let profiles = load_user_profiles(&dir);
    // Invalid profiles are skipped
    assert!(
        profiles.is_empty(),
        "Malformed profiles should be skipped, got {} profiles",
        profiles.len()
    );

    std::fs::remove_dir_all(&dir).ok();
}

// --- Decoder resilience ---

/// Feed random garbage through POCSAG decoder. Should not panic.
#[test]
fn decoder_garbage_input_no_panic() {
    use wavecore::types::Sample;

    let mut registry = DecoderRegistry::new();
    decoders::register_all(&mut registry);

    if let Some(mut decoder) = registry.create("pocsag-1200") {
        // Feed random-ish garbage samples
        let garbage: Vec<Sample> = (0..65536)
            .map(|i| {
                let v = ((i * 7 + 13) % 256) as f32 / 128.0 - 1.0;
                Sample::new(v, -v)
            })
            .collect();

        // This should not panic, just produce no valid messages (or some spurious ones)
        let _messages = decoder.process(&garbage);
        // If we get here, test passes — no panic
    }
}

// --- Channel disconnection ---

/// Drop the event receiver while session is running.
/// Processing thread should detect disconnection and stop cleanly.
#[test]
fn event_channel_drop_during_session() {
    let sample_rate = 2_048_000.0;
    let cf32_path = temp_path("channel_drop.cf32");
    generate_cf32(&cf32_path, sample_rate, 2.0);

    let device = ReplayDevice::open(&cf32_path, sample_rate).expect("open replay");
    let config = make_config(sample_rate);
    let registry = DecoderRegistry::new();

    let (session, event_rx) =
        SessionManager::new_with_device(config, device, registry).expect("start session");

    // Wait for a few events, then drop the receiver
    std::thread::sleep(Duration::from_millis(200));
    drop(event_rx);

    // Give processing thread time to detect disconnection
    std::thread::sleep(Duration::from_millis(500));

    // Shutdown should not hang
    session.shutdown();
    std::fs::remove_file(&cf32_path).ok();
    // If we get here without hanging, test passes
}
