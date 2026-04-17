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
fn library_import_edit_and_remove_roundtrip() {
    let root = unique_test_dir("library_manage");
    let config_root = root.join("config");
    let capture_path = root.join("import.cf32");
    fs::write(&capture_path, vec![0_u8; 4096]).unwrap();

    let import = run_waverunner(
        &config_root,
        &[
            "library",
            "import",
            &capture_path.display().to_string(),
            "--sample-rate",
            "2048000",
            "--frequency",
            "433920000",
            "--label",
            "Imported",
            "--tag",
            "smoke",
        ],
    );
    assert!(
        import.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&import.stdout),
        String::from_utf8_lossy(&import.stderr)
    );

    let list = run_waverunner(&config_root, &["library", "list", "--json"]);
    assert!(list.status.success());
    let list_json: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(list_json.as_array().unwrap().len(), 1);

    let edit = run_waverunner(
        &config_root,
        &[
            "library",
            "edit",
            "latest",
            "--notes",
            "updated notes",
            "--tag",
            "extra",
        ],
    );
    assert!(edit.status.success());

    let latest = run_waverunner(&config_root, &["library", "latest", "--json"]);
    assert!(latest.status.success());
    let latest_json: serde_json::Value = serde_json::from_slice(&latest.stdout).unwrap();
    assert_eq!(latest_json["label"], "Imported");
    assert_eq!(latest_json["notes"], "updated notes");
    assert_eq!(latest_json["tags"], serde_json::json!(["smoke", "extra"]));

    let remove = run_waverunner(&config_root, &["library", "remove", "latest"]);
    assert!(remove.status.success());

    let final_list = run_waverunner(&config_root, &["library", "list", "--json"]);
    assert!(final_list.status.success());
    let final_json: serde_json::Value = serde_json::from_slice(&final_list.stdout).unwrap();
    assert!(final_json.as_array().unwrap().is_empty());
}
