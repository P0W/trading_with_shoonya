use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::ExecutionMode;

/// Restricted event vocabulary: credentials and arbitrary broker responses cannot be logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditEvent {
    Startup,
    InvalidExecutionMode,
    ModeSelected {
        mode: ExecutionMode,
        broker: &'static str,
    },
    ConnectionCheckStarted,
    ConnectionCheckSucceeded,
    ConnectionCheckFailed,
    ContractCheckStarted,
    ContractCheckSucceeded,
    ContractCheckFailed,
    ScanStarted {
        broker: &'static str,
    },
    ScanCompleted {
        spot_paise: i64,
        forward_paise: i64,
        candidates_qualified: usize,
        recommended: bool,
    },
    ScanFailed,
    StatusCheckStarted {
        broker: &'static str,
    },
    StatusCheckCompleted {
        available_margin_paise: i64,
        open_positions: usize,
        active_orders: usize,
    },
    StatusCheckFailed,
    Reconciled {
        state: &'static str,
        open_positions: usize,
        active_orders: usize,
    },
    UncoveredExposureDetected {
        short_qty: u32,
        long_qty: u32,
    },
    MarketClosedSkip,
    NoPositionRecommended {
        evaluated_pairs: usize,
    },
    OrderSubmitted {
        mode: ExecutionMode,
        leg: &'static str,
        side: &'static str,
        qty: u32,
        limit_paise: i64,
    },
    OrderReconciled {
        leg: &'static str,
        status: &'static str,
        filled_qty: u32,
    },
    EntryAbortedNoHedgeFill,
    EntryCompleted {
        mode: ExecutionMode,
        option_kind: &'static str,
        short_strike_paise: i64,
        long_strike_paise: i64,
        qty: u32,
        net_credit_paise: i64,
    },
    SpreadEvaluated {
        regime: &'static str,
        verdict: &'static str,
        net_pnl_paise: i64,
    },
    ShortFlatConfirmed {
        qty_closed: u32,
    },
    SpreadClosed {
        mode: ExecutionMode,
        reason: &'static str,
        qty: u32,
    },
    CycleFailed {
        stage: &'static str,
    },
}

pub struct AuditLog {
    file: File,
}

