use serde_json::Value;
use std::process::{Command, Output};

fn run(args: &[&str], root: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xflow"))
        .env_clear()
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn local_models_offline_report_ignores_invalid_config_and_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let invalid = root.path().join("invalid.toml");
    std::fs::write(&invalid, "invalid TOML [").unwrap();
    let output = run(
        &[
            "models",
            "--on-device",
            "--json",
            "--config",
            invalid.to_str().unwrap(),
        ],
        root.path(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["fit"]["readiness"], "not_ready");
    assert_eq!(value["fit"]["memory_fit"], "unverified");
    assert_eq!(value["catalog"]["provenance"]["kind"], "bundled");
    assert_eq!(value["catalog"]["provenance"]["refresh"], "not_available");
    assert_eq!(value["catalog"]["models"].as_array().unwrap().len(), 1);
    assert_eq!(
        value["catalog"]["models"][0]["artifact"]["files"]
            .as_array()
            .unwrap()
            .len(),
        21
    );
    assert!(!root.path().join("config").exists());
    assert!(!root.path().join("data").exists());
    assert!(!root.path().join("state").exists());
    assert_eq!(std::fs::read_to_string(invalid).unwrap(), "invalid TOML [");
    #[cfg(not(target_os = "macos"))]
    {
        assert_eq!(value["fit"]["hardware"], "unsupported");
        assert_eq!(value["host"]["installed_ram_bytes"], "unknown");
    }
    let output = run(&["--quiet", "models", "--on-device"], root.path());
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "Local model catalog",
        "not ready",
        "unverified",
        "Hindi",
        "Japanese",
        "6.34%",
        "483105645",
        "CC-BY-4.0",
    ] {
        assert!(text.contains(expected), "missing {expected}");
    }
}

#[test]
fn local_models_loopback_model_listing_keeps_its_existing_meaning() {
    let root = tempfile::tempdir().unwrap();
    let output = run(&["models", "local", "--json"], root.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Systran/faster-whisper-small"));
    assert!(!text.contains("parakeet-tdt-0.6b-v3-coreml"));
    let conflict = run(&["models", "local", "--on-device", "--json"], root.path());
    assert_eq!(conflict.status.code(), Some(2));
    assert!(conflict.stdout.is_empty());
    let error: Value = serde_json::from_slice(&conflict.stderr).unwrap();
    assert_eq!(error["exit_code"], 2);
}
