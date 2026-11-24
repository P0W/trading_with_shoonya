//! Unified broker-agnostic execution & guardian engine (`PilotEngine<B: Broker>`).
//!
//! Runs the exact same quantitative pipeline in both `Live` (`ShoonyaBroker`) and
//! `Paper` (`PaperBroker<ShoonyaBroker>`) modes:
//! 1. Reconciles live `PositionBook` & `OrderBook` on startup and every cycle.
//! 2. Guards open spreads with Discounted Black-76 Greeks, 7 Priority-1 kill
//!    switches, and non-linear `F*` OCO triggers (`BUY` Short leg FIRST ->
//!    `SHORT_FLAT_CONFIRMED` -> `SELL` Long hedge SECOND).
//! 3. Unwinds orphan long legs safely and halts fail-closed on any uncovered
//!    short or unrecognized multi-leg exposure.
//! 4. Scans and enters high-edge spreads hedge-first (`BUY` Long hedge FIRST ->
//!    fill-confirmed -> `SELL` Short leg SECOND <= confirmed hedge qty).

use std::collections::HashMap;
use std::env::VarError;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use time::{OffsetDateTime, Time, Weekday};
use zeroize::Zeroizing;

use crate::audit::{AuditEvent, AuditLog};
use crate::broker::{
    Broker, BrokerError, BrokerOrder, BrokerPosition, OptionContract, OrderPriceType, OrderRequest,
    OrderSide, OrderStatus, ProductType, Retention, paise_to_rupees, rupees_to_paise,
};
use crate::strategy::{
    CandidateSpread, LiveEvalInput, LiveSpreadEvaluation, OcoRegime, RankOutcome, StrategyBias,
    evaluate_live_spread, rank_candidate_spreads,
};
use crate::{ExecutionMode, OptionKind, SimulatedFills};

const MAX_ENV_FILE_BYTES: u64 = 65_536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigLoadError {
    InvalidExecutionMode(&'static str),
    Message(String),
}

#[derive(Clone)]
pub struct PilotConfig {
    pub mode: ExecutionMode,
    pub underlying: String,
    pub bias: StrategyBias,
    pub product: ProductType,
    pub lots: u32,
    pub target_net_rupees: Option<f64>,
    pub max_loss_rupees: Option<f64>,
    pub invalidation_spot_rs: Option<f64>,
    pub min_margin_per_lot_rs: f64,
    pub poll_interval: Duration,
    pub order_wait_timeout: Duration,
    pub order_poll_step: Duration,
    pub entry_slippage_pts: f64,
    pub exit_slippage_pts: f64,
    pub chain_window_pts: f64,
    pub allow_new_entry: bool,
    pub bypass_market_hours: bool,
    pub spot_token: String,
    pub account_id: Option<String>,
    pub config_file: PathBuf,
    pub audit_dir: PathBuf,
}

impl fmt::Debug for PilotConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PilotConfig")
            .field("mode", &self.mode)
            .field("underlying", &self.underlying)
            .field("bias", &self.bias)
            .field("product", &self.product)
            .field("lots", &self.lots)
            .field("allow_new_entry", &self.allow_new_entry)
            .field("spot_token", &self.spot_token)
            .field(
                "account_id",
                &self.account_id.as_ref().map(|_| "<redacted>"),
            )
            .field("config_file", &self.config_file)
            .field("audit_dir", &self.audit_dir)
            .finish()
    }
}

impl Default for PilotConfig {
    fn default() -> Self {
        Self {
            mode: ExecutionMode::Paper,
            underlying: "NIFTY".to_owned(),
            bias: StrategyBias::Auto,
            product: ProductType::Margin,
            lots: 1,
            target_net_rupees: None,
            max_loss_rupees: None,
            invalidation_spot_rs: None,
            min_margin_per_lot_rs: 50_000.0,
            poll_interval: Duration::from_secs(15),
            order_wait_timeout: Duration::from_millis(4_000),
            order_poll_step: Duration::from_millis(400),
            entry_slippage_pts: 0.50,
            exit_slippage_pts: 2.00,
            chain_window_pts: 1_000.0,
            allow_new_entry: true,
            bypass_market_hours: false,
            spot_token: "26000".to_owned(),
            account_id: None,
            config_file: PathBuf::from(".env"),
            audit_dir: PathBuf::from("logs"),
        }
    }
}

impl PilotConfig {
    pub fn load(mode_override: Option<ExecutionMode>) -> Result<Self, ConfigLoadError> {
        let config_file = crate::oauth::default_config_path();
        let file_vars = read_dotenv_map(&config_file)?;
        let audit_dir = std::env::var_os("NIFTY_AUDIT_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                file_vars
                    .get("NIFTY_AUDIT_DIR")
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| PathBuf::from(s.trim()))
            })
            .unwrap_or_else(|| PathBuf::from("logs"));

        Self::from_lookup(
            mode_override,
            config_file,
            audit_dir,
            |key| match std::env::var(key) {
                Ok(val) => Ok(Some(val)),
                Err(VarError::NotPresent) => Ok(file_vars.get(key).cloned()),
                Err(VarError::NotUnicode(_)) => {
                    if key == "NIFTY_EXECUTION_MODE" {
                        Err(ConfigLoadError::InvalidExecutionMode(
                            "NIFTY_EXECUTION_MODE contains invalid Unicode.",
                        ))
                    } else {
                        Err(ConfigLoadError::Message(format!(
                            "{key} contains invalid Unicode."
                        )))
                    }
                }
            },
        )
    }

    pub fn from_lookup<F>(
        mode_override: Option<ExecutionMode>,
        config_file: PathBuf,
        audit_dir: PathBuf,
        lookup: F,
    ) -> Result<Self, ConfigLoadError>
    where
        F: Fn(&str) -> Result<Option<String>, ConfigLoadError>,
    {
        let mode = if let Some(m) = mode_override {
            m
        } else {
            match lookup("NIFTY_EXECUTION_MODE")? {
                Some(raw) => raw
                    .trim()
                    .parse::<ExecutionMode>()
                    .map_err(ConfigLoadError::InvalidExecutionMode)?,
                None => ExecutionMode::default(),
            }
        };

        let bias = match lookup("NIFTY_STRATEGY_BIAS")? {
            Some(raw) if !raw.trim().is_empty() => raw
                .parse::<StrategyBias>()
                .map_err(|e| ConfigLoadError::Message(e.to_owned()))?,
            _ => StrategyBias::Auto,
        };

        let product = match lookup("NIFTY_PRODUCT")? {
            Some(raw) if !raw.trim().is_empty() => match raw.trim().to_ascii_uppercase().as_str() {
                "M" | "NRML" | "MARGIN" => ProductType::Margin,
                "I" | "MIS" | "INTRADAY" => ProductType::Intraday,
                _ => {
                    return Err(ConfigLoadError::Message(
                        "NIFTY_PRODUCT must be 'M'/'NRML' or 'I'/'MIS'.".to_owned(),
                    ));
                }
            },
            _ => ProductType::Margin,
        };

        let parse_opt_f64 = |key: &str| -> Result<Option<f64>, ConfigLoadError> {
            match lookup(key)? {
                Some(raw) if !raw.trim().is_empty() => {
                    let v = raw.trim().parse::<f64>().map_err(|_| {
                        ConfigLoadError::Message(format!("{key} must be a valid number."))
                    })?;
                    if !v.is_finite() || v <= 0.0 {
                        return Err(ConfigLoadError::Message(format!(
                            "{key} must be a positive finite number."
                        )));
                    }
                    Ok(Some(v))
                }
                _ => Ok(None),
            }
        };

        let lots = match lookup("NIFTY_LOTS")? {
            Some(raw) if !raw.trim().is_empty() => {
                let n = raw.trim().parse::<u32>().map_err(|_| {
                    ConfigLoadError::Message("NIFTY_LOTS must be a positive integer.".to_owned())
                })?;
                if !(1..=100).contains(&n) {
                    return Err(ConfigLoadError::Message(
                        "NIFTY_LOTS must be between 1 and 100.".to_owned(),
                    ));
                }
                n
            }
            _ => 1,
        };

        let poll_secs = match lookup("NIFTY_POLL_SECS")? {
            Some(raw) if !raw.trim().is_empty() => raw
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|&s| (2..=300).contains(&s))
                .ok_or_else(|| {
                    ConfigLoadError::Message("NIFTY_POLL_SECS must be 2..=300.".to_owned())
                })?,
            _ => 15,
        };

        let order_wait_ms = match lookup("NIFTY_ORDER_WAIT_MS")? {
            Some(raw) if !raw.trim().is_empty() => raw
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|&ms| (500..=30_000).contains(&ms))
                .ok_or_else(|| {
                    ConfigLoadError::Message("NIFTY_ORDER_WAIT_MS must be 500..=30000.".to_owned())
                })?,
            _ => 4_000,
        };

        let allow_new_entry = match lookup("NIFTY_ALLOW_NEW_ENTRY")? {
            Some(raw) if !raw.trim().is_empty() => match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" => true,
                "0" | "false" | "no" => false,
                _ => {
                    return Err(ConfigLoadError::Message(
                        "NIFTY_ALLOW_NEW_ENTRY must be true or false.".to_owned(),
                    ));
                }
            },
            _ => true,
        };

        let spot_token = lookup("NIFTY_QUOTE_TOKEN")?
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "26000".to_owned());
        if !spot_token.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ConfigLoadError::Message(
                "NIFTY_QUOTE_TOKEN must contain only digits.".to_owned(),
            ));
        }

        let account_id = lookup("SHOONYA_ACCOUNT_ID")?
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        if let Some(ref act) = account_id
            && (act.len() > 64 || !act.bytes().all(|b| b.is_ascii_alphanumeric()))
        {
            return Err(ConfigLoadError::Message(
                "SHOONYA_ACCOUNT_ID must be 1..=64 ASCII alphanumeric characters.".to_owned(),
            ));
        }

        let defaults = Self::default();
        Ok(Self {
            mode,
            underlying: defaults.underlying,
            bias,
            product,
            lots,
            target_net_rupees: parse_opt_f64("NIFTY_TARGET_NET_RUPEES")?,
            max_loss_rupees: parse_opt_f64("NIFTY_MAX_LOSS_RUPEES")?,
            invalidation_spot_rs: parse_opt_f64("NIFTY_INVALIDATION_SPOT")?,
            min_margin_per_lot_rs: parse_opt_f64("NIFTY_MIN_MARGIN_PER_LOT_RUPEES")?
                .unwrap_or(defaults.min_margin_per_lot_rs),
            poll_interval: Duration::from_secs(poll_secs),
            order_wait_timeout: Duration::from_millis(order_wait_ms),
            order_poll_step: defaults.order_poll_step,
            entry_slippage_pts: parse_opt_f64("NIFTY_ENTRY_SLIPPAGE_PTS")?
                .unwrap_or(defaults.entry_slippage_pts),
            exit_slippage_pts: parse_opt_f64("NIFTY_EXIT_SLIPPAGE_PTS")?
                .unwrap_or(defaults.exit_slippage_pts),
            chain_window_pts: parse_opt_f64("NIFTY_CHAIN_WINDOW_PTS")?
                .unwrap_or(defaults.chain_window_pts),
            allow_new_entry,
            bypass_market_hours: false,
            spot_token,
            account_id,
            config_file,
            audit_dir,
        })
    }
}

/// Resolves a single non-secret configuration variable (`process env > .env`).
pub fn resolve_config_var(
    config_file: &Path,
    key: &str,
) -> Result<Option<String>, ConfigLoadError> {
    match std::env::var(key) {
        Ok(val) => Ok(Some(val)),
        Err(VarError::NotPresent) => {
            let map = read_dotenv_map(config_file)?;
            Ok(map.get(key).cloned())
        }
        Err(VarError::NotUnicode(_)) => Err(ConfigLoadError::Message(format!(
            "{key} contains invalid Unicode."
        ))),
    }
}

/// Reads non-secret pilot settings from `.env` while zeroizing secret keys immediately.
pub fn read_dotenv_map(path: &Path) -> Result<HashMap<String, String>, ConfigLoadError> {
    let Ok(file) = File::open(path) else {
        return Ok(HashMap::new());
    };
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_ENV_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ConfigLoadError::Message("Cannot read .env configuration.".to_owned()))?;
    if bytes.len() as u64 > MAX_ENV_FILE_BYTES {
        return Err(ConfigLoadError::Message(
            ".env configuration exceeds maximum size.".to_owned(),
        ));
    }
    let mut map = HashMap::new();
    for item in dotenvy::from_read_iter(Cursor::new(bytes.as_slice())) {
        let (k, v) = item
            .map_err(|_| ConfigLoadError::Message("Malformed syntax in .env file.".to_owned()))?;
        let kz = Zeroizing::new(k);
        let vz = Zeroizing::new(v);
        if kz.starts_with("NIFTY_")
            || matches!(kz.as_str(), "SHOONYA_ACCOUNT_ID" | "SHOONYA_USER_ID")
        {
            map.insert(kz.as_str().to_owned(), vz.as_str().to_owned());
        }
    }
    Ok(map)
}

/// Exclusive single-instance kernel advisory lock (`File::try_lock`), automatically
/// released by the OS on normal drop, panic, `SIGTERM`, `SIGKILL`, or reboot.
pub struct ProcessLock {
    _file: File,
}

impl ProcessLock {
    pub fn acquire(audit_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(audit_dir)
            .map_err(|e| format!("Cannot create lock directory: {e}"))?;
        let path = audit_dir.join("nifty-spread-pilot.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .map_err(|e| format!("Cannot open lock file {}: {e}", path.display()))?;
        file.try_lock().map_err(|_| {
            format!(
                "Another nifty-spread-pilot instance holds active lock {}.",
                path.display()
            )
        })?;
        let _ = file.set_len(0);
        let _ = file.seek(SeekFrom::Start(0));
        let _ = writeln!(file, "pid={}", std::process::id());
        let _ = file.flush();
        Ok(Self { _file: file })
    }
}

#[derive(Debug, Clone)]
pub struct ActiveSpread {
    pub short_contract: OptionContract,
    pub long_contract: OptionContract,
    pub product: ProductType,
    pub qty: u32,
    pub excess_long_qty: u32,
    pub short_entry_paise: i64,
    pub long_entry_paise: i64,
}

#[derive(Debug, Clone)]
pub enum ReconciledState {
    Flat,
    ActiveMatchedSpread(ActiveSpread),
    LongOnlyOrphan {
        contract: OptionContract,
        product: ProductType,
        qty: u32,
    },
    UncoveredOrUnrecognized {
        short_qty: u32,
        long_qty: u32,
        reason: &'static str,
    },
}

impl ReconciledState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::ActiveMatchedSpread(_) => "active_spread",
            Self::LongOnlyOrphan { .. } => "long_only_orphan",
            Self::UncoveredOrUnrecognized { .. } => "uncovered_or_unrecognized",
        }
    }
}

pub fn compute_t_days_ist(expiry: time::Date, now_ist: OffsetDateTime) -> f64 {
    let close_time = Time::from_hms(15, 30, 0).unwrap();
    let expiry_dt = now_ist.replace_date(expiry).replace_time(close_time);
    let secs = (expiry_dt - now_ist).whole_seconds().max(60);
    (secs as f64) / 86_400.0
}

pub fn is_market_open_ist(now_ist: OffsetDateTime) -> bool {
    if matches!(now_ist.weekday(), Weekday::Saturday | Weekday::Sunday) {
        return false;
    }
    let open = Time::from_hms(9, 15, 0).unwrap();
    let close = Time::from_hms(15, 30, 0).unwrap();
    now_ist.time() >= open && now_ist.time() < close
}

pub fn is_entry_window_open_ist(now_ist: OffsetDateTime) -> bool {
    if matches!(now_ist.weekday(), Weekday::Saturday | Weekday::Sunday) {
        return false;
    }
    let start = Time::from_hms(9, 20, 0).unwrap();
    let end = Time::from_hms(15, 0, 0).unwrap();
    now_ist.time() >= start && now_ist.time() <= end
}

pub fn is_square_off_due_ist(
    product: ProductType,
    expiry: time::Date,
    now_ist: OffsetDateTime,
) -> bool {
    let cutoff = Time::from_hms(15, 15, 0).unwrap();
    (product == ProductType::Intraday || expiry == now_ist.date()) && now_ist.time() >= cutoff
}

#[derive(Debug, Clone)]
struct ArmedGuardianState {
    short_symbol: String,
    long_symbol: String,
    product: ProductType,
    qty: u32,
    profit_lock_active: bool,
    profit_lock_max_debit_pts: f64,
    profit_lock_stop_forward_rs: f64,
    was_underwater: bool,
    damage_cap_max_debit_pts: Option<f64>,
}

impl ArmedGuardianState {
    fn matches(&self, active: &ActiveSpread) -> bool {
        self.short_symbol == active.short_contract.trading_symbol
            && self.long_symbol == active.long_contract.trading_symbol
            && self.product == active.product
            && self.qty == active.qty
    }

