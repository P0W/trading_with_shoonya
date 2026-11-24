#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use nifty_spread_pilot::ExecutionMode;
use nifty_spread_pilot::audit::{AuditEvent, AuditLog};
use nifty_spread_pilot::broker::PaperBroker;
use nifty_spread_pilot::connection;
use nifty_spread_pilot::engine::{ConfigLoadError, PilotConfig, PilotEngine, ProcessLock};
use nifty_spread_pilot::market;
use nifty_spread_pilot::oauth;
use nifty_spread_pilot::shoonya::ShoonyaBroker;

const USAGE: &str = "\
Usage: nifty-spread-pilot [COMMAND]

Commands:
  --status              Audit live margin, open positions/orders, Black-76 Greeks & OCO triggers (read-only)
  --scan                Scan live NIFTY option chain & rank CORE_INCOME / HIGH_POP_1SD spreads (read-only)
  --once [--live|--paper] Run one complete reconciliation + guardian/entry cycle and exit
  --live                Run continuous live NIFTY spread guardian & execution loop
  --paper               Run continuous paper loop using live quotes/OAuth with simulated fills
  --login-check         Verify headless Shoonya OAuth + TOTP login and fetch NSE NIFTY spot
  --contracts-check     Download and validate Shoonya NFO option symbol master
  --check-connection    Verify an existing NIFTY_SESSION_FILE token
  --help, -h            Show this help message";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliAction {
    Help,
    LoginCheck,
    CheckConnection,
    ContractsCheck,
    Status,
    Scan,
    Once(Option<ExecutionMode>),
    Loop(Option<ExecutionMode>),
}

fn main() -> ExitCode {
    if let Err(error) = run() {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn parse_cli(args: &[OsString]) -> Result<CliAction, String> {
    let strs: Vec<&str> = args
        .iter()
        .map(|a| a.to_str().ok_or("CLI arguments must be valid UTF-8."))
        .collect::<Result<_, _>>()?;
    match strs.as_slice() {
        [] => Ok(CliAction::Loop(None)),
        ["--help" | "-h"] => Ok(CliAction::Help),
        ["--login-check"] => Ok(CliAction::LoginCheck),
        ["--check-connection" | "--connection-check"] => Ok(CliAction::CheckConnection),
        ["--contracts-check"] => Ok(CliAction::ContractsCheck),
        ["--status"] => Ok(CliAction::Status),
        ["--scan"] => Ok(CliAction::Scan),
        ["--once"] => Ok(CliAction::Once(None)),
        ["--once", "--live"] | ["--live", "--once"] => {
            Ok(CliAction::Once(Some(ExecutionMode::Live)))
        }
        ["--once", "--paper"] | ["--paper", "--once"] => {
            Ok(CliAction::Once(Some(ExecutionMode::Paper)))
        }
        ["--live"] => Ok(CliAction::Loop(Some(ExecutionMode::Live))),
        ["--paper"] => Ok(CliAction::Loop(Some(ExecutionMode::Paper))),
        _ => Err(USAGE.to_owned()),
    }
}

fn run() -> Result<(), String> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let cfg_path = oauth::default_config_path();
    let directory = std::env::var_os("NIFTY_AUDIT_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            nifty_spread_pilot::engine::resolve_config_var(&cfg_path, "NIFTY_AUDIT_DIR")
                .ok()
                .flatten()
                .filter(|v| !v.trim().is_empty())
                .map(|v| PathBuf::from(v.trim()))
        })
        .unwrap_or_else(|| PathBuf::from("logs"));
    run_with_args(&args, directory)
}

fn run_with_args(args: &[OsString], directory: PathBuf) -> Result<(), String> {
    let action = parse_cli(args)?;

    match action {
        CliAction::Help => {
            println!("{USAGE}");
            Ok(())
        }
        CliAction::LoginCheck => check_connection_with(directory, oauth::login_and_check),
        CliAction::CheckConnection => check_connection(directory),
        CliAction::ContractsCheck => check_contracts(directory),
        CliAction::Status => run_readonly(directory, false),
        CliAction::Scan => run_readonly(directory, true),
        CliAction::Once(mode_override) => run_trading(directory, mode_override, true),
        CliAction::Loop(mode_override) => run_trading(directory, mode_override, false),
    }
}

fn open_audit(directory: &Path) -> Result<AuditLog, String> {
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "System clock is before the Unix epoch.")?
        .as_nanos();
    let path = directory.join(format!(
        "nifty-spread-pilot-{}-{started}.jsonl",
        std::process::id()
    ));
    AuditLog::open(&path).map_err(|e| format!("Cannot open audit log: {e}"))
}

fn load_audited_config(
    audit: &mut AuditLog,
    mode_override: Option<ExecutionMode>,
) -> Result<PilotConfig, String> {
    match PilotConfig::load(mode_override) {
        Ok(cfg) => Ok(cfg),
        Err(ConfigLoadError::InvalidExecutionMode(msg)) => {
            audit
                .record(AuditEvent::InvalidExecutionMode)
                .map_err(|e| format!("Cannot write audit log: {e}"))?;
            Err(msg.to_owned())
        }
        Err(ConfigLoadError::Message(msg)) => Err(msg),
    }
}