impl AuditLog {
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }

        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Audit log is not a regular file",
            ));
        }
        Ok(Self { file })
    }

    pub fn record(&mut self, event: AuditEvent) -> io::Result<()> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| io::Error::other("System clock is before the Unix epoch"))?
            .as_millis();
        let mode_str = |mode: ExecutionMode| match mode {
            ExecutionMode::Paper => "paper",
            ExecutionMode::Live => "live",
        };
        let record = match event {
            AuditEvent::Startup => json!({
                "timestamp_unix_ms": timestamp,
                "event": "startup",
            }),
            AuditEvent::InvalidExecutionMode => json!({
                "timestamp_unix_ms": timestamp,
                "event": "invalid_execution_mode",
            }),
            AuditEvent::ModeSelected { mode, broker } => json!({
                "timestamp_unix_ms": timestamp,
                "event": "mode_selected",
                "mode": mode_str(mode),
                "broker": broker,
            }),
            AuditEvent::ConnectionCheckStarted => json!({
                "timestamp_unix_ms": timestamp,
                "event": "connection_check_started",
            }),
            AuditEvent::ConnectionCheckSucceeded => json!({
                "timestamp_unix_ms": timestamp,
                "event": "connection_check_succeeded",
            }),
            AuditEvent::ConnectionCheckFailed => json!({
                "timestamp_unix_ms": timestamp,
                "event": "connection_check_failed",
            }),
            AuditEvent::ContractCheckStarted => json!({
                "timestamp_unix_ms": timestamp,
                "event": "contract_check_started",
            }),
            AuditEvent::ContractCheckSucceeded => json!({
                "timestamp_unix_ms": timestamp,
                "event": "contract_check_succeeded",
            }),
            AuditEvent::ContractCheckFailed => json!({
                "timestamp_unix_ms": timestamp,
                "event": "contract_check_failed",
            }),
            AuditEvent::ScanStarted { broker } => json!({
                "timestamp_unix_ms": timestamp,
                "event": "scan_started",
                "broker": broker,
            }),
            AuditEvent::ScanCompleted {
                spot_paise,
                forward_paise,
                candidates_qualified,
                recommended,
            } => json!({
                "timestamp_unix_ms": timestamp,
                "event": "scan_completed",
                "spot_paise": spot_paise,
                "forward_paise": forward_paise,
                "candidates_qualified": candidates_qualified,
                "recommended": recommended,
            }),
            AuditEvent::ScanFailed => json!({
                "timestamp_unix_ms": timestamp,
                "event": "scan_failed",
            }),
            AuditEvent::StatusCheckStarted { broker } => json!({
                "timestamp_unix_ms": timestamp,
                "event": "status_check_started",
                "broker": broker,
            }),
            AuditEvent::StatusCheckCompleted {
                available_margin_paise,
                open_positions,
                active_orders,
            } => json!({
                "timestamp_unix_ms": timestamp,
                "event": "status_check_completed",
                "available_margin_paise": available_margin_paise,
                "open_positions": open_positions,
                "active_orders": active_orders,
            }),
            AuditEvent::StatusCheckFailed => json!({
                "timestamp_unix_ms": timestamp,
                "event": "status_check_failed",
            }),
            AuditEvent::Reconciled {
                state,
                open_positions,
                active_orders,
            } => json!({
                "timestamp_unix_ms": timestamp,
                "event": "reconciled",
                "state": state,
                "open_positions": open_positions,
                "active_orders": active_orders,
            }),
            AuditEvent::UncoveredExposureDetected {
                short_qty,
                long_qty,
            } => json!({
                "timestamp_unix_ms": timestamp,
                "event": "uncovered_exposure_detected",
                "short_qty": short_qty,
                "long_qty": long_qty,
            }),
            AuditEvent::MarketClosedSkip => json!({
                "timestamp_unix_ms": timestamp,
                "event": "market_closed_skip",
            }),
            AuditEvent::NoPositionRecommended { evaluated_pairs } => json!({
                "timestamp_unix_ms": timestamp,
                "event": "no_position_recommended",
                "evaluated_pairs": evaluated_pairs,
            }),
            AuditEvent::OrderSubmitted {
                mode,
                leg,
                side,
                qty,
                limit_paise,
            } => json!({
                "timestamp_unix_ms": timestamp,
                "event": "order_submitted",
                "mode": mode_str(mode),
                "leg": leg,
                "side": side,
                "qty": qty,
                "limit_paise": limit_paise,
            }),
            AuditEvent::OrderReconciled {
                leg,
                status,
                filled_qty,
            } => json!({
                "timestamp_unix_ms": timestamp,
                "event": "order_reconciled",
                "leg": leg,
                "status": status,
                "filled_qty": filled_qty,
            }),
            AuditEvent::EntryAbortedNoHedgeFill => json!({
                "timestamp_unix_ms": timestamp,
                "event": "entry_aborted_no_hedge_fill",
            }),
            AuditEvent::EntryCompleted {
                mode,
                option_kind,
                short_strike_paise,
                long_strike_paise,
                qty,
                net_credit_paise,
            } => json!({
                "timestamp_unix_ms": timestamp,
                "event": "entry_completed",
                "mode": mode_str(mode),
                "option_kind": option_kind,
                "short_strike_paise": short_strike_paise,
                "long_strike_paise": long_strike_paise,
                "qty": qty,
                "net_credit_paise": net_credit_paise,
            }),
            AuditEvent::SpreadEvaluated {
                regime,
                verdict,
                net_pnl_paise,
            } => json!({
                "timestamp_unix_ms": timestamp,
                "event": "spread_evaluated",
                "regime": regime,
                "verdict": verdict,
                "net_pnl_paise": net_pnl_paise,
            }),
            AuditEvent::ShortFlatConfirmed { qty_closed } => json!({
                "timestamp_unix_ms": timestamp,
                "event": "short_flat_confirmed",
                "qty_closed": qty_closed,
            }),
            AuditEvent::SpreadClosed { mode, reason, qty } => json!({
                "timestamp_unix_ms": timestamp,
                "event": "spread_closed",
                "mode": mode_str(mode),
                "reason": reason,
                "qty": qty,
            }),
            AuditEvent::CycleFailed { stage } => json!({
                "timestamp_unix_ms": timestamp,
                "event": "cycle_failed",
                "stage": stage,
            }),
        };
        let mut line = serde_json::to_vec(&record).map_err(io::Error::other)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.sync_data()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_persisted_as_separate_json_records() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("nifty-audit-{}-{stamp}.jsonl", std::process::id()));
        let mut audit = AuditLog::open(&path).unwrap();
        let sample_events = [
            AuditEvent::Startup,
            AuditEvent::InvalidExecutionMode,
            AuditEvent::ModeSelected {
                mode: ExecutionMode::Paper,
                broker: "shoonya",
            },
            AuditEvent::ModeSelected {
                mode: ExecutionMode::Live,
                broker: "shoonya",
            },
            AuditEvent::ConnectionCheckStarted,
            AuditEvent::ConnectionCheckSucceeded,
            AuditEvent::ConnectionCheckFailed,
            AuditEvent::ContractCheckStarted,
            AuditEvent::ContractCheckSucceeded,
            AuditEvent::ContractCheckFailed,
            AuditEvent::ScanStarted { broker: "shoonya" },
            AuditEvent::ScanCompleted {
                spot_paise: 2_535_000,
                forward_paise: 2_536_200,
                candidates_qualified: 2,
                recommended: true,
            },
            AuditEvent::ScanFailed,
            AuditEvent::StatusCheckStarted { broker: "shoonya" },
            AuditEvent::StatusCheckCompleted {
                available_margin_paise: 20_000_000,
                open_positions: 2,
                active_orders: 0,
            },
            AuditEvent::StatusCheckFailed,
            AuditEvent::Reconciled {
                state: "flat",
                open_positions: 0,
                active_orders: 0,
            },
            AuditEvent::UncoveredExposureDetected {
                short_qty: 65,
                long_qty: 0,
            },
            AuditEvent::MarketClosedSkip,
            AuditEvent::NoPositionRecommended {
                evaluated_pairs: 24,
            },
            AuditEvent::OrderSubmitted {
                mode: ExecutionMode::Live,
                leg: "long_hedge",
                side: "buy",
                qty: 65,
                limit_paise: 8_050,
            },
            AuditEvent::OrderReconciled {
                leg: "long_hedge",
                status: "complete",
                filled_qty: 65,
            },
            AuditEvent::EntryAbortedNoHedgeFill,
            AuditEvent::EntryCompleted {
                mode: ExecutionMode::Live,
                option_kind: "CE",
                short_strike_paise: 2_560_000,
                long_strike_paise: 2_580_000,
                qty: 65,
                net_credit_paise: 4_200,
            },
            AuditEvent::SpreadEvaluated {
                regime: "NO_LOSS_PROFIT_LOCK_OCO",
                verdict: "HOLD_WITH_NO_LOSS_OCO",
                net_pnl_paise: 95_000,
            },
            AuditEvent::ShortFlatConfirmed { qty_closed: 65 },
            AuditEvent::SpreadClosed {
                mode: ExecutionMode::Live,
                reason: "profit_target",
                qty: 65,
            },
            AuditEvent::CycleFailed { stage: "quotes" },
        ];
        for event in sample_events.iter().cloned() {
            audit.record(event).unwrap();
        }
        drop(audit);
        let records: Vec<serde_json::Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), sample_events.len());
        assert_eq!(records[0]["event"], "startup");
        assert_eq!(records[1]["event"], "invalid_execution_mode");
        assert_eq!(records[2]["mode"], "paper");
        assert_eq!(records[3]["mode"], "live");
        std::fs::remove_file(path).unwrap();
    }
}