    fn to_json(&self) -> Value {
        let stop_fwd = if self.profit_lock_stop_forward_rs.is_finite() {
            json!(self.profit_lock_stop_forward_rs)
        } else {
            Value::Null
        };
        json!({
            "short_symbol": self.short_symbol,
            "long_symbol": self.long_symbol,
            "product": match self.product {
                ProductType::Margin => "M",
                ProductType::Intraday => "I",
                ProductType::Other => "O",
            },
            "qty": self.qty,
            "profit_lock_active": self.profit_lock_active,
            "profit_lock_max_debit_pts": self.profit_lock_max_debit_pts,
            "profit_lock_stop_forward_rs": stop_fwd,
            "was_underwater": self.was_underwater,
            "damage_cap_max_debit_pts": self.damage_cap_max_debit_pts,
        })
    }

    fn from_json(val: &Value) -> Option<Self> {
        let product = match val.get("product")?.as_str()? {
            "M" => ProductType::Margin,
            "I" => ProductType::Intraday,
            _ => return None,
        };
        let qty = u32::try_from(val.get("qty")?.as_u64()?).ok()?;
        let stop_fwd = match val.get("profit_lock_stop_forward_rs") {
            Some(v) if v.is_null() => f64::INFINITY,
            Some(v) => v.as_f64()?,
            None => f64::INFINITY,
        };
        Some(Self {
            short_symbol: val.get("short_symbol")?.as_str()?.to_owned(),
            long_symbol: val.get("long_symbol")?.as_str()?.to_owned(),
            product,
            qty,
            profit_lock_active: val.get("profit_lock_active")?.as_bool()?,
            profit_lock_max_debit_pts: val.get("profit_lock_max_debit_pts")?.as_f64()?,
            profit_lock_stop_forward_rs: stop_fwd,
            was_underwater: val.get("was_underwater")?.as_bool()?,
            damage_cap_max_debit_pts: val.get("damage_cap_max_debit_pts").and_then(Value::as_f64),
        })
    }
}

pub struct PilotEngine<B> {
    broker: B,
    config: PilotConfig,
    armed_guardian: Option<ArmedGuardianState>,
    guardian_load_error: Option<String>,
}

impl<B: Broker> PilotEngine<B> {
    pub fn new(broker: B, config: PilotConfig) -> Self {
        let (armed_guardian, guardian_load_error) = match load_guardian_state(&config.audit_dir) {
            Ok(state) => (state, None),
            Err(error) => (None, Some(error)),
        };
        Self {
            broker,
            config,
            armed_guardian,
            guardian_load_error,
        }
    }

    pub fn broker_mut(&mut self) -> &mut B {
        &mut self.broker
    }

    fn set_armed_guardian(&mut self, next: Option<ArmedGuardianState>) -> Result<(), String> {
        self.armed_guardian = next;
        persist_guardian_state(&self.config.audit_dir, self.armed_guardian.as_ref())
            .map_err(|e| format!("Fail-closed halt: cannot persist guardian state: {e}"))
    }

    /// Cancels an active order and polls `OrderBook` until terminal (`!is_active()`),
    /// returning the authoritative final [`BrokerOrder`]. Fails closed if still active.
    fn cancel_and_confirm_terminal(&mut self, order_id: &str) -> Result<BrokerOrder, String> {
        self.broker
            .cancel_order(order_id)
            .map_err(|e| format!("CancelOrder failed for {order_id}: {e}"))?;
        let started = std::time::Instant::now();
        loop {
            let orders = self
                .broker
                .get_orders()
                .map_err(|e| format!("Post-cancel OrderBook check failed for {order_id}: {e}"))?;
            match orders.iter().find(|o| o.order_id == order_id) {
                Some(ord) if !ord.status.is_active() => return Ok(ord.clone()),
                None => {
                    return Err(format!(
                        "Cancelled order {order_id} disappeared from OrderBook before terminal confirmation."
                    ));
                }
                _ => {}
            }
            if started.elapsed() >= self.config.order_wait_timeout {
                return Err(format!(
                    "Order {order_id} remained active in OrderBook after cancel timeout."
                ));
            }
            if !self.config.order_poll_step.is_zero() {
                std::thread::sleep(self.config.order_poll_step);
            }
        }
    }

    /// Reconciles live positions and active orders for `self.config.underlying`.
    pub fn reconcile(
        &mut self,
        audit: &mut AuditLog,
        now_ist: OffsetDateTime,
    ) -> Result<(ReconciledState, Vec<BrokerPosition>, Vec<BrokerOrder>), String> {
        let positions = self
            .broker
            .get_positions()
            .map_err(|e| format!("PositionBook failed: {e}"))?;
        let orders = self
            .broker
            .get_orders()
            .map_err(|e| format!("OrderBook failed: {e}"))?;

        let prefix = self.config.underlying.clone();
        let open_pos: Vec<BrokerPosition> = positions
            .into_iter()
            .filter(|p| {
                p.net_qty != 0
                    && self.broker.matches_option_underlying(
                        &p.exchange,
                        &p.trading_symbol,
                        &prefix,
                    )
            })
            .collect();
        let active_ords: Vec<BrokerOrder> = orders
            .into_iter()
            .filter(|o| {
                o.status.is_active()
                    && self.broker.matches_option_underlying(
                        &o.exchange,
                        &o.trading_symbol,
                        &prefix,
                    )
            })
            .collect();

        if open_pos.is_empty() && active_ords.is_empty() {
            let state = ReconciledState::Flat;
            log_event(
                audit,
                AuditEvent::Reconciled {
                    state: state.label(),
                    open_positions: 0,
                    active_orders: 0,
                },
            )?;
            return Ok((state, open_pos, active_ords));
        }

        let short_qty_total: u32 = open_pos
            .iter()
            .filter(|p| p.net_qty < 0)
            .map(|p| p.net_qty.unsigned_abs())
            .sum();
        let long_qty_total: u32 = open_pos
            .iter()
            .filter(|p| p.net_qty > 0)
            .map(|p| p.net_qty as u32)
            .sum();

        let symbols: Vec<String> = open_pos.iter().map(|p| p.trading_symbol.clone()).collect();
        let resolved = if symbols.is_empty() {
            Vec::new()
        } else {
            self.broker
                .resolve_contracts_by_symbols(&self.config.underlying, &symbols, now_ist)
                .map_err(|e| format!("Contract resolution failed: {e}"))?
        };
        let by_sym: HashMap<&str, &OptionContract> = resolved
            .iter()
            .map(|c| (c.trading_symbol.as_str(), c))
            .collect();

        let state = match open_pos.as_slice() {
            [] => ReconciledState::UncoveredOrUnrecognized {
                short_qty: 0,
                long_qty: 0,
                reason: "active resting orders without open position",
            },
            [single] if single.net_qty > 0 && single.product != ProductType::Other => {
                if let Some(&contract) = by_sym.get(single.trading_symbol.as_str()) {
                    ReconciledState::LongOnlyOrphan {
                        contract: contract.clone(),
                        product: single.product,
                        qty: single.net_qty as u32,
                    }
                } else {
                    ReconciledState::UncoveredOrUnrecognized {
                        short_qty: 0,
                        long_qty: single.net_qty as u32,
                        reason: "orphan long contract missing from NFO master",
                    }
                }
            }
            [a, b] => {
                let (short_pos, long_pos) = if a.net_qty < 0 && b.net_qty > 0 {
                    (Some(a), Some(b))
                } else if b.net_qty < 0 && a.net_qty > 0 {
                    (Some(b), Some(a))
                } else {
                    (None, None)
                };
                match (short_pos, long_pos) {
                    (Some(sp), Some(lp)) => {
                        let s_qty = sp.net_qty.unsigned_abs();
                        let l_qty = lp.net_qty as u32;
                        let sc = by_sym.get(sp.trading_symbol.as_str()).copied();
                        let lc = by_sym.get(lp.trading_symbol.as_str()).copied();
                        match (sc, lc) {
                            (Some(sc), Some(lc))
                                if s_qty > 0
                                    && s_qty <= l_qty
                                    && sp.product == lp.product
                                    && sp.product != ProductType::Other
                                    && sc.expiry == lc.expiry
                                    && sc.option_kind == lc.option_kind
                                    && valid_credit_strikes(
                                        sc.option_kind,
                                        sc.strike_paise,
                                        lc.strike_paise,
                                    )
                                    && sp.sell_avg_paise.unwrap_or(0)
                                        > lp.buy_avg_paise.unwrap_or(0)
                                    && lp.buy_avg_paise.unwrap_or(0) > 0 =>
                            {
                                ReconciledState::ActiveMatchedSpread(ActiveSpread {
                                    short_contract: sc.clone(),
                                    long_contract: lc.clone(),
                                    product: sp.product,
                                    qty: s_qty,
                                    excess_long_qty: l_qty - s_qty,
                                    short_entry_paise: sp.sell_avg_paise.unwrap_or(0),
                                    long_entry_paise: lp.buy_avg_paise.unwrap_or(0),
                                })
                            }
                            _ => ReconciledState::UncoveredOrUnrecognized {
                                short_qty: short_qty_total,
                                long_qty: long_qty_total,
                                reason: "two-leg position does not form a matched credit vertical",
                            },
                        }
                    }
                    _ => ReconciledState::UncoveredOrUnrecognized {
                        short_qty: short_qty_total,
                        long_qty: long_qty_total,
                        reason: "two same-direction legs detected",
                    },
                }
            }
            _ => ReconciledState::UncoveredOrUnrecognized {
                short_qty: short_qty_total,
                long_qty: long_qty_total,
                reason: "unrecognized or uncovered NIFTY option exposure",
            },
        };

        log_event(
            audit,
            AuditEvent::Reconciled {
                state: state.label(),
                open_positions: open_pos.len(),
                active_orders: active_ords.len(),
            },
        )?;
        if let ReconciledState::UncoveredOrUnrecognized {
            short_qty,
            long_qty,
            ..
        } = &state
        {
            log_event(
                audit,
                AuditEvent::UncoveredExposureDetected {
                    short_qty: *short_qty,
                    long_qty: *long_qty,
                },
            )?;
        }

        Ok((state, open_pos, active_ords))
    }

    /// Read-only `--scan`: evaluates the live option chain and prints top spread setups.
    pub fn run_scan(
        &mut self,
        audit: &mut AuditLog,
        now_ist: OffsetDateTime,
    ) -> Result<String, String> {
        log_event(
            audit,
            AuditEvent::ScanStarted {
                broker: self.broker.name(),
            },
        )?;
        let result = self.scan_inner(now_ist, false);
        match result {
            Ok((spot_paise, ranked, expiry)) => {
                let forward_paise =
                    rupees_to_paise(ranked.diagnostics.implied_forward_rs).unwrap_or(spot_paise);
                log_event(
                    audit,
                    AuditEvent::ScanCompleted {
                        spot_paise,
                        forward_paise,
                        candidates_qualified: ranked.qualified.len(),
                        recommended: ranked.recommended.is_some(),
                    },
                )?;
                Ok(format_scan_report(
                    self.broker.name(),
                    expiry,
                    &ranked,
                    self.config.lots,
                ))
            }
            Err(err) => {
                let _ = log_event(audit, AuditEvent::ScanFailed);
                Err(err)
            }
        }
    }

    /// Read-only `--status`: fetches live margin, positions, orders, and 3-part tactical guardian dashboard.
    pub fn run_status(
        &mut self,
        audit: &mut AuditLog,
        now_ist: OffsetDateTime,
    ) -> Result<String, String> {
        log_event(
            audit,
            AuditEvent::StatusCheckStarted {
                broker: self.broker.name(),
            },
        )?;
        let res = (|| -> Result<String, String> {
            self.broker
                .ensure_session()
                .map_err(|e| format!("Session error: {e}"))?;
            let limits = self
                .broker
                .get_account_limits()
                .map_err(|e| format!("Limits error: {e}"))?;
            let spot_quote = self
                .broker
                .get_spot_quote(&self.config.underlying)
                .map_err(|e| format!("Spot quote error: {e}"))?;
            let (spot_paise, _) = spot_quote
                .reference_price_paise(true)
                .ok_or("Spot quote contains no usable price.")?;
            let spot_rs = paise_to_rupees(spot_paise);

            let (state, open_pos, active_ords) = self.reconcile(audit, now_ist)?;
            log_event(
                audit,
                AuditEvent::StatusCheckCompleted {
                    available_margin_paise: limits.available_margin_paise(),
                    open_positions: open_pos.len(),
                    active_orders: active_ords.len(),
                },
            )?;

            let mut out = format!(
                "=== NIFTY SPREAD PILOT STATUS ({}) ===\n\
                 Account Available Margin: Rs {:.2} (Cash Rs {:.2} | Collateral Rs {:.2} | Used Rs {:.2})\n\
                 NIFTY Spot: {:.2} | Open Positions: {} | Active Orders: {} | State: {}\n",
                self.broker.name().to_ascii_uppercase(),
                paise_to_rupees(limits.available_margin_paise()),
                paise_to_rupees(limits.cash_paise),
                paise_to_rupees(limits.collateral_paise),
                paise_to_rupees(limits.margin_used_paise),
                spot_rs,
                open_pos.len(),
                active_ords.len(),
                state.label()
            );

            match state {
                ReconciledState::Flat => {
                    out.push_str("Position Status: FLAT (no active NIFTY spread).\n");
                }
                ReconciledState::ActiveMatchedSpread(active) => {
                    let eval = self.evaluate_active_spread(&active, spot_rs, now_ist, true)?;
                    log_event(
                        audit,
                        AuditEvent::SpreadEvaluated {
                            regime: eval.oco_regime.as_str(),
                            verdict: eval.verdict.as_str(),
                            net_pnl_paise: rupees_to_paise(eval.net_pnl_rs).unwrap_or(0),
                        },
                    )?;
                    out.push('\n');
                    out.push_str(&format_evaluation_report(&active, &eval));
                }
                ReconciledState::LongOnlyOrphan {
                    contract,
                    qty,
                    product,
                } => {
                    out.push_str(&format!(
                        "Orphan Long Hedge Detected: {} ({product:?}) +{qty} qty — run `--once` or `--live` to unwind.\n",
                        contract.trading_symbol
                    ));
                }
                ReconciledState::UncoveredOrUnrecognized {
                    short_qty,
                    long_qty,
                    reason,
                } => {
                    out.push_str(&format!(
                        "ALERT — HALTED ({reason}): short_qty={short_qty}, long_qty={long_qty}. Resolve manually.\n"
                    ));
                }
            }
            Ok(out)
        })();

        if res.is_err() {
            let _ = log_event(audit, AuditEvent::StatusCheckFailed);
        }
        res
    }

    /// Executes one complete autonomous reconciliation + guardian / entry cycle.
    pub fn run_cycle(
        &mut self,
        audit: &mut AuditLog,
        now_ist: OffsetDateTime,
    ) -> Result<String, String> {
        if let Some(error) = &self.guardian_load_error {
            return Err(format!(
                "Fail-closed halt: cannot load guardian state: {error}"
            ));
        }
        self.broker
            .ensure_session()
            .map_err(|e| format!("Session authentication failed: {e}"))?;

        let (state, open_pos, active_ords) = self.reconcile(audit, now_ist)?;
        match state {
            ReconciledState::UncoveredOrUnrecognized {
                short_qty,
                long_qty,
                reason,
            } => {
                self.set_armed_guardian(None)?;
                // If flat with ONLY our own stale pilot ("nsp*") resting orders, cancel & confirm
                // terminal so the daemon recovers cleanly on the next cycle instead of halting!
                let all_pilot_orders = !active_ords.is_empty()
                    && active_ords
                        .iter()
                        .all(|o| is_pilot_tag(o.client_tag.as_deref()));
                if open_pos.is_empty() && all_pilot_orders {
                    let mut all_confirmed = true;
                    for ord in &active_ords {
                        if self.cancel_and_confirm_terminal(&ord.order_id).is_err() {
                            all_confirmed = false;
                        }
                    }
                    if all_confirmed {
                        return Err(
                            "Cancelled stale pilot resting order(s) while flat; retrying cleanly next cycle."
                                .to_owned(),
                        );
                    }
                }
                // Cancel only orders tagged by this pilot ("nsp*") so manual positions aren't disturbed.
                for ord in &active_ords {
                    if is_pilot_tag(ord.client_tag.as_deref()) {
                        let _ = self.broker.cancel_order(&ord.order_id);
                    }
                }
                let _ = log_event(
                    audit,
                    AuditEvent::CycleFailed {
                        stage: "uncovered_or_unrecognized_state",
                    },
                );
                Err(format!(
                    "Fail-closed halt: {reason} (short_qty={short_qty}, long_qty={long_qty})."
                ))
            }
            ReconciledState::LongOnlyOrphan {
                contract,
                product,
                qty,
            } => {
                self.set_armed_guardian(None)?;
                if !self.config.bypass_market_hours && !is_market_open_ist(now_ist) {
                    log_event(audit, AuditEvent::MarketClosedSkip)?;
                    return Ok(format!(
                        "Market closed; orphan long {} (+{qty}) deferred until market open.",
                        contract.trading_symbol
                    ));
                }
                // Cancel & confirm terminal for ALL pilot orders AND any order on the orphan symbol
                // before selling the hedge so a resting short-sell can never fill into a naked short!
                for ord in &active_ords {
                    if is_pilot_tag(ord.client_tag.as_deref())
                        || ord.trading_symbol == contract.trading_symbol
                    {
                        self.cancel_and_confirm_terminal(&ord.order_id)?;
                    }
                }
                let closed = self.unwind_long_leg(&contract, product, qty, audit)?;
                Ok(format!(
                    "Unwound orphan long hedge {} ({closed}/{qty} filled).",
                    contract.trading_symbol
                ))
            }
            ReconciledState::ActiveMatchedSpread(active) => {
                if !self.config.bypass_market_hours && !is_market_open_ist(now_ist) {
                    log_event(audit, AuditEvent::MarketClosedSkip)?;
                    return Ok(format!(
                        "Market closed; holding matched spread -{} / +{} ({} qty).",
                        active.short_contract.trading_symbol,
                        active.long_contract.trading_symbol,
                        active.qty
                    ));
                }
                self.guard_active_spread(&active, &active_ords, audit, now_ist)
            }
            ReconciledState::Flat => {
                self.set_armed_guardian(None)?;
                if !self.config.allow_new_entry {
                    return Ok(
                        "Account flat; NIFTY_ALLOW_NEW_ENTRY=false (guardian-only mode)."
                            .to_owned(),
                    );
                }
                if !self.config.bypass_market_hours && !is_entry_window_open_ist(now_ist) {
                    log_event(audit, AuditEvent::MarketClosedSkip)?;
                    return Ok(
                        "Account flat; outside NSE entry window (09:20-15:00 IST Mon-Fri)."
                            .to_owned(),
                    );
                }
                self.scan_and_enter(audit, now_ist)
            }
        }
    }