fn build_shoonya(cfg: &PilotConfig) -> ShoonyaBroker {
    ShoonyaBroker::from_config_path(
        cfg.config_file.clone(),
        cfg.account_id.clone(),
        cfg.spot_token.clone(),
    )
}

fn run_readonly(directory: PathBuf, is_scan: bool) -> Result<(), String> {
    let mut audit = open_audit(&directory)?;
    audit
        .record(AuditEvent::Startup)
        .map_err(|e| format!("Cannot write audit log: {e}"))?;
    let cfg = load_audited_config(&mut audit, Some(ExecutionMode::Paper))?;
    let mut engine = PilotEngine::new(build_shoonya(&cfg), cfg);
    let now_ist = market::ist_now()?;
    let output = if is_scan {
        engine.run_scan(&mut audit, now_ist)?
    } else {
        engine.run_status(&mut audit, now_ist)?
    };
    print!("{output}");
    Ok(())
}

fn run_trading(
    directory: PathBuf,
    mode_override: Option<ExecutionMode>,
    once: bool,
) -> Result<(), String> {
    let mut audit = open_audit(&directory)?;
    audit
        .record(AuditEvent::Startup)
        .map_err(|e| format!("Cannot write audit log: {e}"))?;
    let cfg = load_audited_config(&mut audit, mode_override)?;
    let broker = build_shoonya(&cfg);

    if once {
        audit
            .record(AuditEvent::ModeSelected {
                mode: cfg.mode,
                broker: "shoonya",
            })
            .map_err(|e| format!("Cannot write audit log: {e}"))?;
        let _lock = ProcessLock::acquire(&cfg.audit_dir)?;
        let now_ist = market::ist_now()?;
        let summary = match cfg.mode {
            ExecutionMode::Live => PilotEngine::new(broker, cfg).run_cycle(&mut audit, now_ist)?,
            ExecutionMode::Paper => {
                PilotEngine::new(PaperBroker::new(broker), cfg).run_cycle(&mut audit, now_ist)?
            }
        };
        println!("{summary}");
        Ok(())
    } else {
        match cfg.mode {
            ExecutionMode::Live => PilotEngine::new(broker, cfg).run_loop(&mut audit),
            ExecutionMode::Paper => {
                PilotEngine::new(PaperBroker::new(broker), cfg).run_loop(&mut audit)
            }
        }
    }
}

fn check_contracts(directory: PathBuf) -> Result<(), String> {
    let summary = run_audited(
        &directory,
        AuditEvent::ContractCheckStarted,
        AuditEvent::ContractCheckSucceeded,
        AuditEvent::ContractCheckFailed,
        market::check_contracts,
    )?;
    println!("{summary}");
    println!("Contract data is informational; trading eligibility is not verified.");
    Ok(())
}

fn check_connection(directory: PathBuf) -> Result<(), String> {
    check_connection_with(directory, || {
        let cfg_path = oauth::default_config_path();
        let session = std::env::var_os("NIFTY_SESSION_FILE")
            .map(PathBuf::from)
            .or_else(|| {
                nifty_spread_pilot::engine::resolve_config_var(&cfg_path, "NIFTY_SESSION_FILE")
                    .ok()
                    .flatten()
                    .filter(|v| !v.trim().is_empty())
                    .map(|v| PathBuf::from(v.trim()))
            })
            .ok_or("NIFTY_SESSION_FILE must point to the Shoonya OAuth session file.")?;
        let user_id = nifty_spread_pilot::engine::resolve_config_var(&cfg_path, "SHOONYA_USER_ID")
            .map_err(|_| "SHOONYA_USER_ID is required.".to_owned())?
            .filter(|v| !v.trim().is_empty())
            .ok_or("SHOONYA_USER_ID is required.")?;
        let instrument =
            nifty_spread_pilot::engine::resolve_config_var(&cfg_path, "NIFTY_QUOTE_TOKEN")
                .map_err(|_| "NIFTY_QUOTE_TOKEN is invalid.".to_owned())?
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "26000".to_owned());
        connection::check(&session, &user_id, &instrument)
    })
}

fn check_connection_with(
    directory: PathBuf,
    check: impl FnOnce() -> Result<String, String>,
) -> Result<(), String> {
    let price = run_audited(
        &directory,
        AuditEvent::ConnectionCheckStarted,
        AuditEvent::ConnectionCheckSucceeded,
        AuditEvent::ConnectionCheckFailed,
        check,
    )?;
    println!("Shoonya connection OK. NSE last price: {price}");
    println!("Snapshot freshness and trading eligibility are not verified; no orders were placed.");
    Ok(())
}

