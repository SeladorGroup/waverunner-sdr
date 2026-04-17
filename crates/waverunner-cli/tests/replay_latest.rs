use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_test_dir(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("waverunner_cli_test_{label}_{unique}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_latest_capture_fixture(root: &Path) -> PathBuf {
    let config_root = root.join("config");
    let capture_dir = config_root.join("waverunner").join("captures");
    fs::create_dir_all(&capture_dir).unwrap();

    let capture_path = capture_dir.join("smoke-latest.cf32");
    let metadata_path = capture_dir.join("smoke-latest.json");
    let catalog_path = capture_dir.join("catalog.json");

    fs::write(&capture_path, vec![0_u8; 32768 * 8]).unwrap();
    fs::write(
        &metadata_path,
        r#"{
  "schema_version": 1,
  "center_freq": 94900000.0,
  "sample_rate": 1024000.0,
  "gain": "auto",
  "format": "cf32",
  "timestamp": "2026-04-17T12:00:00Z",
  "duration_secs": 0.256,
  "device": "test",
  "samples_written": 32768,
  "label": "Smoke Latest",
  "notes": null,
  "tags": [],
  "demod_mode": null,
  "decoder": null,
  "timeline_path": null,
  "report_path": null
}"#,
    )
    .unwrap();
    fs::write(
        &catalog_path,
        format!(
            r#"{{
  "capture": [
    {{
      "schema_version": 1,
      "id": "smoke-latest",
      "created_at": "2026-04-17T12:00:00Z",
      "path": "{}",
      "metadata_path": "{}",
      "timeline_path": null,
      "report_path": null,
      "label": "Smoke Latest",
      "notes": null,
      "tags": [],
      "center_freq": 94900000.0,
      "sample_rate": 1024000.0,
      "format": "cf32",
      "duration_secs": 0.256,
      "size_bytes": 262144,
      "demod_mode": null,
      "decoder": null,
      "source": "LiveRecord"
    }}
  ]
}}"#,
            capture_path.display(),
            metadata_path.display()
        ),
    )
    .unwrap();

    config_root
}

fn run_waverunner(config_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_waverunner"))
        .args(args)
        .env("XDG_CONFIG_HOME", config_root)
        .env(
            "HOME",
            std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()),
        )
        .output()
        .unwrap()
}

#[test]
fn replay_latest_uses_catalog_and_exits_cleanly() {
    let root = unique_test_dir("replay_latest");
    let config_root = write_latest_capture_fixture(&root);

    let output = run_waverunner(&config_root, &["replay", "--latest", "--fast"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Replay finished."));
}

#[test]
fn analyze_latest_measure_handles_short_capture() {
    let root = unique_test_dir("analyze_latest");
    let config_root = write_latest_capture_fixture(&root);

    let output = run_waverunner(&config_root, &["analyze", "--latest", "measure"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Signal Measurement"));
}

#[test]
fn replay_capture_selector_uses_catalog_entry() {
    let root = unique_test_dir("replay_selector");
    let config_root = write_latest_capture_fixture(&root);

    let output = run_waverunner(
        &config_root,
        &["replay", "--capture", "smoke-latest", "--fast"],
    );
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Replay finished."));
}

#[test]
fn analyze_capture_selector_measure_handles_short_capture() {
    let root = unique_test_dir("analyze_selector");
    let config_root = write_latest_capture_fixture(&root);

    let output = run_waverunner(
        &config_root,
        &["analyze", "--capture", "smoke-latest", "measure"],
    );
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Signal Measurement"));
}