    pub fn run_loop(&mut self, audit: &mut AuditLog) -> Result<(), String> {
        self.run_bounded_loop(audit, None)
    }

    pub fn run_bounded_loop(
        &mut self,
        audit: &mut AuditLog,
        max_cycles: Option<usize>,
    ) -> Result<(), String> {
        log_event(
            audit,
            AuditEvent::ModeSelected {
                mode: self.config.mode,
                broker: self.broker.name(),
            },
        )?;
        let _lock = ProcessLock::acquire(&self.config.audit_dir)?;
        println!(
            "Starting NIFTY Spread Pilot loop | mode={:?} | broker={} | poll={}s | bias={:?} | lots={}",
            self.config.mode,
            self.broker.name(),
            self.config.poll_interval.as_secs(),
            self.config.bias,
            self.config.lots
        );
        let mut completed = 0_usize;
        loop {
            let now_ist = crate::market::ist_now()?;
            match self.run_cycle(audit, now_ist) {
                Ok(summary) => {
                    println!("[{}] {summary}", now_ist);
                }
                Err(err) => {
                    eprintln!("[{}] ERROR: {err}", now_ist);
                    if err.starts_with("Fail-closed halt:") {
                        return Err(err);
                    }
                }
            }
            completed += 1;
            if max_cycles.is_some_and(|limit| completed >= limit) {
                return Ok(());
            }
            if !self.config.poll_interval.is_zero() {
                std::thread::sleep(self.config.poll_interval);
            }
        }
    }

    fn scan_inner(
        &mut self,
        now_ist: OffsetDateTime,
        require_executable_touch: bool,
    ) -> Result<(i64, RankOutcome, time::Date), String> {
        self.broker
            .ensure_session()
            .map_err(|e| format!("Session error: {e}"))?;
        let spot_quote = self
            .broker
            .get_spot_quote(&self.config.underlying)
            .map_err(|e| format!("Spot quote failed: {e}"))?;
        let (spot_paise, _) = spot_quote
            .reference_price_paise(!require_executable_touch)
            .ok_or("Spot quote unavailable.")?;
        let spot_rs = paise_to_rupees(spot_paise);

        let chain = self
            .broker
            .discover_option_chain_contracts(&self.config.underlying, now_ist)
            .map_err(|e| format!("Contract discovery failed: {e}"))?;
        let window_paise = rupees_to_paise(self.config.chain_window_pts).unwrap_or(100_000);
        let filtered: Vec<OptionContract> = chain
            .contracts
            .into_iter()
            .filter(|c| (c.strike_paise - spot_paise).abs() <= window_paise)
            .collect();
        if filtered.len() < 4 {
            return Err(
                "Insufficient option contracts within strike window around spot.".to_owned(),
            );
        }

        let quotes = self
            .broker
            .get_option_quotes(&filtered)
            .map_err(|e| format!("Option chain quotes failed: {e}"))?;
        let t_days = compute_t_days_ist(chain.expiry, now_ist);
        let ranked = rank_candidate_spreads(
            spot_rs,
            t_days,
            self.config.bias,
            self.config.lots,
            &quotes,
            self.broker.fee_schedule(),
            require_executable_touch,
        )
        .map_err(str::to_owned)?;

        Ok((spot_paise, ranked, chain.expiry))
    }

    fn scan_and_enter(
        &mut self,
        audit: &mut AuditLog,
        now_ist: OffsetDateTime,
    ) -> Result<String, String> {
        log_event(
            audit,
            AuditEvent::ScanStarted {
                broker: self.broker.name(),
            },
        )?;
        let (spot_paise, ranked, _) = match self.scan_inner(now_ist, true) {
            Ok(v) => v,
            Err(e) => {
                let _ = log_event(audit, AuditEvent::ScanFailed);
                let _ = log_event(
                    audit,
                    AuditEvent::CycleFailed {
                        stage: "scan_for_entry",
                    },
                );
                return Err(e);
            }
        };
        let forward_paise =
            rupees_to_paise(ranked.diagnostics.implied_forward_rs).unwrap_or(spot_paise);
        log_event(
            audit,
            AuditEvent::ScanCompleted {
                spot_paise,
                forward_paise,
                candidates_qualified: ranked.qualified.len(),
                recommended: ranked.recommended.is_some(),
            },
        )?;

        let Some(best) = ranked.recommended else {
            log_event(
                audit,
                AuditEvent::NoPositionRecommended {
                    evaluated_pairs: ranked.evaluated_pairs,
                },
            )?;
            return Ok(format!(
                "NO_POSITION_RECOMMENDED ({} pairs evaluated; none passed CORE_INCOME or HIGH_POP_1SD gates).",
                ranked.evaluated_pairs
            ));
        };

        // Pre-entry free-margin verification.
        let limits = self
            .broker
            .get_account_limits()
            .map_err(|e| format!("Limits check failed before entry: {e}"))?;
        let qty = best
            .short_contract
            .lot_size
            .saturating_mul(self.config.lots);
        let required_margin_rs = (self.config.min_margin_per_lot_rs * f64::from(self.config.lots))
            .max(best.long_entry_rs * f64::from(qty) + best.max_loss_net_rs);
        let avail_rs = paise_to_rupees(limits.available_margin_paise());
        if avail_rs < required_margin_rs {
            return Err(format!(
                "Insufficient available margin for entry: available Rs {avail_rs:.2} < required Rs {required_margin_rs:.2}."
            ));
        }

        self.execute_spread_entry(&best, qty, audit)
    }

    fn execute_spread_entry(
        &mut self,
        candidate: &CandidateSpread,
        target_qty: u32,
        audit: &mut AuditLog,
    ) -> Result<String, String> {
        // Re-fetch fresh top-of-book quotes for the two selected legs immediately before submission.
        let fresh = self
            .broker
            .get_option_quotes(&[
                candidate.long_contract.clone(),
                candidate.short_contract.clone(),
            ])
            .map_err(|e| format!("Pre-entry quote refresh failed: {e}"))?;
        let long_quote = fresh
            .iter()
            .find(|(c, _)| c.trading_symbol == candidate.long_contract.trading_symbol)
            .map(|(_, q)| *q)
            .ok_or("Missing fresh hedge quote.")?;
        let short_quote = fresh
            .iter()
            .find(|(c, _)| c.trading_symbol == candidate.short_contract.trading_symbol)
            .map(|(_, q)| *q)
            .ok_or("Missing fresh short quote.")?;

        let long_touch = long_quote.executable_quote().map_err(str::to_owned)?;
        let short_touch = short_quote.executable_quote().map_err(str::to_owned)?;
        if short_touch.bid <= long_touch.ask {
            return Err("Pre-entry quote spread inverted; aborting entry.".to_owned());
        }

        let slip_paise = rupees_to_paise(self.config.entry_slippage_pts).unwrap_or(50);
        let long_limit_paise = long_quote
            .clamp_limit_paise(
                long_touch.ask.saturating_add(slip_paise),
                candidate.long_contract.tick_paise,
            )
            .map_err(str::to_owned)?;

        // STEP 1: BUY LONG HEDGE FIRST!
        let mut sim_fills = SimulatedFills::default();
        let (confirmed_long_qty, long_fill_paise) = self.submit_and_reconcile_leg(
            &candidate.long_contract,
            OrderSide::Buy,
            self.config.product,
            target_qty,
            long_limit_paise,
            "entry_long_hedge",
            "EL",
            audit,
        )?;
        if confirmed_long_qty == 0 {
            log_event(audit, AuditEvent::EntryAbortedNoHedgeFill)?;
            return Ok(format!(
                "Entry aborted safely: hedge leg {} received 0 fills.",
                candidate.long_contract.trading_symbol
            ));
        }
        sim_fills
            .record_hedge_buy(confirmed_long_qty)
            .map_err(str::to_owned)?;

        // STEP 2: SELL SHORT LEG SECOND — strictly capped at confirmed_long_qty!
        let short_fresh = self
            .broker
            .get_option_quotes(std::slice::from_ref(&candidate.short_contract))
            .map_err(|e| format!("Short quote refresh failed: {e}"))?;
        let sq = short_fresh.first().map(|(_, q)| *q).unwrap_or(short_quote);
        let st = sq.executable_quote().unwrap_or(short_touch);
        let short_limit_paise = sq
            .clamp_limit_paise(
                (st.bid - slip_paise).max(candidate.short_contract.tick_paise),
                candidate.short_contract.tick_paise,
            )
            .map_err(str::to_owned)?;
        let actual_long_cost_paise = long_fill_paise.unwrap_or(long_limit_paise);
        if short_limit_paise <= actual_long_cost_paise {
            let unwound = self.unwind_long_leg(
                &candidate.long_contract,
                self.config.product,
                confirmed_long_qty,
                audit,
            )?;
            if unwound < confirmed_long_qty {
                return Err(format!(
                    "Short quote inverted after hedge fill; partial hedge unwind ({unwound}/{confirmed_long_qty})."
                ));
            }
            return Ok(format!(
                "Short quote inverted after hedge fill; unwound {} hedge qty cleanly.",
                confirmed_long_qty
            ));
        }

        sim_fills
            .record_short_sell(confirmed_long_qty)
            .map_err(str::to_owned)?;
        let (confirmed_short_qty, short_fill_paise) = self.submit_and_reconcile_leg(
            &candidate.short_contract,
            OrderSide::Sell,
            self.config.product,
            confirmed_long_qty,
            short_limit_paise,
            "entry_short",
            "ES",
            audit,
        )?;

        // If short leg filled partially or 0, immediately unwind the excess long hedge!
        if confirmed_short_qty < confirmed_long_qty {
            let excess_long = confirmed_long_qty - confirmed_short_qty;
            let unwound = self.unwind_long_leg(
                &candidate.long_contract,
                self.config.product,
                excess_long,
                audit,
            )?;
            if unwound < excess_long {
                return Err(format!(
                    "Excess hedge unwind partially filled ({unwound}/{excess_long}); remaining excess long will retry next cycle."
                ));
            }
            if confirmed_short_qty == 0 {
                return Ok(format!(
                    "Short leg {} did not fill; unwound {} hedge qty cleanly.",
                    candidate.short_contract.trading_symbol, excess_long
                ));
            }
        }

        let net_credit_paise = short_fill_paise.unwrap_or(short_limit_paise)
            - long_fill_paise.unwrap_or(long_limit_paise);
        let kind_str = match candidate.option_kind {
            OptionKind::Call => "CE",
            OptionKind::Put => "PE",
        };
        log_event(
            audit,
            AuditEvent::EntryCompleted {
                mode: self.config.mode,
                option_kind: kind_str,
                short_strike_paise: candidate.short_contract.strike_paise,
                long_strike_paise: candidate.long_contract.strike_paise,
                qty: confirmed_short_qty,
                net_credit_paise,
            },
        )?;

        Ok(format!(
            "ENTERED {} SPREAD ({}): -{} @ Rs {:.2} / +{} @ Rs {:.2} | qty={} | credit=Rs {:.2} pts",
            kind_str,
            candidate.edge_tier.as_str(),
            candidate.short_contract.trading_symbol,
            paise_to_rupees(short_fill_paise.unwrap_or(short_limit_paise)),
            candidate.long_contract.trading_symbol,
            paise_to_rupees(long_fill_paise.unwrap_or(long_limit_paise)),
            confirmed_short_qty,
            paise_to_rupees(net_credit_paise)
        ))
    }

    fn evaluate_active_spread(
        &mut self,
        active: &ActiveSpread,
        spot_rs: f64,
        now_ist: OffsetDateTime,
        allow_prev_close: bool,
    ) -> Result<LiveSpreadEvaluation, String> {
        let chain = self
            .broker
            .discover_option_chain_contracts(&self.config.underlying, now_ist)
            .ok();
        // Pull nearest ATM CE+PE pair on the SAME expiry for Discounted Put-Call Parity + the two held legs.
        let mut to_fetch = vec![active.short_contract.clone(), active.long_contract.clone()];
        if let Some(ref ch) = chain
            && ch.expiry == active.short_contract.expiry
        {
            let spot_paise = rupees_to_paise(spot_rs).unwrap_or(0);
            if let Some(atm_strike) = ch
                .contracts
                .iter()
                .filter(|c| c.expiry == active.short_contract.expiry)
                .map(|c| c.strike_paise)
                .min_by_key(|s| (s - spot_paise).abs())
            {
                for c in &ch.contracts {
                    if c.expiry == active.short_contract.expiry
                        && c.strike_paise == atm_strike
                        && !to_fetch
                            .iter()
                            .any(|x| x.trading_symbol == c.trading_symbol)
                    {
                        to_fetch.push(c.clone());
                    }
                }
            }
        }

        let quotes = self
            .broker
            .get_option_quotes(&to_fetch)
            .map_err(|e| format!("Active spread quote fetch failed: {e}"))?;
        let sq = quotes
            .iter()
            .find(|(c, _)| c.trading_symbol == active.short_contract.trading_symbol)
            .map(|(_, q)| *q)
            .ok_or("Missing live quote for short leg.")?;
        let lq = quotes
            .iter()
            .find(|(c, _)| c.trading_symbol == active.long_contract.trading_symbol)
            .map(|(_, q)| *q)
            .ok_or("Missing live quote for long leg.")?;

        let t_days = compute_t_days_ist(active.short_contract.expiry, now_ist);
        let forward_rs = crate::strategy::analyze_chain(spot_rs, t_days, &quotes, allow_prev_close)
            .map(|d| d.implied_forward_rs)
            .unwrap_or(spot_rs);

        let intraday_exit_due =
            is_square_off_due_ist(active.product, active.short_contract.expiry, now_ist);

        evaluate_live_spread(LiveEvalInput {
            forward_rs,
            spot_rs,
            t_days,
            option_kind: active.short_contract.option_kind,
            short_strike_rs: paise_to_rupees(active.short_contract.strike_paise),
            long_strike_rs: paise_to_rupees(active.long_contract.strike_paise),
            short_entry_rs: paise_to_rupees(active.short_entry_paise),
            long_entry_rs: paise_to_rupees(active.long_entry_paise),
            short_quote: sq,
            long_quote: lq,
            qty: active.qty,
            target_net_override_rs: self.config.target_net_rupees,
            max_loss_override_rs: self.config.max_loss_rupees,
            invalidation_spot_level_rs: self.config.invalidation_spot_rs,
            intraday_exit_due,
            allow_indicative_quotes: allow_prev_close,
            fees: self.broker.fee_schedule(),
        })
        .map_err(str::to_owned)
    }

