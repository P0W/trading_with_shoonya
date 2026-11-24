use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use nifty_spread_pilot::audit::AuditLog;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temporary_log() -> PathBuf {
    std::env::temp_dir().join(format!(
        "nifty-spread-audit-{}-{}.jsonl",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
}

fn run_with_log(mode: &str) -> (std::process::Output, String) {
    let directory = temporary_log();
    let missing_env = directory.join("missing.env");
    let output = Command::new(env!("CARGO_BIN_EXE_nifty-spread-pilot"))
        .arg("--once")
        .env("NIFTY_AUDIT_DIR", &directory)
        .env("SHOONYA_CONFIG_FILE", &missing_env)
        .env("NIFTY_EXECUTION_MODE", mode)
        .output()
        .unwrap();
    let paths: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    assert_eq!(paths.len(), 1);
    let content = fs::read_to_string(&paths[0]).unwrap();
    fs::remove_dir_all(directory).unwrap();
    (output, content)
}

#[test]
fn records_startup_and_mode_selection_before_requiring_credentials() {
    let (output, content) = run_with_log("paper");
    assert!(!output.status.success());
    let lines: Vec<serde_json::Value> = content
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["event"], "startup");
    assert_eq!(lines[1]["event"], "mode_selected");
    assert_eq!(lines[1]["mode"], "paper");
    assert_eq!(lines[1]["broker"], "shoonya");
    assert!(lines[0]["timestamp_unix_ms"].is_u64());
}

#[test]
fn invalid_mode_is_logged_without_its_value() {
    let secret = "untrusted-value-that-must-not-be-logged";
    let (output, content) = run_with_log(secret);
    assert!(!output.status.success());
    assert!(!content.contains(secret));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    assert!(content.contains("invalid_execution_mode"));
}

#[test]
fn live_mode_is_selected_explicitly() {
    let (output, content) = run_with_log("live");
    assert!(!output.status.success());
    let events: Vec<serde_json::Value> = content
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[1]["mode"], "live");
    assert_eq!(events[1]["event"], "mode_selected");
}

#[test]
fn restarts_create_separate_audit_files() {
    let directory = temporary_log();
    let missing_env = directory.join("missing.env");
    for _ in 0..2 {
        let output = Command::new(env!("CARGO_BIN_EXE_nifty-spread-pilot"))
            .arg("--once")
            .env("NIFTY_AUDIT_DIR", &directory)
            .env("SHOONYA_CONFIG_FILE", &missing_env)
            .env("NIFTY_EXECUTION_MODE", "paper")
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    let paths: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    assert_eq!(paths.len(), 2);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn cli_help_scan_and_status_modes_execute_cleanly() {
    let help = Command::new(env!("CARGO_BIN_EXE_nifty-spread-pilot"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--status"));

    let bad = Command::new(env!("CARGO_BIN_EXE_nifty-spread-pilot"))
        .arg("--unknown-flag")
        .output()
        .unwrap();
    assert!(!bad.status.success());

    for flag in ["--scan", "--status"] {
        let directory = temporary_log();
        let missing_env = directory.join("missing.env");
        let out = Command::new(env!("CARGO_BIN_EXE_nifty-spread-pilot"))
            .arg(flag)
            .env("NIFTY_AUDIT_DIR", &directory)
            .env("SHOONYA_CONFIG_FILE", &missing_env)
            .output()
            .unwrap();
        assert!(!out.status.success());
        let paths: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .collect();
        assert_eq!(paths.len(), 1);
        let log = fs::read_to_string(&paths[0]).unwrap();
        assert!(log.contains("startup"));
        fs::remove_dir_all(directory).unwrap();
    }

    // Also verify --once --paper and --once --live CLI flags audit startup & mode_selected cleanly.
    for flag in ["--paper", "--live"] {
        let directory = temporary_log();
        let missing_env = directory.join("missing.env");
        let out = Command::new(env!("CARGO_BIN_EXE_nifty-spread-pilot"))
            .args(["--once", flag])
            .env("NIFTY_AUDIT_DIR", &directory)
            .env("SHOONYA_CONFIG_FILE", &missing_env)
            .output()
            .unwrap();
        assert!(!out.status.success());
        let paths: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .collect();
        assert_eq!(paths.len(), 1);
        let log = fs::read_to_string(&paths[0]).unwrap();
        assert!(log.contains("startup"));
        assert!(log.contains("mode_selected"));
        fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn audit_open_failure_prevents_start() {
    let path = temporary_log();
    fs::write(&path, b"not a directory").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_nifty-spread-pilot"))
        .env("NIFTY_AUDIT_DIR", &path)
        .output()
        .unwrap();
    fs::remove_file(path).unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Cannot open audit log"));
}

#[test]
fn existing_log_and_concurrent_writer_are_rejected() {
    let path = temporary_log();
    let first = AuditLog::open(&path).unwrap();
    assert!(AuditLog::open(&path).is_err());
    drop(first);
    fs::write(&path, b"{\"unfinished\":").unwrap();
    assert!(AuditLog::open(&path).is_err());
    fs::remove_file(path).unwrap();
}

#[test]
fn connection_check_failure_is_audited_without_credentials() {
    let directory = temporary_log();
    let output = Command::new(env!("CARGO_BIN_EXE_nifty-spread-pilot"))
        .arg("--check-connection")
        .env("NIFTY_AUDIT_DIR", &directory)
        .env_remove("NIFTY_SESSION_FILE")
        .env("SHOONYA_USER_ID", "untrusted-private-value")
        .env("NIFTY_QUOTE_TOKEN", "26000")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let path = fs::read_dir(&directory)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("connection_check_started"));
    assert!(content.contains("connection_check_failed"));
    assert!(!content.contains("untrusted-private-value"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("untrusted-private-value"));
    fs::remove_file(path).unwrap();
    fs::remove_dir(directory).unwrap();
}