fn run_audited(
    directory: &Path,
    started: AuditEvent,
    succeeded: AuditEvent,
    failed: AuditEvent,
    check: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    let mut audit = open_audit(directory)?;
    audit
        .record(started)
        .map_err(|e| format!("Cannot write audit log: {e}"))?;
    let result = check();
    let event = if result.is_ok() { succeeded } else { failed };
    audit
        .record(event)
        .map_err(|e| format!("Cannot write audit log: {e}"))?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("nifty-startup-{}-{stamp}", std::process::id()))
    }

    #[test]
    fn cli_parser_supports_all_modes_and_rejects_unknown_flags() {
        let os = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(parse_cli(&os(&[])), Ok(CliAction::Loop(None)));
        assert_eq!(parse_cli(&os(&["--help"])), Ok(CliAction::Help));
        assert_eq!(
            parse_cli(&os(&["--login-check"])),
            Ok(CliAction::LoginCheck)
        );
        assert_eq!(
            parse_cli(&os(&["--check-connection"])),
            Ok(CliAction::CheckConnection)
        );
        assert_eq!(
            parse_cli(&os(&["--connection-check"])),
            Ok(CliAction::CheckConnection)
        );
        assert_eq!(
            parse_cli(&os(&["--contracts-check"])),
            Ok(CliAction::ContractsCheck)
        );
        assert_eq!(parse_cli(&os(&["--status"])), Ok(CliAction::Status));
        assert_eq!(parse_cli(&os(&["--scan"])), Ok(CliAction::Scan));
        assert_eq!(parse_cli(&os(&["--once"])), Ok(CliAction::Once(None)));
        assert_eq!(
            parse_cli(&os(&["--once", "--live"])),
            Ok(CliAction::Once(Some(ExecutionMode::Live)))
        );
        assert_eq!(
            parse_cli(&os(&["--paper", "--once"])),
            Ok(CliAction::Once(Some(ExecutionMode::Paper)))
        );
        assert_eq!(
            parse_cli(&os(&["--live"])),
            Ok(CliAction::Loop(Some(ExecutionMode::Live)))
        );
        assert_eq!(
            parse_cli(&os(&["--paper"])),
            Ok(CliAction::Loop(Some(ExecutionMode::Paper)))
        );
        assert!(parse_cli(&os(&["--bogus"])).is_err());
    }

    #[test]
    fn missing_audit_capability_prevents_start() {
        let directory = temporary_directory();
        std::fs::write(&directory, "not a directory").unwrap();
        let error = run_trading(directory.clone(), Some(ExecutionMode::Paper), true).unwrap_err();
        assert!(error.contains("Cannot open audit log"));
        std::fs::remove_file(directory).unwrap();
    }

    #[test]
    fn connection_check_audits_success_and_failure_without_private_details() {
        for result in [Ok("123.45".to_owned()), Err("private-token".to_owned())] {
            let directory = temporary_directory();
            let should_succeed = result.is_ok();
            assert_eq!(
                check_connection_with(directory.clone(), || result).is_ok(),
                should_succeed
            );
            let path = std::fs::read_dir(&directory)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let log = std::fs::read_to_string(&path).unwrap();
            assert!(log.contains("connection_check_started"));
            assert!(log.contains(if should_succeed {
                "connection_check_succeeded"
            } else {
                "connection_check_failed"
            }));
            assert!(!log.contains("private-token"));
            std::fs::remove_file(path).unwrap();
            std::fs::remove_dir(directory).unwrap();
        }
    }

    #[test]
    fn contract_check_audits_failures() {
        let directory = temporary_directory();
        let error = run_audited(
            &directory,
            AuditEvent::ContractCheckStarted,
            AuditEvent::ContractCheckSucceeded,
            AuditEvent::ContractCheckFailed,
            || Err("invalid symbol master".to_owned()),
        )
        .unwrap_err();
        assert_eq!(error, "invalid symbol master");
        let path = std::fs::read_dir(&directory)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let log = std::fs::read_to_string(&path).unwrap();
        assert!(log.contains("contract_check_started"));
        assert!(log.contains("contract_check_failed"));
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn run_with_args_dispatches_help_and_rejects_bad_flags() {
        use nifty_spread_pilot::broker::Broker;
        let dir = temporary_directory();
        assert!(run_with_args(&[OsString::from("--help")], dir.clone()).is_ok());
        assert!(run_with_args(&[OsString::from("--bad")], dir.clone()).is_err());
        assert!(run_with_args(&[OsString::from("--login-check")], dir.clone()).is_err());
        assert!(run_with_args(&[OsString::from("--check-connection")], dir.clone()).is_err());
        assert!(run_with_args(&[OsString::from("--status")], dir.clone()).is_err());
        assert!(run_with_args(&[OsString::from("--scan")], dir.clone()).is_err());
        assert!(run_with_args(&[OsString::from("--once")], dir.clone()).is_err());
        assert!(
            run_with_args(
                &[OsString::from("--once"), OsString::from("--live")],
                dir.clone()
            )
            .is_err()
        );
        assert!(
            run_with_args(
                &[OsString::from("--once"), OsString::from("--paper")],
                dir.clone()
            )
            .is_err()
        );
        let mut audit = open_audit(&dir).unwrap();
        let cfg = load_audited_config(&mut audit, Some(ExecutionMode::Paper)).unwrap();
        let shoonya = build_shoonya(&cfg);
        assert_eq!(shoonya.name(), "shoonya");
        let _ = std::fs::remove_dir_all(dir);
    }
}