    fn guard_active_spread(
        &mut self,
        active: &ActiveSpread,
        active_ords: &[BrokerOrder],
        audit: &mut AuditLog,
        now_ist: OffsetDateTime,
    ) -> Result<String, String> {
        let sqoff_due =
            is_square_off_due_ist(active.product, active.short_contract.expiry, now_ist);
        if active.excess_long_qty > 0 && !sqoff_due {
            if !active_ords.is_empty() {
                return Err(
                    "Fail-closed halt: active orders prevent safe excess hedge unwind.".to_owned(),
                );
            }
            let closed = self.unwind_long_leg(
                &active.long_contract,
                active.product,
                active.excess_long_qty,
                audit,
            )?;
            if closed != active.excess_long_qty {
                return Err(format!(
                    "Excess hedge unwind incomplete: {closed}/{}.",
                    active.excess_long_qty
                ));
            }
            return Ok(format!(
                "Unwound excess hedge ({closed} units); reconciling again next cycle."
            ));
        }

        // Pre-evaluation mandatory square-off gate: even if NSE spot or parity quotes are
        // degraded at 15:15 IST on expiry/MIS day, still execute sequenced spread exit!
        let spot_rs = match self
            .broker
            .get_spot_quote(&self.config.underlying)
            .ok()
            .and_then(|q| q.reference_price_paise(false))
        {
            Some((spot_paise, _)) => paise_to_rupees(spot_paise),
            None if sqoff_due => {
                let res = self.execute_spread_exit(
                    active,
                    active_ords,
                    "intraday_square_off_window",
                    audit,
                )?;
                self.set_armed_guardian(None)?;
                return Ok(res);
            }
            None => return Err("Live spot quote unavailable.".to_owned()),
        };

        let eval = match self.evaluate_active_spread(active, spot_rs, now_ist, false) {
            Ok(ev) => ev,
            Err(_) if sqoff_due => {
                let res = self.execute_spread_exit(
                    active,
                    active_ords,
                    "intraday_square_off_window",
                    audit,
                )?;
                self.set_armed_guardian(None)?;
                return Ok(res);
            }
            Err(e) => return Err(e),
        };
        log_event(
            audit,
            AuditEvent::SpreadEvaluated {
                regime: eval.oco_regime.as_str(),
                verdict: eval.verdict.as_str(),
                net_pnl_paise: rupees_to_paise(eval.net_pnl_rs).unwrap_or(0),
            },
        )?;

        if !self
            .armed_guardian
            .as_ref()
            .is_some_and(|g| g.matches(active))
        {
            self.set_armed_guardian(None)?;
        }

        // Check Priority-1 kill switches AND armed ratcheted OCO triggers from prior cycles!
        let mut close_reason: Option<&'static str> = eval.kill_reasons.first().copied();
        if close_reason.is_none()
            && let Some(ref armed) = self.armed_guardian
        {
            let stop_fwd_crossed = match eval.option_kind {
                OptionKind::Call => eval.forward_rs >= armed.profit_lock_stop_forward_rs,
                OptionKind::Put => eval.forward_rs <= armed.profit_lock_stop_forward_rs,
            };
            if armed.profit_lock_active
                && (eval.exec_exit_debit_pts >= armed.profit_lock_max_debit_pts || stop_fwd_crossed)
            {
                close_reason = Some("oco_no_loss_profit_lock_triggered");
            } else if armed.was_underwater && eval.net_pnl_rs >= 0.0 {
                close_reason = Some("oco_breakeven_scratch_triggered");
            } else if let Some(dmg_debit) = armed.damage_cap_max_debit_pts
                && eval.exec_exit_debit_pts >= dmg_debit
            {
                close_reason = Some("oco_damage_cap_stop_triggered");
            }
        }

        if let Some(reason) = close_reason {
            // Preserve `armed_guardian` until `execute_spread_exit` succeeds so a transient
            // exit failure never forgets a ratcheted profit-lock or scratch state!
            let res = self.execute_spread_exit(active, active_ords, reason, audit)?;
            self.set_armed_guardian(None)?;
            return Ok(res);
        }

        // Ratchet or arm guardian state for subsequent poll cycles.
        let q = f64::from(eval.qty);
        let zero_loss_exit_debit_pts =
            (eval.entry_credit_pts - eval.charges.total_charges_rs / q).max(0.05);
        let fallback_cap_debit_pts = (eval.entry_credit_pts
            + (eval.effective_loss_cap_rs - eval.charges.total_charges_rs) / q)
            .max(eval.exec_exit_debit_pts + 0.50);
        let mut next = self
            .armed_guardian
            .take()
            .unwrap_or_else(|| ArmedGuardianState {
                short_symbol: active.short_contract.trading_symbol.clone(),
                long_symbol: active.long_contract.trading_symbol.clone(),
                product: active.product,
                qty: active.qty,
                profit_lock_active: false,
                profit_lock_max_debit_pts: zero_loss_exit_debit_pts,
                profit_lock_stop_forward_rs: match eval.option_kind {
                    OptionKind::Call => f64::INFINITY,
                    OptionKind::Put => 0.0,
                },
                was_underwater: false,
                damage_cap_max_debit_pts: None,
            });
        match eval.oco_regime {
            OcoRegime::NoLossProfitLockOco => {
                let lock_debit =
                    (zero_loss_exit_debit_pts - 0.25).max(eval.exec_exit_debit_pts + 0.25);
                next.profit_lock_active = true;
                next.was_underwater = false;
                next.profit_lock_max_debit_pts = next.profit_lock_max_debit_pts.min(lock_debit);
                if let Some(ref oco) = eval.oco {
                    next.profit_lock_stop_forward_rs = match eval.option_kind {
                        OptionKind::Call => next
                            .profit_lock_stop_forward_rs
                            .min(oco.upper_stop_forward_rs),
                        OptionKind::Put => next
                            .profit_lock_stop_forward_rs
                            .max(oco.upper_stop_forward_rs),
                    };
                }
            }
            OcoRegime::ProfitLockInfeasible if !next.profit_lock_active => {
                next.damage_cap_max_debit_pts = Some(
                    next.damage_cap_max_debit_pts
                        .map_or(fallback_cap_debit_pts, |prev| {
                            prev.min(fallback_cap_debit_pts)
                        }),
                );
            }
            OcoRegime::LossMinimizationOco if !next.profit_lock_active => {
                next.was_underwater = true;
                let dmg_debit = eval.oco.as_ref().map_or(fallback_cap_debit_pts, |oco| {
                    (oco.short_upper_trigger_rs - oco.long_upper_trigger_rs)
                        .max(eval.exec_exit_debit_pts + 0.50)
                });
                next.damage_cap_max_debit_pts = Some(
                    next.damage_cap_max_debit_pts
                        .map_or(dmg_debit, |prev| prev.min(dmg_debit)),
                );
            }
            _ => {}
        }
        self.set_armed_guardian(Some(next))?;

        Ok(format!(
            "HOLD -{} / +{} ({} qty) | Net P&L: Rs {:+.2} | Regime: {} | Verdict: {}",
            active.short_contract.trading_symbol,
            active.long_contract.trading_symbol,
            active.qty,
            eval.net_pnl_rs,
            eval.oco_regime.as_str(),
            eval.verdict.as_str()
        ))
    }

    /// Sequenced exit: BUY back Short leg FIRST -> confirm `short_qty == 0` -> SELL Long hedge SECOND.
    fn execute_spread_exit(
        &mut self,
        active: &ActiveSpread,
        active_ords: &[BrokerOrder],
        reason: &'static str,
        audit: &mut AuditLog,
    ) -> Result<String, String> {
        for ord in active_ords {
            if ord.trading_symbol == active.short_contract.trading_symbol
                || ord.trading_symbol == active.long_contract.trading_symbol
            {
                self.cancel_and_confirm_terminal(&ord.order_id)?;
            }
        }

        let quotes = self
            .broker
            .get_option_quotes(&[active.short_contract.clone(), active.long_contract.clone()])
            .map_err(|e| format!("Exit quote refresh failed: {e}"))?;
        let sq = quotes
            .iter()
            .find(|(c, _)| c.trading_symbol == active.short_contract.trading_symbol)
            .map(|(_, q)| *q)
            .ok_or("Missing short quote for exit.")?;
        let short_touch = sq.executable_quote().map_err(str::to_owned)?;

        let mut sim_fills = SimulatedFills::default();
        sim_fills
            .record_hedge_buy(active.qty)
            .map_err(str::to_owned)?;
        sim_fills
            .record_short_sell(active.qty)
            .map_err(str::to_owned)?;

        // STEP 1: BUY SHORT LEG FIRST!
        let slip_paise = rupees_to_paise(self.config.exit_slippage_pts).unwrap_or(200);
        let short_buy_limit = sq
            .clamp_limit_paise(
                short_touch.ask.saturating_add(slip_paise),
                active.short_contract.tick_paise,
            )
            .map_err(str::to_owned)?;

        let (closed_short_qty, _) = self.submit_and_reconcile_leg(
            &active.short_contract,
            OrderSide::Buy,
            active.product,
            active.qty,
            short_buy_limit,
            "exit_short_first",
            "XS",
            audit,
        )?;
        if closed_short_qty == 0 {
            let _ = log_event(
                audit,
                AuditEvent::CycleFailed {
                    stage: "exit_short_unfilled",
                },
            );
            return Err(format!(
                "Short leg {} exit order received 0 fills; keeping long hedge intact for protection.",
                active.short_contract.trading_symbol
            ));
        }
        sim_fills
            .record_short_buy(closed_short_qty)
            .map_err(str::to_owned)?;
        if closed_short_qty == active.qty {
            log_event(
                audit,
                AuditEvent::ShortFlatConfirmed {
                    qty_closed: closed_short_qty,
                },
            )?;
        }

        // STEP 2: SELL LONG HEDGE SECOND — strictly <= closed_short_qty so remaining short stays covered!
        sim_fills
            .record_hedge_sell(closed_short_qty)
            .map_err(str::to_owned)?;
        let closed_long_qty = self.unwind_long_leg(
            &active.long_contract,
            active.product,
            closed_short_qty,
            audit,
        )?;

        log_event(
            audit,
            AuditEvent::SpreadClosed {
                mode: self.config.mode,
                reason,
                qty: closed_short_qty,
            },
        )?;

        Ok(format!(
            "CLOSED SPREAD ({reason}): bought short {} ({}/{}) -> sold hedge {} ({}/{})",
            active.short_contract.trading_symbol,
            closed_short_qty,
            active.qty,
            active.long_contract.trading_symbol,
            closed_long_qty,
            closed_short_qty
        ))
    }

    fn unwind_long_leg(
        &mut self,
        contract: &OptionContract,
        product: ProductType,
        qty: u32,
        audit: &mut AuditLog,
    ) -> Result<u32, String> {
        let quotes = self
            .broker
            .get_option_quotes(std::slice::from_ref(contract))
            .map_err(|e| format!("Hedge unwind quote failed: {e}"))?;
        let lq = quotes
            .first()
            .map(|(_, q)| *q)
            .ok_or("Missing hedge quote.")?;
        let touch = lq.executable_quote().map_err(str::to_owned)?;
        let slip_paise = rupees_to_paise(self.config.exit_slippage_pts).unwrap_or(200);
        let sell_limit = lq
            .clamp_limit_paise(
                (touch.bid - slip_paise).max(contract.tick_paise),
                contract.tick_paise,
            )
            .map_err(str::to_owned)?;
        let (filled, _) = self.submit_and_reconcile_leg(
            contract,
            OrderSide::Sell,
            product,
            qty,
            sell_limit,
            "exit_long_hedge",
            "XL",
            audit,
        )?;
        Ok(filled)
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_and_reconcile_leg(
        &mut self,
        contract: &OptionContract,
        side: OrderSide,
        product: ProductType,
        qty: u32,
        limit_paise: i64,
        leg_label: &'static str,
        tag_code: &'static str,
        audit: &mut AuditLog,
    ) -> Result<(u32, Option<i64>), String> {
        // Strictly alphanumeric 17-char tag (`nsp` + 2-char leg code + 12 hex) for universal
        // Indian OMS compatibility (Shoonya, Kite, Kotak Neo, Dhan, Groww, m.Stock, Fyers).
        let short_uuid = &uuid::Uuid::new_v4().simple().to_string()[..12];
        let client_tag = format!("nsp{tag_code}{short_uuid}");
        let side_str = match side {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
        };
        log_event(
            audit,
            AuditEvent::OrderSubmitted {
                mode: self.config.mode,
                leg: leg_label,
                side: side_str,
                qty,
                limit_paise,
            },
        )?;

        let req = OrderRequest {
            exchange: contract.exchange.clone(),
            token: contract.token.clone(),
            trading_symbol: contract.trading_symbol.clone(),
            side,
            product,
            quantity: qty,
            price_type: OrderPriceType::Limit {
                price_paise: limit_paise,
            },
            retention: Retention::Day,
            client_tag: client_tag.clone(),
        };

        let mut known_order_id = match self.broker.place_order(&req) {
            Ok(placement) => placement.order_id,
            Err(BrokerError::AmbiguousTimeout { .. }) => {
                // Reconcile OrderBook by client_tag before assuming failure — never duplicate!
                let orders = self
                    .broker
                    .get_orders()
                    .map_err(|e| format!("Post-timeout OrderBook reconciliation failed: {e}"))?;
                match find_order(&orders, None, &client_tag) {
                    Some(found) => found.order_id.clone(),
                    None => {
                        log_event(
                            audit,
                            AuditEvent::OrderReconciled {
                                leg: leg_label,
                                status: "timeout_not_found",
                                filled_qty: 0,
                            },
                        )?;
                        return Err(format!(
                            "Fail-closed halt: ambiguous {leg_label} order was not found in OrderBook; manual reconciliation required before trading."
                        ));
                    }
                }
            }
            Err(err) => {
                let _ = log_event(audit, AuditEvent::CycleFailed { stage: leg_label });
                return Err(format!("Order placement failed on {leg_label}: {err}"));
            }
        };

        let started = std::time::Instant::now();
        loop {
            let orders = self
                .broker
                .get_orders()
                .map_err(|e| format!("OrderBook poll failed on {leg_label}: {e}"))?;
            if let Some(ord) = find_order(&orders, Some(&known_order_id), &client_tag) {
                known_order_id.clone_from(&ord.order_id);
                if ord.status == OrderStatus::Complete || ord.filled_qty >= qty {
                    log_event(
                        audit,
                        AuditEvent::OrderReconciled {
                            leg: leg_label,
                            status: ord.status.as_str(),
                            filled_qty: ord.filled_qty.min(qty),
                        },
                    )?;
                    return Ok((ord.filled_qty.min(qty), ord.avg_fill_price_paise));
                }
                if !ord.status.is_active() {
                    log_event(
                        audit,
                        AuditEvent::OrderReconciled {
                            leg: leg_label,
                            status: ord.status.as_str(),
                            filled_qty: ord.filled_qty.min(qty),
                        },
                    )?;
                    return Ok((ord.filled_qty.min(qty), ord.avg_fill_price_paise));
                }
            }

            if started.elapsed() >= self.config.order_wait_timeout {
                break;
            }
            if !self.config.order_poll_step.is_zero() {
                std::thread::sleep(self.config.order_poll_step);
            }
        }

        // Cancel remaining unfilled quantity, confirm terminal state, and fetch authoritative final fill count.
        let ord = self.cancel_and_confirm_terminal(&known_order_id)?;
        let status_str = ord.status.as_str();
        let filled = ord.filled_qty.min(qty);
        let avg = ord.avg_fill_price_paise;
        log_event(
            audit,
            AuditEvent::OrderReconciled {
                leg: leg_label,
                status: status_str,
                filled_qty: filled,
            },
        )?;
        Ok((filled, avg))
    }
}

fn find_order<'a>(
    orders: &'a [BrokerOrder],
    order_id: Option<&str>,
    client_tag: &str,
) -> Option<&'a BrokerOrder> {
    if let Some(id) = order_id
        && let Some(found) = orders.iter().find(|o| o.order_id == id)
    {
        return Some(found);
    }
    orders
        .iter()
        .find(|o| o.client_tag.as_deref() == Some(client_tag))
}

fn is_pilot_tag(tag: Option<&str>) -> bool {
    tag.is_some_and(|t| t.starts_with("nsp"))
}

fn guardian_state_path(audit_dir: &Path) -> PathBuf {
    audit_dir.join("nifty-spread-pilot.guardian.json")
}

fn load_guardian_state(audit_dir: &Path) -> Result<Option<ArmedGuardianState>, String> {
    let path = guardian_state_path(audit_dir);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("unreadable {}: {e}", path.display())),
    };
    if bytes.len() > 8_192 {
        return Err(format!("oversized {}", path.display()));
    }
    let val: Value = serde_json::from_slice(&bytes)
        .map_err(|_| format!("corrupt JSON in {}", path.display()))?;
    let state = ArmedGuardianState::from_json(&val)
        .ok_or_else(|| format!("invalid schema in {}", path.display()))?;
    Ok(Some(state))
}

fn persist_guardian_state(
    audit_dir: &Path,
    state: Option<&ArmedGuardianState>,
) -> Result<(), String> {
    let path = guardian_state_path(audit_dir);
    match state {
        None => match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
        },
        Some(armed) => {
            std::fs::create_dir_all(audit_dir)
                .map_err(|e| format!("cannot create {}: {e}", audit_dir.display()))?;
            let tmp = audit_dir.join(format!(
                "nifty-spread-pilot.guardian.{}.tmp",
                uuid::Uuid::new_v4().simple()
            ));
            let mut opts = OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts
                .open(&tmp)
                .map_err(|e| format!("cannot open {}: {e}", tmp.display()))?;
            serde_json::to_writer(&mut f, &armed.to_json())
                .map_err(|e| format!("cannot serialize guardian state: {e}"))?;
            f.flush()
                .map_err(|e| format!("cannot flush {}: {e}", tmp.display()))?;
            drop(f);
            std::fs::rename(&tmp, &path).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("cannot rename {} -> {}: {e}", tmp.display(), path.display())
            })
        }
    }
}

fn valid_credit_strikes(kind: OptionKind, short_paise: i64, long_paise: i64) -> bool {
    match kind {
        OptionKind::Call => short_paise < long_paise,
        OptionKind::Put => short_paise > long_paise,
    }
}

fn log_event(audit: &mut AuditLog, event: AuditEvent) -> Result<(), String> {
    audit
        .record(event)
        .map_err(|e| format!("Cannot write audit log: {e}"))
}

pub fn format_scan_report(
    broker_name: &str,
    expiry: time::Date,
    ranked: &RankOutcome,
    lots: u32,
) -> String {
    let d = &ranked.diagnostics;
    let mut out = format!(
        "=== NIFTY SPREAD PILOT SCAN ({}) ===\n\
         Expiry: {expiry} ({:.2} DTE) | Spot: {:.2} | Implied Forward (Parity={}): {:.2}\n\
         ATM IV: {} | 1-Sigma Horizon: {} | PCR(OI): {} | 25D P-C Skew: {}\n\
         Call OI Wall: {} | Put OI Wall: {} | Pairs Evaluated: {}\n",
        broker_name.to_ascii_uppercase(),
        d.t_days,
        d.spot_rs,
        d.parity_used,
        d.implied_forward_rs,
        d.atm_iv_pct
            .map_or_else(|| "n/a".to_owned(), |v| format!("{v:.2}%")),
        d.horizon_1sigma_pts
            .map_or_else(|| "n/a".to_owned(), |v| format!("+-{v:.1} pts")),
        d.chain_pcr_oi
            .map_or_else(|| "n/a".to_owned(), |v| format!("{v:.2}")),
        d.put_call_skew_25d_pct
            .map_or_else(|| "n/a".to_owned(), |v| format!("{v:+.2}%")),
        d.max_call_oi_strike_rs.map_or_else(
            || "n/a".to_owned(),
            |k| format!("{k:.0} ({})", d.max_call_oi)
        ),
        d.max_put_oi_strike_rs.map_or_else(
            || "n/a".to_owned(),
            |k| format!("{k:.0} ({})", d.max_put_oi)
        ),
        ranked.evaluated_pairs
    );

    if let Some(ref best) = ranked.recommended {
        let kind_label = match best.option_kind {
            OptionKind::Call => "BEAR CALL CREDIT SPREAD",
            OptionKind::Put => "BULL PUT CREDIT SPREAD",
        };
        out.push_str(&format!(
            "\nRECOMMENDED ({} | {kind_label} | {lots} lot(s) | ExecutableTouch={}):\n\
             - SELL {} @ Rs {:.2}  (IV {:.2}%, Delta {:+.3})\n\
             - BUY  {} @ Rs {:.2}  (IV {:.2}%)\n\
             - Net Credit: Rs {:.2} pts ({:.1}% of {:.0}-pt width) | Cushion: {:.1} pts\n\
             - Net Max Profit: Rs {:.2} | Net Max Loss: Rs {:.2} | Net Breakeven: {:.2} | PoP: {:.1}%\n\
             - Net Theta: Rs {:+.2}/day ({:+.2} pts/day) | OI Wall Backed: {}\n",
            best.edge_tier.as_str(),
            best.touch_executable,
            best.short_contract.trading_symbol,
            best.short_entry_rs,
            best.short_iv_pct,
            best.short_delta,
            best.long_contract.trading_symbol,
            best.long_entry_rs,
            best.long_iv_pct,
            best.entry_credit_pts,
            best.credit_to_width_pct,
            best.width_pts,
            best.otm_cushion_pts,
            best.max_profit_net_rs,
            best.max_loss_net_rs,
            best.breakeven_net_rs,
            best.pop_terminal_pct,
            best.net_theta_rs_day,
            best.net_theta_pts_day,
            best.oi_wall_backed
        ));
    } else {
        out.push_str(
            "\nVERDICT: NO_POSITION_RECOMMENDED — DO NOT ENTER A SPREAD NOW (no spread passed CORE_INCOME or HIGH_POP_1SD gates).\n",
        );
        if !ranked.near_misses.is_empty() {
            out.push_str("Top Near-Misses (for reference only):\n");
            for nm in &ranked.near_misses {
                out.push_str(&format!(
                    "  * -{} / +{} | credit={:.2} ({:.1}%w) | PoP={:.1}% | |Delta_s|={:.2} | Theta={:+.2} pts/d\n",
                    nm.short_contract.trading_symbol,
                    nm.long_contract.trading_symbol,
                    nm.entry_credit_pts,
                    nm.credit_to_width_pct,
                    nm.pop_terminal_pct,
                    nm.short_delta.abs(),
                    nm.net_theta_pts_day
                ));
            }
        }
    }
    out
}

pub fn format_evaluation_report(active: &ActiveSpread, eval: &LiveSpreadEvaluation) -> String {
    let mut out = format!(
        "--- Part A: Live Position & Net-After-Charges ---\n\
         Spread: -{} (entry Rs {:.2}) / +{} (entry Rs {:.2}) | Qty: {}\n\
         Implied Forward: {:.2} | Cushion: {:.1} pts | Entry Credit: {:.2} | Exit Touch Debit: {:.2}\n\
         Gross P&L: Rs {:+.2} | Statutory+Brokerage Charges: Rs {:.2} | NET P&L: Rs {:+.2}\n\
         Net Target: Rs {:.2} | Loss Cap: Rs {:.2} | Net Breakeven: {:.2}\n",
        active.short_contract.trading_symbol,
        paise_to_rupees(active.short_entry_paise),
        active.long_contract.trading_symbol,
        paise_to_rupees(active.long_entry_paise),
        active.qty,
        eval.forward_rs,
        eval.otm_cushion_pts,
        eval.entry_credit_pts,
        eval.exec_exit_debit_pts,
        eval.gross_pnl_rs,
        eval.charges.total_charges_rs,
        eval.net_pnl_rs,
        eval.target_net_rs,
        eval.effective_loss_cap_rs,
        eval.breakeven_net_rs
    );

    out.push_str("--- Part B: Discounted Black-76 Greeks ---\n");
    if let (Some(gs), Some(gl)) = (eval.short_greeks, eval.long_greeks) {
        out.push_str(&format!(
            "Short IV {:.2}% (Delta {:+.3}, Theta {:.2}/d, Vega {:.2}) | Long IV {:.2}% (Delta {:+.3}, Theta {:.2}/d)\n\
             Spread Net Delta: {:+.3} | Net Theta: Rs {:+.2}/day\n",
            gs.iv_pct,
            gs.delta,
            gs.theta_per_day,
            gs.vega_1pct,
            gl.iv_pct,
            gl.delta,
            gl.theta_per_day,
            eval.net_delta.unwrap_or(0.0),
            eval.net_theta_rs_day.unwrap_or(0.0)
        ));
    } else {
        out.push_str("Greeks: IV non-convergent (Priority-1 kill-switches remain active).\n");
    }

    out.push_str(&format!(
        "--- Part C: OCO Regime & Sequenced Action ---\n\
         Regime: {} | Verdict: {}\n",
        eval.oco_regime.as_str(),
        eval.verdict.as_str()
    ));
    if !eval.kill_reasons.is_empty() {
        out.push_str(&format!(
            "Kill-Switch Reasons: {}\n",
            eval.kill_reasons.join(", ")
        ));
    }
    if let Some(ref oco) = eval.oco {
        out.push_str(&format!(
            "F*_lower (Target): {:.2} -> Short BUY trig {:.2} / lim {:.2} (Net P&L ~ Rs {:+.2})\n\
             F*_upper (Stop):   {:.2} -> Short BUY trig {:.2} / lim {:.2} | Long SELL trig {:.2} / lim {:.2}\n\
             Zero-Loss Floor Feasible: {} | Worst-Case Stop Limit Net P&L: Rs {:+.2} | H_min: Rs {:.2}\n",
            oco.lower_target_forward_rs,
            oco.short_lower_trigger_rs,
            oco.short_lower_limit_rs,
            oco.normal_lower_net_pnl_rs,
            oco.upper_stop_forward_rs,
            oco.short_upper_trigger_rs,
            oco.short_upper_limit_rs,
            oco.long_upper_trigger_rs,
            oco.long_upper_limit_rs,
            oco.zero_loss_floor_feasible,
            oco.worst_upper_limit_net_pnl_rs,
            oco.h_min_after_short_stop_fill_rs
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::{
        AccountLimits, ExpiryChainContracts, FeeSchedule, MarketQuote, OrderPlacement, PaperBroker,
    };
    use crate::math::{DEFAULT_RISK_FREE_RATE, black76_price};
    use time::{Date, Month};

    struct ScriptedBroker {
        spot_paise: i64,
        expiry: Date,
        contracts: Vec<OptionContract>,
        quotes: HashMap<String, MarketQuote>,
        positions: Vec<BrokerPosition>,
        orders: Vec<BrokerOrder>,
        limits: AccountLimits,
        timeout_first_place: bool,
        timeout_drop_first_place: bool,
        cancel_noop: bool,
        cancel_drop: bool,
        fill_price_override_paise: Option<i64>,
        scripted_fills: std::collections::VecDeque<(OrderStatus, u32)>,
        next_oid: u32,
    }

    impl ScriptedBroker {
        fn build_chain(spot_rs: f64, expiry: Date, t_days: f64, iv: f64) -> Self {
            let mut contracts = Vec::new();
            let mut quotes = HashMap::new();
            let t_years = t_days / 365.0;
            let mut tok = 5000_u32;
            for step in (-16..=16).map(|i| i * 50) {
                let k = (spot_rs / 50.0).round() * 50.0 + f64::from(step);
                let kp = rupees_to_paise(k).unwrap();
                for kind in [OptionKind::Call, OptionKind::Put] {
                    tok += 1;
                    let sym = format!(
                        "NIFTY01DEC26{}{}",
                        if kind == OptionKind::Call { "C" } else { "P" },
                        k as i64
                    );
                    let theo = black76_price(
                        k * 0.0 + spot_rs,
                        k,
                        t_years,
                        iv,
                        kind,
                        DEFAULT_RISK_FREE_RATE,
                    )
                    .max(1.0);
                    let mid = rupees_to_paise(theo).unwrap();
                    let c = OptionContract {
                        exchange: "NFO".to_owned(),
                        token: tok.to_string(),
                        trading_symbol: sym.clone(),
                        underlying: "NIFTY".to_owned(),
                        expiry,
                        option_kind: kind,
                        strike_paise: kp,
                        lot_size: 65,
                        tick_paise: 5,
                    };
                    let q = MarketQuote {
                        ltp_paise: Some(mid),
                        prev_close_paise: Some(mid),
                        bid_paise: Some((mid - 10).max(5)),
                        ask_paise: Some(mid + 10),
                        bid_qty: 650,
                        ask_qty: 650,
                        open_interest: Some(2_000_000),
                        lower_circuit_paise: Some(5),
                        upper_circuit_paise: Some(mid * 4),
                        ..MarketQuote::default()
                    };
                    contracts.push(c);
                    quotes.insert(sym, q);
                }
            }
            Self {
                spot_paise: rupees_to_paise(spot_rs).unwrap(),
                expiry,
                contracts,
                quotes,
                positions: Vec::new(),
                orders: Vec::new(),
                limits: AccountLimits {
                    cash_paise: 25_000_000,
                    collateral_paise: 0,
                    payin_paise: 0,
                    payout_paise: 0,
                    margin_used_paise: 0,
                },
                timeout_first_place: false,
                timeout_drop_first_place: false,
                cancel_noop: false,
                cancel_drop: false,
                fill_price_override_paise: None,
                scripted_fills: std::collections::VecDeque::new(),
                next_oid: 1,
            }
        }
    }

    impl Broker for ScriptedBroker {
        fn name(&self) -> &'static str {
            "mock"
        }
        fn fee_schedule(&self) -> FeeSchedule {
            FeeSchedule::shoonya_2026()
        }
        fn ensure_session(&mut self) -> Result<(), BrokerError> {
            Ok(())
        }
        fn discover_option_chain_contracts(
            &mut self,
            _underlying: &str,
            _now_ist: OffsetDateTime,
        ) -> Result<ExpiryChainContracts, BrokerError> {
            Ok(ExpiryChainContracts {
                expiry: self.expiry,
                lot_size: 65,
                tick_paise: 5,
                contracts: self.contracts.clone(),
            })
        }
        fn resolve_contracts_by_symbols(
            &mut self,
            _underlying: &str,
            symbols: &[String],
            _now_ist: OffsetDateTime,
        ) -> Result<Vec<OptionContract>, BrokerError> {
            Ok(self
                .contracts
                .iter()
                .filter(|c| symbols.iter().any(|s| s == &c.trading_symbol))
                .cloned()
                .collect())
        }
        fn get_spot_quote(&mut self, _underlying: &str) -> Result<MarketQuote, BrokerError> {
            Ok(MarketQuote {
                ltp_paise: Some(self.spot_paise),
                prev_close_paise: Some(self.spot_paise),
                ..MarketQuote::default()
            })
        }
        fn get_option_quotes(
            &mut self,
            contracts: &[OptionContract],
        ) -> Result<Vec<(OptionContract, MarketQuote)>, BrokerError> {
            Ok(contracts
                .iter()
                .map(|c| {
                    let q = self
                        .quotes
                        .get(&c.trading_symbol)
                        .copied()
                        .unwrap_or_default();
                    (c.clone(), q)
                })
                .collect())
        }
        fn get_account_limits(&mut self) -> Result<AccountLimits, BrokerError> {
            Ok(self.limits)
        }
        fn get_positions(&mut self) -> Result<Vec<BrokerPosition>, BrokerError> {
            Ok(self.positions.clone())
        }
        fn get_orders(&mut self) -> Result<Vec<BrokerOrder>, BrokerError> {
            Ok(self.orders.clone())
        }
        fn place_order(&mut self, req: &OrderRequest) -> Result<OrderPlacement, BrokerError> {
            if self.timeout_drop_first_place {
                self.timeout_drop_first_place = false;
                return Err(BrokerError::AmbiguousTimeout {
                    stage: "PlaceOrder",
                });
            }
            let oid = format!("OID{}", self.next_oid);
            self.next_oid += 1;
            let OrderPriceType::Limit { price_paise } = req.price_type else {
                unreachable!()
            };
            let (status, fill_qty) = self
                .scripted_fills
                .pop_front()
                .unwrap_or((OrderStatus::Complete, req.quantity));
            let actual_fill_paise = self.fill_price_override_paise.take().unwrap_or(price_paise);
            // Record order even if network response times out (tests ambiguous timeout recovery!).
            self.orders.push(BrokerOrder {
                order_id: oid.clone(),
                exchange: req.exchange.clone(),
                trading_symbol: req.trading_symbol.clone(),
                side: req.side,
                product: req.product,
                status,
                requested_qty: req.quantity,
                filled_qty: fill_qty,
                avg_fill_price_paise: (fill_qty > 0).then_some(actual_fill_paise),
                limit_price_paise: Some(price_paise),
                trigger_price_paise: None,
                client_tag: Some(req.client_tag.clone()),
            });
            if fill_qty > 0 {
                let signed = match req.side {
                    OrderSide::Buy => fill_qty as i32,
                    OrderSide::Sell => -(fill_qty as i32),
                };
                if let Some(pos) = self
                    .positions
                    .iter_mut()
                    .find(|p| p.trading_symbol == req.trading_symbol)
                {
                    pos.net_qty += signed;
                } else {
                    self.positions.push(BrokerPosition {
                        exchange: req.exchange.clone(),
                        token: "1".to_owned(),
                        trading_symbol: req.trading_symbol.clone(),
                        product: req.product,
                        net_qty: signed,
                        buy_avg_paise: (req.side == OrderSide::Buy).then_some(actual_fill_paise),
                        sell_avg_paise: (req.side == OrderSide::Sell).then_some(actual_fill_paise),
                        ltp_paise: Some(actual_fill_paise),
                        realized_pnl_paise: 0,
                        unrealized_pnl_paise: 0,
                    });
                }
            }
            if self.timeout_first_place {
                self.timeout_first_place = false;
                return Err(BrokerError::AmbiguousTimeout {
                    stage: "PlaceOrder",
                });
            }
            Ok(OrderPlacement { order_id: oid })
        }
        fn cancel_order(&mut self, order_id: &str) -> Result<(), BrokerError> {
            if self.cancel_drop {
                self.orders.retain(|o| o.order_id != order_id);
                return Ok(());
            }
            if !self.cancel_noop
                && let Some(ord) = self.orders.iter_mut().find(|o| o.order_id == order_id)
                && ord.status.is_active()
            {
                ord.status = OrderStatus::Canceled;
            }
            Ok(())
        }
    }

    fn temp_audit() -> (PathBuf, AuditLog) {
        let path =
            std::env::temp_dir().join(format!("nsp-engine-test-{}.jsonl", uuid::Uuid::new_v4()));
        let log = AuditLog::open(&path).unwrap();
        (path, log)
    }

    fn test_cfg() -> PilotConfig {
        PilotConfig {
            audit_dir: std::env::temp_dir().join(format!("nsp-test-dir-{}", uuid::Uuid::new_v4())),
            ..PilotConfig::default()
        }
    }

    #[test]
    fn unified_engine_scans_enters_guards_and_exits_cleanly_in_live_and_paper() {
        let expiry = Date::from_calendar_date(2026, Month::December, 1).unwrap();
        let now_ist = OffsetDateTime::from_unix_timestamp(1_795_400_000)
            .unwrap()
            .replace_date(Date::from_calendar_date(2026, Month::November, 25).unwrap())
            .replace_time(Time::from_hms(10, 30, 0).unwrap());

        let mut scripted = ScriptedBroker::build_chain(25_400.0, expiry, 6.2, 0.16);
        scripted.timeout_first_place = true; // Verify ambiguous-timeout OrderBook reconciliation!

        let cfg = PilotConfig {
            mode: ExecutionMode::Live,
            order_poll_step: Duration::ZERO,
            order_wait_timeout: Duration::from_millis(50),
            ..test_cfg()
        };

        let (log_path, mut audit) = temp_audit();
        let mut engine = PilotEngine::new(scripted, cfg.clone());

        // 1. Read-only scan & flat status
        let scan_txt = engine.run_scan(&mut audit, now_ist).unwrap();
        assert!(scan_txt.contains("RECOMMENDED"));
        let status_flat = engine.run_status(&mut audit, now_ist).unwrap();
        assert!(status_flat.contains("FLAT"));

        // 2. Cycle 1: Flat -> Hedge-First Entry (with ambiguous timeout recovery on leg 1!)
        let entered = engine.run_cycle(&mut audit, now_ist).unwrap();
        assert!(entered.contains("ENTERED"), "{entered}");

        // 3. Status & Cycle 2: Active Matched Spread -> Hold with OCO
        let status_active = engine.run_status(&mut audit, now_ist).unwrap();
        assert!(status_active.contains("Part A: Live Position"));
        assert!(status_active.contains("Part C: OCO Regime"));
        let hold_msg = engine.run_cycle(&mut audit, now_ist).unwrap();
        assert!(hold_msg.contains("HOLD"), "{hold_msg}");

        // 4. Cycle 3: Expiry square-off window (15:20 IST on expiry day) -> Short-First Close!
        let expiry_sqoff = now_ist
            .replace_date(expiry)
            .replace_time(Time::from_hms(15, 20, 0).unwrap());
        let closed_msg = engine.run_cycle(&mut audit, expiry_sqoff).unwrap();
        assert!(closed_msg.contains("CLOSED SPREAD"), "{closed_msg}");

        // 5. Also verify PaperBroker<ScriptedBroker> uses the exact same pipeline!
        let paper_inner = ScriptedBroker::build_chain(25_400.0, expiry, 6.2, 0.16);
        let mut paper_engine = PilotEngine::new(
            PaperBroker::new(paper_inner),
            PilotConfig {
                mode: ExecutionMode::Paper,
                ..cfg
            },
        );
        let paper_entered = paper_engine.run_cycle(&mut audit, now_ist).unwrap();
        assert!(paper_entered.contains("ENTERED"));

        drop(audit);
        let _ = std::fs::remove_file(log_path);
    }

    #[test]
    fn engine_unwinds_orphan_long_and_halts_on_uncovered_short() {
        let expiry = Date::from_calendar_date(2026, Month::December, 1).unwrap();
        let now_ist = OffsetDateTime::from_unix_timestamp(1_795_400_000)
            .unwrap()
            .replace_date(Date::from_calendar_date(2026, Month::November, 25).unwrap())
            .replace_time(Time::from_hms(11, 0, 0).unwrap());

        // Case A: Orphan Long -> unwound cleanly.
        let mut b1 = ScriptedBroker::build_chain(25_400.0, expiry, 6.0, 0.16);
        b1.positions.push(BrokerPosition {
            exchange: "NFO".to_owned(),
            token: "5001".to_owned(),
            trading_symbol: "NIFTY01DEC26C25600".to_owned(),
            product: ProductType::Margin,
            net_qty: 65,
            buy_avg_paise: Some(5_000),
            sell_avg_paise: None,
            ltp_paise: Some(4_800),
            realized_pnl_paise: 0,
            unrealized_pnl_paise: 0,
        });
        let (p1, mut a1) = temp_audit();
        let mut e1 = PilotEngine::new(
            b1,
            PilotConfig {
                order_poll_step: Duration::ZERO,
                ..test_cfg()
            },
        );
        let msg = e1.run_cycle(&mut a1, now_ist).unwrap();
        assert!(msg.contains("Unwound orphan long hedge"));
        drop(a1);
        let _ = std::fs::remove_file(p1);

        // Case B: Uncovered Short -> immediate fail-closed halt!
        let mut b2 = ScriptedBroker::build_chain(25_400.0, expiry, 6.0, 0.16);
        b2.positions.push(BrokerPosition {
            exchange: "NFO".to_owned(),
            token: "5001".to_owned(),
            trading_symbol: "NIFTY01DEC26C25500".to_owned(),
            product: ProductType::Margin,
            net_qty: -65,
            buy_avg_paise: None,
            sell_avg_paise: Some(9_000),
            ltp_paise: Some(9_500),
            realized_pnl_paise: 0,
            unrealized_pnl_paise: 0,
        });
        let (p2, mut a2) = temp_audit();
        let mut e2 = PilotEngine::new(b2, test_cfg());
        let err = e2.run_cycle(&mut a2, now_ist).unwrap_err();
        assert!(err.contains("Fail-closed halt"));
        drop(a2);
        let _ = std::fs::remove_file(p2);
    }

    #[test]
    fn process_lock_prevents_concurrent_instances() {
        let dir = std::env::temp_dir().join(format!("nsp-lock-{}", uuid::Uuid::new_v4()));
        let lock1 = ProcessLock::acquire(&dir).unwrap();
        assert!(ProcessLock::acquire(&dir).is_err());
        drop(lock1);
        let lock2 = ProcessLock::acquire(&dir).unwrap();
        drop(lock2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn config_parser_and_dotenv_reader_validate_all_fields() {
        let dir = std::env::temp_dir().join(format!("nsp-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // Missing file -> empty map.
        assert!(
            read_dotenv_map(&dir.join("missing.env"))
                .unwrap()
                .is_empty()
        );

        // Valid .env file.
        let valid_env = dir.join("valid.env");
        std::fs::write(
            &valid_env,
            "NIFTY_EXECUTION_MODE=live\n\
             NIFTY_STRATEGY_BIAS=bull_put\n\
             NIFTY_PRODUCT=MIS\n\
             NIFTY_LOTS=2\n\
             NIFTY_POLL_SECS=10\n\
             NIFTY_ORDER_WAIT_MS=2500\n\
             NIFTY_ALLOW_NEW_ENTRY=false\n\
             NIFTY_QUOTE_TOKEN=26000\n\
             SHOONYA_ACCOUNT_ID=FA99999\n\
             NIFTY_TARGET_NET_RUPEES=2500\n\
             NIFTY_MAX_LOSS_RUPEES=3000\n\
             NIFTY_INVALIDATION_SPOT=25800\n\
             NIFTY_MIN_MARGIN_PER_LOT_RUPEES=55000\n\
             NIFTY_ENTRY_SLIPPAGE_PTS=0.75\n\
             NIFTY_EXIT_SLIPPAGE_PTS=2.50\n\
             NIFTY_CHAIN_WINDOW_PTS=900\n",
        )
        .unwrap();
        let map = read_dotenv_map(&valid_env).unwrap();
        let cfg = PilotConfig::from_lookup(None, valid_env.clone(), dir.clone(), |k| {
            Ok(map.get(k).cloned())
        })
        .unwrap();
        assert_eq!(cfg.mode, ExecutionMode::Live);
        assert_eq!(cfg.bias, StrategyBias::BullPut);
        assert_eq!(cfg.product, ProductType::Intraday);
        assert_eq!(cfg.lots, 2);
        assert_eq!(cfg.poll_interval, Duration::from_secs(10));
        assert_eq!(cfg.order_wait_timeout, Duration::from_millis(2_500));
        assert!(!cfg.allow_new_entry);
        assert_eq!(cfg.account_id.as_deref(), Some("FA99999"));
        assert_eq!(cfg.target_net_rupees, Some(2_500.0));
        assert_eq!(cfg.max_loss_rupees, Some(3_000.0));
        assert_eq!(cfg.invalidation_spot_rs, Some(25_800.0));

        // Oversized .env rejected.
        let huge_env = dir.join("huge.env");
        std::fs::write(&huge_env, vec![b'A'; 70_000]).unwrap();
        assert!(read_dotenv_map(&huge_env).is_err());

        // Malformed .env rejected.
        let bad_env = dir.join("bad.env");
        std::fs::write(&bad_env, "BROKEN=\"unterminated").unwrap();
        assert!(read_dotenv_map(&bad_env).is_err());

        // Invalid individual settings rejected cleanly.
        let check_err = |key: &str, val: &str| {
            let mut m = HashMap::new();
            m.insert(key.to_owned(), val.to_owned());
            PilotConfig::from_lookup(None, valid_env.clone(), dir.clone(), |k| {
                Ok(m.get(k).cloned())
            })
            .unwrap_err()
        };
        assert!(matches!(
            check_err("NIFTY_EXECUTION_MODE", "yolo"),
            ConfigLoadError::InvalidExecutionMode(_)
        ));
        assert!(matches!(
            check_err("NIFTY_STRATEGY_BIAS", "iron_condor"),
            ConfigLoadError::Message(_)
        ));
        assert!(matches!(
            check_err("NIFTY_PRODUCT", "CNC"),
            ConfigLoadError::Message(_)
        ));
        assert!(matches!(
            check_err("NIFTY_LOTS", "0"),
            ConfigLoadError::Message(_)
        ));
        assert!(matches!(
            check_err("NIFTY_LOTS", "abc"),
            ConfigLoadError::Message(_)
        ));
        assert!(matches!(
            check_err("NIFTY_POLL_SECS", "1"),
            ConfigLoadError::Message(_)
        ));
        assert!(matches!(
            check_err("NIFTY_ORDER_WAIT_MS", "10"),
            ConfigLoadError::Message(_)
        ));
        assert!(matches!(
            check_err("NIFTY_ALLOW_NEW_ENTRY", "maybe"),
            ConfigLoadError::Message(_)
        ));
        assert!(matches!(
            check_err("NIFTY_QUOTE_TOKEN", "NIFTY"),
            ConfigLoadError::Message(_)
        ));
        assert!(matches!(
            check_err("NIFTY_TARGET_NET_RUPEES", "-5"),
            ConfigLoadError::Message(_)
        ));
        assert!(matches!(
            check_err("NIFTY_TARGET_NET_RUPEES", "nan_text"),
            ConfigLoadError::Message(_)
        ));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn guardian_ratchets_profit_lock_and_breakeven_scratch_and_handles_edge_states() {
        let expiry = Date::from_calendar_date(2026, Month::December, 1).unwrap();
        let weekday_open = OffsetDateTime::from_unix_timestamp(1_795_400_000)
            .unwrap()
            .replace_date(Date::from_calendar_date(2026, Month::November, 25).unwrap())
            .replace_time(Time::from_hms(11, 0, 0).unwrap());
        let after_hours = weekday_open.replace_time(Time::from_hms(16, 0, 0).unwrap());
        let saturday = OffsetDateTime::from_unix_timestamp(1_795_400_000)
            .unwrap()
            .replace_date(Date::from_calendar_date(2026, Month::November, 28).unwrap())
            .replace_time(Time::from_hms(11, 0, 0).unwrap());

        assert!(!is_market_open_ist(saturday));
        assert!(!is_entry_window_open_ist(saturday));
        assert!(!is_market_open_ist(after_hours));

        let (log_path, mut audit) = temp_audit();

        // 1. No-position gate + near-misses scan report + insufficient margin check.
        let far_expiry = Date::from_calendar_date(2027, Month::January, 26).unwrap();
        let dull_broker = ScriptedBroker::build_chain(25_400.0, far_expiry, 62.0, 0.06);
        let mut dull_engine = PilotEngine::new(dull_broker, test_cfg());
        let dull_scan = dull_engine.run_scan(&mut audit, weekday_open).unwrap();
        assert!(dull_scan.contains("NO_POSITION_RECOMMENDED"));
        let dull_cycle = dull_engine.run_cycle(&mut audit, weekday_open).unwrap();
        assert!(dull_cycle.contains("NO_POSITION_RECOMMENDED"));

        // Market-hours skip & guardian-only flat skip.
        assert!(
            dull_engine
                .run_cycle(&mut audit, after_hours)
                .unwrap()
                .contains("outside NSE entry window")
        );
        let mut guardian_only = PilotEngine::new(
            ScriptedBroker::build_chain(25_400.0, expiry, 6.0, 0.16),
            PilotConfig {
                allow_new_entry: false,
                ..test_cfg()
            },
        );
        assert!(
            guardian_only
                .run_cycle(&mut audit, weekday_open)
                .unwrap()
                .contains("guardian-only")
        );

        // Insufficient margin before entry.
        let mut low_margin_broker = ScriptedBroker::build_chain(25_400.0, expiry, 6.0, 0.16);
        low_margin_broker.limits.cash_paise = 100_000; // Rs 1,000 only
        let mut low_margin_engine = PilotEngine::new(low_margin_broker, test_cfg());
        assert!(
            low_margin_engine
                .run_cycle(&mut audit, weekday_open)
                .unwrap_err()
                .contains("Insufficient available margin")
        );

        // 2. Ratcheted Profit-Lock OCO across cycles:
        // Cycle A: Active Bear Call in NoLossProfitLockOco -> arms profit lock.
        // Cycle B: Adverse spike pushes exit debit up to armed stop -> closes with oco_no_loss_profit_lock_triggered!
        let mut lock_broker = ScriptedBroker::build_chain(25_200.0, expiry, 6.0, 0.16);
        lock_broker.positions = vec![
            BrokerPosition {
                exchange: "NFO".to_owned(),
                token: "1".to_owned(),
                trading_symbol: "NIFTY01DEC26C25500".to_owned(),
                product: ProductType::Margin,
                net_qty: -65,
                buy_avg_paise: None,
                sell_avg_paise: Some(11_000),
                ltp_paise: Some(6_400),
                realized_pnl_paise: 0,
                unrealized_pnl_paise: 0,
            },
            BrokerPosition {
                exchange: "NFO".to_owned(),
                token: "2".to_owned(),
                trading_symbol: "NIFTY01DEC26C25700".to_owned(),
                product: ProductType::Margin,
                net_qty: 65,
                buy_avg_paise: Some(5_000),
                sell_avg_paise: None,
                ltp_paise: Some(2_500),
                realized_pnl_paise: 0,
                unrealized_pnl_paise: 0,
            },
        ];
        lock_broker.quotes.insert(
            "NIFTY01DEC26C25500".to_owned(),
            MarketQuote {
                ltp_paise: Some(6_450),
                bid_paise: Some(6_400),
                ask_paise: Some(6_500),
                ..MarketQuote::default()
            },
        );
        lock_broker.quotes.insert(
            "NIFTY01DEC26C25700".to_owned(),
            MarketQuote {
                ltp_paise: Some(2_550),
                bid_paise: Some(2_500),
                ask_paise: Some(2_600),
                ..MarketQuote::default()
            },
        );
        let mut lock_engine = PilotEngine::new(
            lock_broker,
            PilotConfig {
                target_net_rupees: Some(3_000.0),
                order_poll_step: Duration::ZERO,
                poll_interval: Duration::ZERO,
                ..test_cfg()
            },
        );
        // Market closed hold check first.
        assert!(
            lock_engine
                .run_cycle(&mut audit, after_hours)
                .unwrap()
                .contains("Market closed; holding matched spread")
        );
        let hold_a1 = lock_engine.run_cycle(&mut audit, weekday_open).unwrap();
        assert!(hold_a1.contains("NO_LOSS_PROFIT_LOCK_OCO"), "{hold_a1}");
        // Now adverse spike pushes debit right to breakeven lock!
        lock_engine.broker_mut().quotes.insert(
            "NIFTY01DEC26C25500".to_owned(),
            MarketQuote {
                ltp_paise: Some(9_950),
                bid_paise: Some(9_900),
                ask_paise: Some(10_000),
                ..MarketQuote::default()
            },
        );
        lock_engine.broker_mut().quotes.insert(
            "NIFTY01DEC26C25700".to_owned(),
            MarketQuote {
                ltp_paise: Some(4_050),
                bid_paise: Some(4_000),
                ask_paise: Some(4_100),
                ..MarketQuote::default()
            },
        );
        let lock_closed = lock_engine.run_cycle(&mut audit, weekday_open).unwrap();
        assert!(
            lock_closed.contains("oco_no_loss_profit_lock_triggered"),
            "{lock_closed}"
        );

        // 3. Underwater -> Breakeven Scratch across cycles + run_bounded_loop!
        let mut uw_broker = ScriptedBroker::build_chain(25_350.0, expiry, 6.0, 0.16);
        uw_broker.positions = vec![
            BrokerPosition {
                exchange: "NFO".to_owned(),
                token: "1".to_owned(),
                trading_symbol: "NIFTY01DEC26C25500".to_owned(),
                product: ProductType::Margin,
                net_qty: -65,
                buy_avg_paise: None,
                sell_avg_paise: Some(9_000),
                ltp_paise: Some(10_200),
                realized_pnl_paise: 0,
                unrealized_pnl_paise: 0,
            },
            BrokerPosition {
                exchange: "NFO".to_owned(),
                token: "2".to_owned(),
                trading_symbol: "NIFTY01DEC26C25700".to_owned(),
                product: ProductType::Margin,
                net_qty: 65,
                buy_avg_paise: Some(4_000),
                sell_avg_paise: None,
                ltp_paise: Some(4_500),
                realized_pnl_paise: 0,
                unrealized_pnl_paise: 0,
            },
        ];
        uw_broker.quotes.insert(
            "NIFTY01DEC26C25500".to_owned(),
            MarketQuote {
                ltp_paise: Some(10_200),
                bid_paise: Some(10_150),
                ask_paise: Some(10_250),
                ..MarketQuote::default()
            },
        );
        uw_broker.quotes.insert(
            "NIFTY01DEC26C25700".to_owned(),
            MarketQuote {
                ltp_paise: Some(4_500),
                bid_paise: Some(4_450),
                ask_paise: Some(4_550),
                ..MarketQuote::default()
            },
        );
        let lock_dir = std::env::temp_dir().join(format!("nsp-bloop-{}", uuid::Uuid::new_v4()));
        let mut uw_engine = PilotEngine::new(
            uw_broker,
            PilotConfig {
                target_net_rupees: Some(2_500.0),
                max_loss_rupees: Some(4_000.0),
                order_poll_step: Duration::ZERO,
                poll_interval: Duration::ZERO,
                bypass_market_hours: true,
                allow_new_entry: false,
                audit_dir: lock_dir.clone(),
                ..PilotConfig::default()
            },
        );
        let hold_uw = uw_engine.run_cycle(&mut audit, weekday_open).unwrap();
        assert!(hold_uw.contains("LOSS_MINIMIZATION_OCO"), "{hold_uw}");
        // Bounce back to slight net green -> scratches immediately!
        uw_engine.broker_mut().quotes.insert(
            "NIFTY01DEC26C25500".to_owned(),
            MarketQuote {
                ltp_paise: Some(8_400),
                bid_paise: Some(8_350),
                ask_paise: Some(8_450),
                ..MarketQuote::default()
            },
        );
        uw_engine.broker_mut().quotes.insert(
            "NIFTY01DEC26C25700".to_owned(),
            MarketQuote {
                ltp_paise: Some(3_700),
                bid_paise: Some(3_650),
                ask_paise: Some(3_750),
                ..MarketQuote::default()
            },
        );
        let scratched = uw_engine.run_cycle(&mut audit, weekday_open).unwrap();
        assert!(
            scratched.contains("oco_breakeven_scratch_triggered"),
            "{scratched}"
        );
        // Bounded loop executes cleanly when flat!
        uw_engine.run_bounded_loop(&mut audit, Some(2)).unwrap();
        let _ = std::fs::remove_dir_all(lock_dir);

        // 4. Status & reconciliation on resting order without position + two same-direction legs.
        let mut weird = ScriptedBroker::build_chain(25_400.0, expiry, 6.0, 0.16);
        weird.orders.push(BrokerOrder {
            order_id: "REST1".to_owned(),
            exchange: "NFO".to_owned(),
            trading_symbol: "NIFTY01DEC26C25500".to_owned(),
            side: OrderSide::Buy,
            product: ProductType::Margin,
            status: OrderStatus::Open,
            requested_qty: 65,
            filled_qty: 0,
            avg_fill_price_paise: None,
            limit_price_paise: Some(5_000),
            trigger_price_paise: None,
            client_tag: Some("nsp-EL-stale".to_owned()),
        });
        let mut weird_engine = PilotEngine::new(weird, test_cfg());
        let st_halt = weird_engine.run_status(&mut audit, weekday_open).unwrap();
        assert!(st_halt.contains("ALERT — HALTED"));
        assert!(weird_engine.run_cycle(&mut audit, weekday_open).is_err());

        drop(audit);
        let _ = std::fs::remove_file(log_path);
    }

    #[test]
    fn partial_and_zero_fills_never_leave_naked_short_and_preserve_hedge_on_exit_failure() {
        let expiry = Date::from_calendar_date(2026, Month::December, 1).unwrap();
        let now_ist = OffsetDateTime::from_unix_timestamp(1_795_400_000)
            .unwrap()
            .replace_date(Date::from_calendar_date(2026, Month::November, 25).unwrap())
            .replace_time(Time::from_hms(11, 0, 0).unwrap());
        let (log_path, mut audit) = temp_audit();
        let fast_cfg = PilotConfig {
            order_poll_step: Duration::ZERO,
            order_wait_timeout: Duration::ZERO,
            ..test_cfg()
        };

        // Case 1: Hedge order rests OPEN -> times out -> cancelled with 0 fills -> aborts before short!
        let mut b1 = ScriptedBroker::build_chain(25_400.0, expiry, 6.0, 0.16);
        b1.scripted_fills.push_back((OrderStatus::Open, 0));
        let mut e1 = PilotEngine::new(b1, fast_cfg.clone());
        let res1 = e1.run_cycle(&mut audit, now_ist).unwrap();
        assert!(res1.contains("Entry aborted safely"), "{res1}");
        assert_eq!(e1.broker.orders.len(), 1); // Short leg never placed!

        // Case 2: Hedge AmbiguousTimeout AND not found in OrderBook -> fail-closed halt, no duplicate!
        let mut b2 = ScriptedBroker::build_chain(25_400.0, expiry, 6.0, 0.16);
        b2.timeout_drop_first_place = true;
        let mut e2 = PilotEngine::new(b2, fast_cfg.clone());
        let err2 = e2.run_cycle(&mut audit, now_ist).unwrap_err();
        assert!(err2.contains("Fail-closed halt"), "{err2}");
        assert!(e2.broker.orders.is_empty());

        // Case 3: Hedge fills 65, Short leg gets 0 fills -> excess 65 hedge immediately unwound!
        let mut b3 = ScriptedBroker::build_chain(25_400.0, expiry, 6.0, 0.16);
        b3.scripted_fills.push_back((OrderStatus::Complete, 65));
        b3.scripted_fills.push_back((OrderStatus::Rejected, 0));
        b3.scripted_fills.push_back((OrderStatus::Complete, 65)); // unwind long hedge
        let mut e3 = PilotEngine::new(b3, fast_cfg.clone());
        let res3 = e3.run_cycle(&mut audit, now_ist).unwrap();
        assert!(res3.contains("unwound 65 hedge qty cleanly"), "{res3}");
        assert!(e3.broker.positions.iter().all(|p| p.net_qty == 0));

        // Case 4: Exit short order receives 0 fills -> fails closed and keeps long hedge intact!
        let b4 = ScriptedBroker::build_chain(25_400.0, expiry, 6.0, 0.16);
        let mut e4 = PilotEngine::new(b4, fast_cfg.clone());
        assert!(
            e4.run_cycle(&mut audit, now_ist)
                .unwrap()
                .contains("ENTERED")
        );
        e4.broker_mut()
            .scripted_fills
            .push_back((OrderStatus::Rejected, 0));
        let expiry_sqoff = now_ist
            .replace_date(expiry)
            .replace_time(Time::from_hms(15, 20, 0).unwrap());
        let err4 = e4.run_cycle(&mut audit, expiry_sqoff).unwrap_err();
        assert!(err4.contains("keeping long hedge intact"), "{err4}");
        // Long hedge (+65) is still held!
        assert!(e4.broker.positions.iter().any(|p| p.net_qty == 65));
        // Add an active resting order on the spread leg and run exit again -> cancels resting order & exits cleanly!
        let short_sym4 = e4
            .broker
            .positions
            .iter()
            .find(|p| p.net_qty < 0)
            .unwrap()
            .trading_symbol
            .clone();
        e4.broker_mut().orders.push(BrokerOrder {
            order_id: "RESTING-1".to_owned(),
            exchange: "NFO".to_owned(),
            trading_symbol: short_sym4,
            side: OrderSide::Buy,
            product: ProductType::Margin,
            status: OrderStatus::Open,
            requested_qty: 65,
            filled_qty: 0,
            limit_price_paise: Some(1_000),
            trigger_price_paise: None,
            avg_fill_price_paise: None,
            client_tag: Some("manual".to_owned()),
        });
        assert!(
            e4.run_cycle(&mut audit, expiry_sqoff)
                .unwrap()
                .contains("CLOSED SPREAD")
        );

        // Case 5: Orphan long status report, Flat status report, Uncovered status report,
        // active resting order with 0 positions, unknown orphan symbol, debit-strike mismatch, and 3-leg mismatch.
        let b5 = ScriptedBroker::build_chain(25_400.0, expiry, 6.0, 0.16);
        let mut e5 = PilotEngine::new(b5, fast_cfg);
        assert!(
            e5.run_status(&mut audit, now_ist)
                .unwrap()
                .contains("Position Status: FLAT")
        );
        // Active order with 0 positions -> UncoveredOrUnrecognized!
        e5.broker_mut().orders.push(BrokerOrder {
            order_id: "ORD-OPEN-FLAT".to_owned(),
            exchange: "NFO".to_owned(),
            trading_symbol: "NIFTY01DEC26C25600".to_owned(),
            side: OrderSide::Buy,
            product: ProductType::Margin,
            status: OrderStatus::Open,
            requested_qty: 65,
            filled_qty: 0,
            limit_price_paise: Some(2_000),
            trigger_price_paise: None,
            avg_fill_price_paise: None,
            client_tag: None,
        });
        assert!(
            e5.run_status(&mut audit, now_ist)
                .unwrap()
                .contains("ALERT — HALTED")
        );
        e5.broker_mut().orders.clear();

        // Unknown orphan long symbol missing from NFO master -> UncoveredOrUnrecognized!
        e5.broker_mut().positions.push(BrokerPosition {
            exchange: "NFO".to_owned(),
            token: "9999".to_owned(),
            trading_symbol: "NIFTY_UNKNOWN_SYM".to_owned(),
            product: ProductType::Margin,
            net_qty: 65,
            buy_avg_paise: Some(5_000),
            sell_avg_paise: None,
            ltp_paise: Some(5_000),
            realized_pnl_paise: 0,
            unrealized_pnl_paise: 0,
        });
        assert!(e5.run_cycle(&mut audit, now_ist).is_err());
        e5.broker_mut().positions.clear();

        // Known orphan long -> status + market-closed deferral + active order cancel on unwind!
        e5.broker_mut().positions.push(BrokerPosition {
            exchange: "NFO".to_owned(),
            token: "1".to_owned(),
            trading_symbol: "NIFTY01DEC26C25600".to_owned(),
            product: ProductType::Margin,
            net_qty: 65,
            buy_avg_paise: Some(5_000),
            sell_avg_paise: None,
            ltp_paise: Some(5_000),
            realized_pnl_paise: 0,
            unrealized_pnl_paise: 0,
        });
        let st_orphan = e5.run_status(&mut audit, now_ist).unwrap();
        assert!(st_orphan.contains("Orphan Long Hedge Detected"));
        // Two-leg debit vertical (short strike > long strike on Calls) -> UncoveredOrUnrecognized!
        e5.broker_mut().positions.push(BrokerPosition {
            exchange: "NFO".to_owned(),
            token: "2".to_owned(),
            trading_symbol: "NIFTY01DEC26C25800".to_owned(),
            product: ProductType::Margin,
            net_qty: -65,
            buy_avg_paise: None,
            sell_avg_paise: Some(2_000),
            ltp_paise: Some(2_000),
            realized_pnl_paise: 0,
            unrealized_pnl_paise: 0,
        });
        assert!(e5.run_cycle(&mut audit, now_ist).is_err());
        // Two same-direction legs -> UncoveredOrUnrecognized!
        e5.broker_mut().positions[1].net_qty = 65;
        assert!(e5.run_cycle(&mut audit, now_ist).is_err());
        // Three legs -> UncoveredOrUnrecognized!
        e5.broker_mut().positions.push(BrokerPosition {
            exchange: "NFO".to_owned(),
            token: "3".to_owned(),
            trading_symbol: "NIFTY01DEC26C25700".to_owned(),
            product: ProductType::Margin,
            net_qty: -65,
            buy_avg_paise: None,
            sell_avg_paise: Some(3_000),
            ltp_paise: Some(3_000),
            realized_pnl_paise: 0,
            unrealized_pnl_paise: 0,
        });
        assert!(e5.run_cycle(&mut audit, now_ist).is_err());

        drop(audit);
        let _ = std::fs::remove_file(log_path);
    }

    #[test]
    fn durable_guardian_carry_forward_credit_inversion_and_sqoff_degraded_spot_work_safely() {
        let expiry = Date::from_calendar_date(2026, Month::December, 1).unwrap();
        let now_ist = OffsetDateTime::from_unix_timestamp(1_795_400_000)
            .unwrap()
            .replace_date(Date::from_calendar_date(2026, Month::November, 25).unwrap())
            .replace_time(Time::from_hms(11, 0, 0).unwrap());

        let guard_dir = std::env::temp_dir().join(format!("nsp-guard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&guard_dir).unwrap();
        let (log_path, mut audit) = temp_audit();
        let cfg = PilotConfig {
            audit_dir: guard_dir.clone(),
            bias: StrategyBias::BullPut,
            order_poll_step: Duration::ZERO,
            order_wait_timeout: Duration::ZERO,
            ..PilotConfig::default()
        };

        // 1. Secret filtering in `read_dotenv_map`, `resolve_config_var`, `Debug` redaction, & account_id check.
        let secret_env = guard_dir.join("sec.env");
        std::fs::write(
            &secret_env,
            "SHOONYA_USER_ID=FA12345\n\
             SHOONYA_ACCOUNT_ID=FA12345\n\
             SHOONYA_PASSWORD=top_secret_pw\n\
             SHOONYA_SECRET_CODE=top_secret_key\n\
             SHOONYA_TOTP_SECRET=JBSWY3DPEHPK3PXP\n\
             NIFTY_QUOTE_TOKEN=26000\n",
        )
        .unwrap();
        let filtered = read_dotenv_map(&secret_env).unwrap();
        assert!(!filtered.contains_key("SHOONYA_PASSWORD"));
        assert!(!filtered.contains_key("SHOONYA_SECRET_CODE"));
        assert!(!filtered.contains_key("SHOONYA_TOTP_SECRET"));
        assert_eq!(
            resolve_config_var(&secret_env, "SHOONYA_USER_ID")
                .unwrap()
                .as_deref(),
            Some("FA12345")
        );
        let cfg_with_acct = PilotConfig {
            account_id: Some("FA12345".to_owned()),
            ..cfg.clone()
        };
        let dbg_str = format!("{cfg_with_acct:?}");
        assert!(dbg_str.contains("<redacted>") && !dbg_str.contains("FA12345"));
        let bad_acct_res =
            PilotConfig::from_lookup(None, secret_env.clone(), guard_dir.clone(), |k| {
                Ok(if k == "SHOONYA_ACCOUNT_ID" {
                    Some("BAD!ACCT".to_owned())
                } else {
                    None
                })
            });
        assert!(bad_acct_res.is_err());

        // 2. Matched `Margin` Put spread with `excess_long_qty = 65`
        //    AND durable `ArmedGuardianState` reload across process restart!
        let mut pb = ScriptedBroker::build_chain(25_400.0, expiry, 6.0, 0.16);
        pb.positions.push(BrokerPosition {
            exchange: "NFO".to_owned(),
            token: "6001".to_owned(),
            trading_symbol: "NIFTY01DEC26P25200".to_owned(),
            product: ProductType::Margin,
            net_qty: -65,
            buy_avg_paise: None,
            sell_avg_paise: Some(11_000),
            ltp_paise: Some(7_200),
            realized_pnl_paise: 0,
            unrealized_pnl_paise: 0,
        });
        pb.positions.push(BrokerPosition {
            exchange: "NFO".to_owned(),
            token: "6002".to_owned(),
            trading_symbol: "NIFTY01DEC26P25000".to_owned(),
            product: ProductType::Margin,
            net_qty: 130, // +65 excess hedge -> trimmed first!
            buy_avg_paise: Some(5_000),
            sell_avg_paise: None,
            ltp_paise: Some(2_800),
            realized_pnl_paise: 0,
            unrealized_pnl_paise: 0,
        });
        pb.quotes.insert(
            "NIFTY01DEC26P25200".to_owned(),
            MarketQuote {
                ltp_paise: Some(7_200),
                bid_paise: Some(7_150),
                ask_paise: Some(7_250),
                ..MarketQuote::default()
            },
        );
        pb.quotes.insert(
            "NIFTY01DEC26P25000".to_owned(),
            MarketQuote {
                ltp_paise: Some(2_800),
                bid_paise: Some(2_750),
                ask_paise: Some(2_850),
                ..MarketQuote::default()
            },
        );
        let mut put_engine = PilotEngine::new(pb, cfg.clone());
        let trim_msg = put_engine.run_cycle(&mut audit, now_ist).unwrap();
        assert!(trim_msg.contains("Unwound excess hedge"), "{trim_msg}");
        let hold_msg = put_engine.run_cycle(&mut audit, now_ist).unwrap();
        assert!(hold_msg.contains("HOLD -NIFTY01DEC26P25200"), "{hold_msg}");
        assert!(guardian_state_path(&guard_dir).exists());

        // Simulate process restart (`PilotEngine::new` reloads `nifty-spread-pilot.guardian.json`)!
        let saved_broker = put_engine.broker;
        let mut restarted = PilotEngine::new(saved_broker, cfg.clone());
        assert!(restarted.armed_guardian.is_some());

        // Degrade spot quote to None during 15:20 IST expiry square-off -> pre-eval square-off STILL exits!
        restarted.broker_mut().spot_paise = 0; // `parse` / `reference_price_paise` > 0 returns None!
        let sqoff_time = now_ist
            .replace_date(expiry)
            .replace_time(Time::from_hms(15, 20, 0).unwrap());
        let sq_msg = restarted.run_cycle(&mut audit, sqoff_time).unwrap();
        assert!(sq_msg.contains("intraday_square_off_window"), "{sq_msg}");
        assert!(!guardian_state_path(&guard_dir).exists());

        // 3. Ambiguous timeout absent from OrderBook halts fail-closed, AND corrupt guardian JSON halts fail-closed!
        let mut amb_broker = ScriptedBroker::build_chain(25_400.0, expiry, 6.2, 0.16);
        amb_broker.timeout_drop_first_place = true;
        let mut amb_engine = PilotEngine::new(
            amb_broker,
            PilotConfig {
                audit_dir: guard_dir.clone(),
                order_poll_step: Duration::ZERO,
                order_wait_timeout: Duration::ZERO,
                ..PilotConfig::default()
            },
        );
        let amb_err = amb_engine.run_cycle(&mut audit, now_ist).unwrap_err();
        assert!(amb_err.contains("Fail-closed halt"), "{amb_err}");

        std::fs::write(guardian_state_path(&guard_dir), b"{corrupt").unwrap();
        let corrupt_broker = ScriptedBroker::build_chain(25_400.0, expiry, 6.0, 0.16);
        let mut corrupt_engine = PilotEngine::new(corrupt_broker, cfg.clone());
        let corrupt_err = corrupt_engine.run_cycle(&mut audit, now_ist).unwrap_err();
        assert!(
            corrupt_err.contains("Fail-closed halt: cannot load guardian state"),
            "{corrupt_err}"
        );
        std::fs::remove_file(guardian_state_path(&guard_dir)).unwrap();

        // 4. Stale pilot-tagged resting order while flat cancels & recovers on next cycle in `run_bounded_loop`!
        let mut stale_broker = ScriptedBroker::build_chain(25_400.0, expiry, 6.0, 0.16);
        stale_broker.orders.push(BrokerOrder {
            order_id: "STALE-NSP-1".to_owned(),
            exchange: "NFO".to_owned(),
            trading_symbol: "NIFTY01DEC26C25600".to_owned(),
            side: OrderSide::Buy,
            product: ProductType::Margin,
            status: OrderStatus::Open,
            requested_qty: 65,
            filled_qty: 0,
            avg_fill_price_paise: None,
            limit_price_paise: Some(3_000),
            trigger_price_paise: None,
            client_tag: Some("nspELdeadbeef1234".to_owned()),
        });
        let mut stale_engine = PilotEngine::new(
            stale_broker,
            PilotConfig {
                audit_dir: guard_dir.clone(),
                allow_new_entry: false,
                bypass_market_hours: true,
                poll_interval: Duration::from_millis(1),
                order_poll_step: Duration::ZERO,
                order_wait_timeout: Duration::ZERO,
                ..PilotConfig::default()
            },
        );
        // Cycle 1 cancels stale `nsp*` resting order (returns retryable Err), Cycle 2 runs cleanly!
        stale_engine.run_bounded_loop(&mut audit, Some(2)).unwrap();
        assert_eq!(stale_engine.broker.orders[0].status, OrderStatus::Canceled);

        // 5. Entry credit inversion unwinds long hedge cleanly, AND partial inversion unwind halts!
        let mut inv_broker = ScriptedBroker::build_chain(25_400.0, expiry, 6.2, 0.16);
        inv_broker.fill_price_override_paise = Some(99_999_900); // forces short_limit <= actual_long_cost!
        let mut inv_engine = PilotEngine::new(inv_broker, cfg.clone());
        let inv_ok = inv_engine.run_cycle(&mut audit, now_ist).unwrap();
        assert!(
            inv_ok.contains("Short quote inverted after hedge fill; unwound 65 hedge qty cleanly"),
            "{inv_ok}"
        );

        let mut inv_part_broker = ScriptedBroker::build_chain(25_400.0, expiry, 6.2, 0.16);
        inv_part_broker.fill_price_override_paise = Some(99_999_900);
        inv_part_broker
            .scripted_fills
            .push_back((OrderStatus::Complete, 65)); // hedge buy
        inv_part_broker
            .scripted_fills
            .push_back((OrderStatus::Canceled, 20)); // partial hedge unwind
        let mut inv_part_engine = PilotEngine::new(inv_part_broker, cfg.clone());
        let inv_err = inv_part_engine.run_cycle(&mut audit, now_ist).unwrap_err();
        assert!(inv_err.contains("partial hedge unwind"), "{inv_err}");

        // 6. `cancel_and_confirm_terminal` failure modes (`cancel_noop`, `cancel_drop`) + AmbiguousTimeout active cancel via `client_tag`!
        let mut noop_broker = ScriptedBroker::build_chain(25_400.0, expiry, 6.2, 0.16);
        noop_broker.cancel_noop = true;
        noop_broker.scripted_fills.push_back((OrderStatus::Open, 0));
        let mut noop_engine = PilotEngine::new(noop_broker, cfg.clone());
        let noop_err = noop_engine.run_cycle(&mut audit, now_ist).unwrap_err();
        assert!(
            noop_err.contains("remained active in OrderBook after cancel timeout"),
            "{noop_err}"
        );

        let mut drop_broker = ScriptedBroker::build_chain(25_400.0, expiry, 6.2, 0.16);
        drop_broker.cancel_drop = true;
        drop_broker.scripted_fills.push_back((OrderStatus::Open, 0));
        let mut drop_engine = PilotEngine::new(drop_broker, cfg.clone());
        let drop_err = drop_engine.run_cycle(&mut audit, now_ist).unwrap_err();
        assert!(
            drop_err.contains("disappeared from OrderBook before terminal confirmation"),
            "{drop_err}"
        );

        let mut amb_open_broker = ScriptedBroker::build_chain(25_400.0, expiry, 6.2, 0.16);
        amb_open_broker.timeout_first_place = true;
        amb_open_broker
            .scripted_fills
            .push_back((OrderStatus::Open, 0));
        let mut amb_open_engine = PilotEngine::new(amb_open_broker, cfg.clone());
        let amb_open_res = amb_open_engine.run_cycle(&mut audit, now_ist).unwrap();
        assert!(
            amb_open_res.contains("Entry aborted safely"),
            "{amb_open_res}"
        );

        // 7. `LongOnlyOrphan` deferred when market closed + cancels pilot order before unwinding when open!
        let mut orph_broker = ScriptedBroker::build_chain(25_400.0, expiry, 6.2, 0.16);
        orph_broker.positions.push(BrokerPosition {
            exchange: "NFO".to_owned(),
            token: "1002".to_owned(),
            trading_symbol: "NIFTY01DEC26C25800".to_owned(),
            product: ProductType::Margin,
            net_qty: 65,
            buy_avg_paise: Some(3_000),
            sell_avg_paise: None,
            ltp_paise: Some(3_000),
            realized_pnl_paise: 0,
            unrealized_pnl_paise: 0,
        });
        orph_broker.orders.push(BrokerOrder {
            order_id: "ORPH-ORD-1".to_owned(),
            exchange: "NFO".to_owned(),
            trading_symbol: "NIFTY01DEC26C25600".to_owned(),
            side: OrderSide::Sell,
            product: ProductType::Margin,
            status: OrderStatus::Open,
            requested_qty: 65,
            filled_qty: 0,
            avg_fill_price_paise: None,
            limit_price_paise: Some(8_000),
            trigger_price_paise: None,
            client_tag: Some("nspES1234567890ab".to_owned()),
        });
        let closed_time = now_ist.replace_time(Time::from_hms(8, 30, 0).unwrap());
        let mut orph_engine = PilotEngine::new(orph_broker, cfg.clone());
        let def_msg = orph_engine.run_cycle(&mut audit, closed_time).unwrap();
        assert!(def_msg.contains("deferred until market open"), "{def_msg}");
        let unw_msg = orph_engine.run_cycle(&mut audit, now_ist).unwrap();
        assert!(unw_msg.contains("Unwound orphan long hedge"), "{unw_msg}");
        assert_eq!(orph_engine.broker.orders[0].status, OrderStatus::Canceled);

        // 8. `ProfitLockInfeasible` arms `damage_cap_max_debit_pts` on Cycle 1 and exits on Cycle 2!
        let mut inf_broker = ScriptedBroker::build_chain(25_400.0, expiry, 6.2, 0.16);
        inf_broker.positions.push(BrokerPosition {
            exchange: "NFO".to_owned(),
            token: "7001".to_owned(),
            trading_symbol: "NIFTY01DEC26C25600".to_owned(),
            product: ProductType::Margin,
            net_qty: -65,
            buy_avg_paise: None,
            sell_avg_paise: Some(9_500),
            ltp_paise: Some(9_320),
            realized_pnl_paise: 0,
            unrealized_pnl_paise: 0,
        });
        inf_broker.positions.push(BrokerPosition {
            exchange: "NFO".to_owned(),
            token: "7002".to_owned(),
            trading_symbol: "NIFTY01DEC26C25800".to_owned(),
            product: ProductType::Margin,
            net_qty: 65,
            buy_avg_paise: Some(3_500),
            sell_avg_paise: None,
            ltp_paise: Some(3_480),
            realized_pnl_paise: 0,
            unrealized_pnl_paise: 0,
        });
        // Entry credit = 60.00 pts; charges ~0.95 pts -> E_zero ~59.05 pts.
        // Set touch exit debit = 93.40 - 34.90 = 58.50 pts (tiny net profit -> ProfitLockInfeasible!).
        inf_broker.quotes.insert(
            "NIFTY01DEC26C25600".to_owned(),
            MarketQuote {
                ltp_paise: Some(9_320),
                bid_paise: Some(9_300),
                ask_paise: Some(9_340),
                ..MarketQuote::default()
            },
        );
        inf_broker.quotes.insert(
            "NIFTY01DEC26C25800".to_owned(),
            MarketQuote {
                ltp_paise: Some(3_500),
                bid_paise: Some(3_490),
                ask_paise: Some(3_510),
                ..MarketQuote::default()
            },
        );
        let mut inf_engine = PilotEngine::new(
            inf_broker,
            PilotConfig {
                max_loss_rupees: Some(1_500.0),
                ..cfg.clone()
            },
        );
        let inf_hold = inf_engine.run_cycle(&mut audit, now_ist).unwrap();
        assert!(inf_hold.contains("PROFIT_LOCK_INFEASIBLE"), "{inf_hold}");
        assert!(
            inf_engine
                .armed_guardian
                .as_ref()
                .and_then(|g| g.damage_cap_max_debit_pts)
                .is_some()
        );
        // Adverse spike crosses `damage_cap_max_debit_pts` (~82.1 pts) without hitting widened max_loss -> `oco_damage_cap_stop_triggered`!
        let sqoff_status = inf_engine.run_status(&mut audit, sqoff_time).unwrap();
        assert!(
            sqoff_status.contains("Kill-Switch Reasons:"),
            "{sqoff_status}"
        );
        inf_engine.config.max_loss_rupees = Some(5_000.0);
        inf_engine.broker.quotes.insert(
            "NIFTY01DEC26C25600".to_owned(),
            MarketQuote {
                ltp_paise: Some(11_750),
                bid_paise: Some(11_700),
                ask_paise: Some(11_800),
                ..MarketQuote::default()
            },
        );
        let dmg_exit = inf_engine.run_cycle(&mut audit, now_ist).unwrap();
        assert!(dmg_exit.contains("CLOSED SPREAD"), "{dmg_exit}");

        // 9. `evaluate_active_spread` error (e.g. anomalous carry-forward avg credit >= width) halts intraday, yet STILL squares off at 15:20 IST!
        let mut bad_avg = ScriptedBroker::build_chain(25_400.0, expiry, 6.2, 0.16);
        bad_avg.positions.push(BrokerPosition {
            exchange: "NFO".to_owned(),
            token: "7001".to_owned(),
            trading_symbol: "NIFTY01DEC26C25600".to_owned(),
            product: ProductType::Margin,
            net_qty: -65,
            buy_avg_paise: None,
            sell_avg_paise: Some(25_000),
            ltp_paise: Some(9_000),
            realized_pnl_paise: 0,
            unrealized_pnl_paise: 0,
        });
        bad_avg.positions.push(BrokerPosition {
            exchange: "NFO".to_owned(),
            token: "7002".to_owned(),
            trading_symbol: "NIFTY01DEC26C25800".to_owned(),
            product: ProductType::Margin,
            net_qty: 65,
            buy_avg_paise: Some(3_000),
            sell_avg_paise: None,
            ltp_paise: Some(3_000),
            realized_pnl_paise: 0,
            unrealized_pnl_paise: 0,
        });
        bad_avg.quotes.insert(
            "NIFTY01DEC26C25600".to_owned(),
            MarketQuote {
                ltp_paise: None,
                bid_paise: Some(8_900),
                ask_paise: Some(9_000),
                ..MarketQuote::default()
            },
        );
        let mut bad_avg_engine = PilotEngine::new(bad_avg, cfg.clone());
        assert!(bad_avg_engine.run_cycle(&mut audit, now_ist).is_err());
        let sqoff_fallback = bad_avg_engine.run_cycle(&mut audit, sqoff_time).unwrap();
        assert!(
            sqoff_fallback.contains("intraday_square_off_window"),
            "{sqoff_fallback}"
        );

        // 10. Excess hedge with active orders halts fail-closed, partial excess unwind reports incomplete, and scan_and_enter handles missing spot!
        let mut exc_broker = ScriptedBroker::build_chain(25_400.0, expiry, 6.2, 0.16);
        exc_broker.positions.push(BrokerPosition {
            exchange: "NFO".to_owned(),
            token: "7001".to_owned(),
            trading_symbol: "NIFTY01DEC26C25600".to_owned(),
            product: ProductType::Margin,
            net_qty: -65,
            buy_avg_paise: None,
            sell_avg_paise: Some(9_000),
            ltp_paise: Some(8_000),
            realized_pnl_paise: 0,
            unrealized_pnl_paise: 0,
        });
        exc_broker.positions.push(BrokerPosition {
            exchange: "NFO".to_owned(),
            token: "7002".to_owned(),
            trading_symbol: "NIFTY01DEC26C25800".to_owned(),
            product: ProductType::Margin,
            net_qty: 130,
            buy_avg_paise: Some(3_000),
            sell_avg_paise: None,
            ltp_paise: Some(2_500),
            realized_pnl_paise: 0,
            unrealized_pnl_paise: 0,
        });
        exc_broker.orders.push(BrokerOrder {
            order_id: "ACT-EXC-1".to_owned(),
            exchange: "NFO".to_owned(),
            trading_symbol: "NIFTY01DEC26C25600".to_owned(),
            side: OrderSide::Buy,
            product: ProductType::Margin,
            status: OrderStatus::Open,
            requested_qty: 65,
            filled_qty: 0,
            avg_fill_price_paise: None,
            limit_price_paise: Some(7_000),
            trigger_price_paise: None,
            client_tag: Some("nspXS1234567890ab".to_owned()),
        });
        let mut exc_engine = PilotEngine::new(exc_broker, cfg.clone());
        let exc_err = exc_engine.run_cycle(&mut audit, now_ist).unwrap_err();
        assert!(
            exc_err.contains("active orders prevent safe excess hedge unwind"),
            "{exc_err}"
        );
        exc_engine.broker.orders.clear();
        exc_engine
            .broker
            .scripted_fills
            .push_back((OrderStatus::Canceled, 20));
        let exc_inc = exc_engine.run_cycle(&mut audit, now_ist).unwrap_err();
        assert!(
            exc_inc.contains("Excess hedge unwind incomplete"),
            "{exc_inc}"
        );

        let mut bad_scan = ScriptedBroker::build_chain(25_400.0, expiry, 6.2, 0.16);
        bad_scan.spot_paise = 0;
        let mut bad_scan_engine = PilotEngine::new(bad_scan, cfg.clone());
        assert!(bad_scan_engine.run_cycle(&mut audit, now_ist).is_err());

        drop(audit);
        let _ = std::fs::remove_file(log_path);
        let _ = std::fs::remove_dir_all(guard_dir);
    }
}
