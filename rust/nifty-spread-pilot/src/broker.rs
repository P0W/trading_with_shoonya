//! Broker-agnostic execution trait, option domain types, statutory charges,
//! and thin paper-execution overlay.
//!
//! To support another Indian broker (Zerodha Kite, Kotak Neo, Groww, Dhan,
//! Mirae m.Stock, Fyers, etc.), implement [`Broker`] for that adapter. All
//! option pricing, spread selection, OCO root-finding, reconciliation, and
//! sequenced execution logic run unchanged across any [`Broker`].

use std::collections::HashMap;
use std::fmt;

use time::{Date, OffsetDateTime};

use crate::{OptionKind, Quote};

pub fn paise_to_rupees(paise: i64) -> f64 {
    paise as f64 / 100.0
}

pub fn rupees_to_paise(rupees: f64) -> Option<i64> {
    if !rupees.is_finite() {
        return None;
    }
    let scaled = (rupees * 100.0).round();
    if scaled < (i64::MIN as f64) || scaled > (i64::MAX as f64) {
        return None;
    }
    Some(scaled as i64)
}

pub fn round_to_tick_paise(paise: i64, tick_paise: i64) -> i64 {
    if tick_paise <= 1 {
        return paise;
    }
    let half = tick_paise / 2;
    if paise >= 0 {
        ((paise + half) / tick_paise) * tick_paise
    } else {
        ((paise - half) / tick_paise) * tick_paise
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionContract {
    pub exchange: String,
    pub token: String,
    pub trading_symbol: String,
    pub underlying: String,
    pub expiry: Date,
    pub option_kind: OptionKind,
    pub strike_paise: i64,
    pub lot_size: u32,
    pub tick_paise: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiryChainContracts {
    pub expiry: Date,
    pub lot_size: u32,
    pub tick_paise: i64,
    pub contracts: Vec<OptionContract>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MarketQuote {
    pub ltp_paise: Option<i64>,
    pub prev_close_paise: Option<i64>,
    pub bid_paise: Option<i64>,
    pub ask_paise: Option<i64>,
    pub bid_qty: u32,
    pub ask_qty: u32,
    pub open_interest: Option<u64>,
    pub prev_open_interest: Option<u64>,
    pub volume: Option<u64>,
    pub lower_circuit_paise: Option<i64>,
    pub upper_circuit_paise: Option<i64>,
    pub feed_epoch_secs: Option<u64>,
}

impl MarketQuote {
    /// Validates live executable top-of-book quote for order placement/exit.
    pub fn executable_quote(&self) -> Result<Quote, &'static str> {
        let bid = self
            .bid_paise
            .filter(|&v| v > 0)
            .ok_or("Missing positive bid price.")?;
        let ask = self
            .ask_paise
            .filter(|&v| v > 0)
            .ok_or("Missing positive ask price.")?;
        if bid > ask {
            return Err("Crossed bid/ask book.");
        }
        if let (Some(lc), Some(uc)) = (self.lower_circuit_paise, self.upper_circuit_paise)
            && lc > 0
            && uc > 0
            && (lc > uc || bid < lc || ask > uc)
        {
            return Err("Top-of-book quote violates exchange circuit limits.");
        }
        Ok(Quote { bid, ask })
    }

    /// Reference mid or last price for read-only diagnostics/scans.
    pub fn reference_price_paise(&self, allow_prev_close: bool) -> Option<(i64, bool)> {
        if let Ok(touch) = self.executable_quote() {
            return Some(((touch.bid + touch.ask) / 2, false));
        }
        if let Some(ltp) = self.ltp_paise.filter(|&p| p > 0) {
            return Some((ltp, false));
        }
        if allow_prev_close && let Some(close) = self.prev_close_paise.filter(|&p| p > 0) {
            return Some((close, true));
        }
        None
    }

    /// Rounds a limit price to tick size and clamps within exchange circuit limits.
    pub fn clamp_limit_paise(&self, raw_paise: i64, tick_paise: i64) -> Result<i64, &'static str> {
        let tick = tick_paise.max(1);
        let mut rounded = round_to_tick_paise(raw_paise, tick);
        if let Some(lc) = self.lower_circuit_paise.filter(|&v| v > 0) {
            // Round lower circuit UP to tick so we never submit below lc.
            let lc_tick = ((lc + tick - 1) / tick) * tick;
            if let Some(uc) = self.upper_circuit_paise.filter(|&v| v > 0) {
                let uc_tick = (uc / tick) * tick;
                if lc_tick > uc_tick {
                    return Err("Invalid exchange circuit limits.");
                }
                rounded = rounded.clamp(lc_tick, uc_tick);
            } else {
                rounded = rounded.max(lc_tick);
            }
        } else if let Some(uc) = self.upper_circuit_paise.filter(|&v| v > 0) {
            let uc_tick = (uc / tick) * tick;
            rounded = rounded.min(uc_tick);
        }
        if rounded <= 0 {
            return Err("Computed limit price must be strictly positive.");
        }
        Ok(rounded)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccountLimits {
    pub cash_paise: i64,
    pub collateral_paise: i64,
    pub payin_paise: i64,
    pub payout_paise: i64,
    pub margin_used_paise: i64,
}

impl AccountLimits {
    pub fn available_margin_paise(&self) -> i64 {
        self.cash_paise
            .saturating_add(self.collateral_paise)
            .saturating_add(self.payin_paise)
            .saturating_sub(self.payout_paise)
            .saturating_sub(self.margin_used_paise)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProductType {
    /// Overnight carry-forward (`M` on Shoonya, `NRML` on Kite/Kotak/Groww/Fyers, `MARGIN` on Dhan).
    Margin,
    /// Intraday (`I` on Shoonya, `MIS` on Kite/Kotak/Groww, `INTRADAY` on Dhan/Fyers).
    Intraday,
    /// Unrecognized product (e.g., CNC equity) preserved so reconciliation stays aware.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    Day,
    Ioc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderPriceType {
    Limit {
        price_paise: i64,
    },
    StopLimit {
        trigger_paise: i64,
        limit_paise: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderRequest {
    pub exchange: String,
    pub token: String,
    pub trading_symbol: String,
    pub side: OrderSide,
    pub product: ProductType,
    pub quantity: u32,
    pub price_type: OrderPriceType,
    pub retention: Retention,
    pub client_tag: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderPlacement {
    pub order_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    Pending,
    Open,
    TriggerPending,
    Complete,
    Canceled,
    Rejected,
    Unknown,
}

impl OrderStatus {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Pending | Self::Open | Self::TriggerPending | Self::Unknown
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Open => "open",
            Self::TriggerPending => "trigger_pending",
            Self::Complete => "complete",
            Self::Canceled => "canceled",
            Self::Rejected => "rejected",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerOrder {
    pub order_id: String,
    pub exchange: String,
    pub trading_symbol: String,
    pub side: OrderSide,
    pub product: ProductType,
    pub status: OrderStatus,
    pub requested_qty: u32,
    pub filled_qty: u32,
    pub avg_fill_price_paise: Option<i64>,
    pub limit_price_paise: Option<i64>,
    pub trigger_price_paise: Option<i64>,
    pub client_tag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerPosition {
    pub exchange: String,
    pub token: String,
    pub trading_symbol: String,
    pub product: ProductType,
    pub net_qty: i32,
    pub buy_avg_paise: Option<i64>,
    pub sell_avg_paise: Option<i64>,
    pub ltp_paise: Option<i64>,
    pub realized_pnl_paise: i64,
    pub unrealized_pnl_paise: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerError {
    AuthRequired(String),
    RateLimited,
    AmbiguousTimeout {
        stage: &'static str,
    },
    Rejected {
        stage: &'static str,
        category: &'static str,
    },
    Transport(String),
    InvalidResponse(String),
}

impl fmt::Display for BrokerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthRequired(msg) => write!(f, "Authentication required: {msg}"),
            Self::RateLimited => write!(f, "Broker rate limit reached."),
            Self::AmbiguousTimeout { stage } => {
                write!(f, "Broker {stage} timed out; reconciliation required.")
            }
            Self::Rejected { stage, category } => {
                write!(f, "Broker rejected {stage} ({category}).")
            }
            Self::Transport(msg) => write!(f, "{msg}"),
            Self::InvalidResponse(msg) => write!(f, "{msg}"),
        }
    }
}

/// Configurable statutory + brokerage fee schedule for Indian NSE F&O options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeeSchedule {
    pub brokerage_per_order_rs: f64,
    pub stt_sell_rate: f64,
    pub exchange_txn_rate: f64,
    pub sebi_ipft_rate: f64,
    pub stamp_duty_buy_rate: f64,
    pub gst_rate: f64,
}

impl FeeSchedule {
    /// Shoonya (Finvasia) 2026 F&O Options schedule:
    /// Rs 5/executed order, 0.15% sell STT (Apr 2026), 0.03553% NSE txn (Mar 2026),
    /// Rs 60/crore SEBI+IPFT, 0.003% buy stamp duty, 18% GST.
    pub const fn shoonya_2026() -> Self {
        Self {
            brokerage_per_order_rs: 5.0,
            stt_sell_rate: 0.0015,
            exchange_txn_rate: 0.000_355_3,
            sebi_ipft_rate: 0.000_006,
            stamp_duty_buy_rate: 0.000_03,
            gst_rate: 0.18,
        }
    }

    /// Zerodha Kite 2026 F&O Options schedule (Rs 20/order).
    pub const fn zerodha_2026() -> Self {
        Self {
            brokerage_per_order_rs: 20.0,
            ..Self::shoonya_2026()
        }
    }

    /// Zero-brokerage F&O schedule (e.g., Kotak Neo / Mirae m.Stock zero-brokerage plans).
    pub const fn zero_brokerage_2026() -> Self {
        Self {
            brokerage_per_order_rs: 0.0,
            ..Self::shoonya_2026()
        }
    }

    pub fn compute_charges(
        &self,
        short_entry_rs: f64,
        long_entry_rs: f64,
        short_exit_rs: f64,
        long_exit_rs: f64,
        qty: u32,
        num_orders: u32,
    ) -> ChargeBreakdown {
        let q = f64::from(qty);
        let buy_turnover = (long_entry_rs.max(0.0) + short_exit_rs.max(0.0)) * q;
        let sell_turnover = (short_entry_rs.max(0.0) + long_exit_rs.max(0.0)) * q;
        let total_turnover = buy_turnover + sell_turnover;

        let brokerage = self.brokerage_per_order_rs * f64::from(num_orders);
        let stt = sell_turnover * self.stt_sell_rate;
        let exchange_txn = total_turnover * self.exchange_txn_rate;
        let sebi_ipft = total_turnover * self.sebi_ipft_rate;
        let stamp_duty = buy_turnover * self.stamp_duty_buy_rate;
        let gst = (brokerage + exchange_txn + sebi_ipft) * self.gst_rate;
        let total = brokerage + stt + exchange_txn + sebi_ipft + stamp_duty + gst;

        ChargeBreakdown {
            buy_turnover_rs: buy_turnover,
            sell_turnover_rs: sell_turnover,
            brokerage_rs: brokerage,
            stt_rs: stt,
            exchange_txn_rs: exchange_txn,
            sebi_ipft_rs: sebi_ipft,
            stamp_duty_rs: stamp_duty,
            gst_rs: gst,
            total_charges_rs: total,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChargeBreakdown {
    pub buy_turnover_rs: f64,
    pub sell_turnover_rs: f64,
    pub brokerage_rs: f64,
    pub stt_rs: f64,
    pub exchange_txn_rs: f64,
    pub sebi_ipft_rs: f64,
    pub stamp_duty_rs: f64,
    pub gst_rs: f64,
    pub total_charges_rs: f64,
}

/// Broker-agnostic adapter contract for plugging in Shoonya, Kite, Kotak, Groww, Dhan, Mirae, or Fyers.
pub trait Broker {
    /// Returns the short lowercase identifier of the broker adapter (e.g., `"shoonya"`, `"kite"`).
    fn name(&self) -> &'static str;
    /// Returns the broker brokerage and NSE statutory fee schedule used for net P&L and OCO floors.
    fn fee_schedule(&self) -> FeeSchedule;
    /// Ensures an authenticated broker session is active, refreshing or logging in if needed.
    fn ensure_session(&mut self) -> Result<(), BrokerError>;
    /// Discovers the nearest tradable expiry's full Call/Put contract chain for `underlying` at `now_ist`.
    fn discover_option_chain_contracts(
        &mut self,
        underlying: &str,
        now_ist: OffsetDateTime,
    ) -> Result<ExpiryChainContracts, BrokerError>;
    /// Resolves held or carried-over option trading symbols into typed `OptionContract` metadata.
    fn resolve_contracts_by_symbols(
        &mut self,
        underlying: &str,
        symbols: &[String],
        now_ist: OffsetDateTime,
    ) -> Result<Vec<OptionContract>, BrokerError>;
    /// Returns true if `(exchange, trading_symbol)` is an option contract (not a future) on `underlying`.
    fn matches_option_underlying(
        &self,
        exchange: &str,
        trading_symbol: &str,
        underlying: &str,
    ) -> bool {
        if !matches!(exchange, "NFO" | "BFO") {
            return false;
        }
        let sym = trading_symbol.rsplit(':').next().unwrap_or(trading_symbol);
        if sym.to_ascii_uppercase().ends_with("FUT") {
            return false;
        }
        sym.strip_prefix(underlying)
            .is_some_and(|rest| !rest.is_empty() && !rest.as_bytes()[0].is_ascii_alphabetic())
    }
    /// Fetches the live spot quote for `underlying` (e.g., `"NIFTY"`).
    fn get_spot_quote(&mut self, underlying: &str) -> Result<MarketQuote, BrokerError>;
    /// Fetches live quotes (LTP, best bid/ask, OI, circuit limits) for the requested option contracts.
    fn get_option_quotes(
        &mut self,
        contracts: &[OptionContract],
    ) -> Result<Vec<(OptionContract, MarketQuote)>, BrokerError>;
    /// Fetches live free cash, collateral, and margin utilized for pre-trade margin gating.
    fn get_account_limits(&mut self) -> Result<AccountLimits, BrokerError>;
    /// Fetches all current net positions across the account for spread reconciliation.
    fn get_positions(&mut self) -> Result<Vec<BrokerPosition>, BrokerError>;
    /// Fetches today's order book (resting, partial, filled, cancelled, rejected) for fill reconciliation.
    fn get_orders(&mut self) -> Result<Vec<BrokerOrder>, BrokerError>;
    /// Submits a single-leg order (`LIMIT`, `SL-LMT`, IOC/DAY) and returns the broker order ID.
    fn place_order(&mut self, request: &OrderRequest) -> Result<OrderPlacement, BrokerError>;
    /// Cancels an open or trigger-pending order by its broker `order_id`.
    fn cancel_order(&mut self, order_id: &str) -> Result<(), BrokerError>;
}

/// Thin order-punching overlay over any live [`Broker`].
///
/// Market discovery, spot quotes, option chain quotes, OAuth session, and account
/// limits all come directly from the underlying live broker `B`, while order
/// submission/cancellation is simulated against `B`'s live executable quotes.
pub struct PaperBroker<B> {
    inner: B,
    next_id: u64,
    seed_from_live: bool,
    positions_seeded: bool,
    orders: Vec<BrokerOrder>,
    positions: HashMap<String, BrokerPosition>,
    latest_quotes: HashMap<String, (OptionContract, MarketQuote)>,
}

impl<B: Broker> PaperBroker<B> {
    /// Clean-slate paper broker: starts flat without mutating or re-importing live positions.
    pub fn new(inner: B) -> Self {
        Self {
            inner,
            next_id: 1,
            seed_from_live: false,
            positions_seeded: true,
            orders: Vec::new(),
            positions: HashMap::new(),
            latest_quotes: HashMap::new(),
        }
    }

    /// Paper broker seeded once from `inner.get_positions()` on first position query/fill,
    /// then maintained purely inside the local paper ledger (never falling back to live again).
    pub fn from_live_snapshot(inner: B) -> Self {
        Self {
            inner,
            next_id: 1,
            seed_from_live: true,
            positions_seeded: false,
            orders: Vec::new(),
            positions: HashMap::new(),
            latest_quotes: HashMap::new(),
        }
    }

    pub fn into_inner(self) -> B {
        self.inner
    }

    fn ensure_seeded(&mut self) -> Result<(), BrokerError> {
        if !self.positions_seeded {
            if self.seed_from_live {
                for pos in self.inner.get_positions()? {
                    self.positions.insert(pos.trading_symbol.clone(), pos);
                }
            }
            self.positions_seeded = true;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_fill_fields(
        &mut self,
        exchange: &str,
        token: &str,
        trading_symbol: &str,
        side: OrderSide,
        product: ProductType,
        quantity: u32,
        fill_price_paise: i64,
    ) {
        let signed_qty = match side {
            OrderSide::Buy => quantity as i32,
            OrderSide::Sell => -(quantity as i32),
        };
        let entry = self
            .positions
            .entry(trading_symbol.to_owned())
            .or_insert_with(|| BrokerPosition {
                exchange: exchange.to_owned(),
                token: token.to_owned(),
                trading_symbol: trading_symbol.to_owned(),
                product,
                net_qty: 0,
                buy_avg_paise: None,
                sell_avg_paise: None,
                ltp_paise: Some(fill_price_paise),
                realized_pnl_paise: 0,
                unrealized_pnl_paise: 0,
            });
        let old_qty = entry.net_qty;
        let new_qty = old_qty + signed_qty;
        entry.ltp_paise = Some(fill_price_paise);

        if old_qty == 0 || old_qty.signum() == signed_qty.signum() {
            let old_abs = i64::from(old_qty.abs());
            let add_abs = i64::from(signed_qty.abs());
            let new_abs = i64::from(new_qty.abs());
            match side {
                OrderSide::Buy => {
                    let prev = entry.buy_avg_paise.unwrap_or(fill_price_paise);
                    entry.buy_avg_paise =
                        Some((prev * old_abs + fill_price_paise * add_abs) / new_abs);
                    entry.sell_avg_paise = None;
                }
                OrderSide::Sell => {
                    let prev = entry.sell_avg_paise.unwrap_or(fill_price_paise);
                    entry.sell_avg_paise =
                        Some((prev * old_abs + fill_price_paise * add_abs) / new_abs);
                    entry.buy_avg_paise = None;
                }
            }
        } else {
            let closed_qty = i64::from(old_qty.abs().min(signed_qty.abs()));
            if old_qty > 0 {
                if let Some(open_buy) = entry.buy_avg_paise {
                    entry.realized_pnl_paise += (fill_price_paise - open_buy) * closed_qty;
                }
            } else if let Some(open_sell) = entry.sell_avg_paise {
                entry.realized_pnl_paise += (open_sell - fill_price_paise) * closed_qty;
            }
            if new_qty == 0 {
                entry.buy_avg_paise = None;
                entry.sell_avg_paise = None;
            } else if new_qty.signum() != old_qty.signum() {
                match side {
                    OrderSide::Buy => {
                        entry.buy_avg_paise = Some(fill_price_paise);
                        entry.sell_avg_paise = None;
                    }
                    OrderSide::Sell => {
                        entry.sell_avg_paise = Some(fill_price_paise);
                        entry.buy_avg_paise = None;
                    }
                }
            }
        }
        entry.net_qty = new_qty;
    }

    fn evaluate_fill(
        side: OrderSide,
        limit_paise: Option<i64>,
        trigger_paise: Option<i64>,
        quote: &MarketQuote,
        touch: Quote,
    ) -> Option<i64> {
        match (limit_paise, trigger_paise) {
            (Some(price_paise), None) => match side {
                OrderSide::Buy => (price_paise >= touch.ask).then_some(touch.ask),
                OrderSide::Sell => (price_paise <= touch.bid).then_some(touch.bid),
            },
            (Some(limit_p), Some(trig_p)) => {
                let ltp = quote.ltp_paise.unwrap_or((touch.bid + touch.ask) / 2);
                let triggered = match side {
                    OrderSide::Buy => ltp >= trig_p && limit_p >= touch.ask,
                    OrderSide::Sell => ltp <= trig_p && limit_p <= touch.bid,
                };
                triggered.then_some(match side {
                    OrderSide::Buy => touch.ask,
                    OrderSide::Sell => touch.bid,
                })
            }
            _ => None,
        }
    }
}

impl<B: Broker> Broker for PaperBroker<B> {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn fee_schedule(&self) -> FeeSchedule {
        self.inner.fee_schedule()
    }

    fn ensure_session(&mut self) -> Result<(), BrokerError> {
        self.inner.ensure_session()
    }

    fn discover_option_chain_contracts(
        &mut self,
        underlying: &str,
        now_ist: OffsetDateTime,
    ) -> Result<ExpiryChainContracts, BrokerError> {
        self.inner
            .discover_option_chain_contracts(underlying, now_ist)
    }

    fn resolve_contracts_by_symbols(
        &mut self,
        underlying: &str,
        symbols: &[String],
        now_ist: OffsetDateTime,
    ) -> Result<Vec<OptionContract>, BrokerError> {
        self.inner
            .resolve_contracts_by_symbols(underlying, symbols, now_ist)
    }

    fn matches_option_underlying(
        &self,
        exchange: &str,
        trading_symbol: &str,
        underlying: &str,
    ) -> bool {
        self.inner
            .matches_option_underlying(exchange, trading_symbol, underlying)
    }

    fn get_spot_quote(&mut self, underlying: &str) -> Result<MarketQuote, BrokerError> {
        self.inner.get_spot_quote(underlying)
    }

    fn get_option_quotes(
        &mut self,
        contracts: &[OptionContract],
    ) -> Result<Vec<(OptionContract, MarketQuote)>, BrokerError> {
        let pairs = self.inner.get_option_quotes(contracts)?;
        let mut pending_fills = Vec::new();
        for (contract, quote) in &pairs {
            self.latest_quotes
                .insert(contract.trading_symbol.clone(), (contract.clone(), *quote));
            if let Some(pos) = self.positions.get_mut(&contract.trading_symbol) {
                pos.ltp_paise = quote.ltp_paise.or(pos.ltp_paise);
            }
            if let Ok(touch) = quote.executable_quote() {
                for (idx, ord) in self.orders.iter_mut().enumerate() {
                    if ord.trading_symbol == contract.trading_symbol
                        && matches!(ord.status, OrderStatus::Open | OrderStatus::TriggerPending)
                        && let Some(fill) = Self::evaluate_fill(
                            ord.side,
                            ord.limit_price_paise,
                            ord.trigger_price_paise,
                            quote,
                            touch,
                        )
                    {
                        let rem = ord.requested_qty.saturating_sub(ord.filled_qty);
                        if rem > 0 {
                            ord.status = OrderStatus::Complete;
                            ord.filled_qty = ord.requested_qty;
                            ord.avg_fill_price_paise = Some(fill);
                            pending_fills.push((
                                idx,
                                ord.exchange.clone(),
                                contract.token.clone(),
                                ord.trading_symbol.clone(),
                                ord.side,
                                ord.product,
                                rem,
                                fill,
                            ));
                        }
                    }
                }
            }
        }
        for (_, exch, tok, sym, side, prod, qty, fill) in pending_fills {
            self.apply_fill_fields(&exch, &tok, &sym, side, prod, qty, fill);
        }
        Ok(pairs)
    }

    fn get_account_limits(&mut self) -> Result<AccountLimits, BrokerError> {
        self.inner.get_account_limits()
    }

    fn get_positions(&mut self) -> Result<Vec<BrokerPosition>, BrokerError> {
        self.ensure_seeded()?;
        Ok(self.positions.values().cloned().collect())
    }

    fn get_orders(&mut self) -> Result<Vec<BrokerOrder>, BrokerError> {
        Ok(self.orders.clone())
    }

    fn place_order(&mut self, request: &OrderRequest) -> Result<OrderPlacement, BrokerError> {
        self.ensure_seeded()?;
        if request.quantity == 0 {
            return Err(BrokerError::Rejected {
                stage: "PlaceOrder",
                category: "invalid quantity",
            });
        }
        let (contract, quote) = self
            .latest_quotes
            .get(&request.trading_symbol)
            .cloned()
            .ok_or(BrokerError::Rejected {
                stage: "PlaceOrder",
                category: "missing fresh quote for symbol",
            })?;
        let touch = quote
            .executable_quote()
            .map_err(|_| BrokerError::Rejected {
                stage: "PlaceOrder",
                category: "non-executable quote",
            })?;
        let order_id = format!("PAPER-{}", self.next_id);
        self.next_id += 1;

        let (limit_paise, trigger_paise) = match request.price_type {
            OrderPriceType::Limit { price_paise } => (Some(price_paise), None),
            OrderPriceType::StopLimit {
                trigger_paise,
                limit_paise,
            } => (Some(limit_paise), Some(trigger_paise)),
        };
        let fill_price =
            Self::evaluate_fill(request.side, limit_paise, trigger_paise, &quote, touch);

        let (status, filled_qty, avg_fill_price_paise) = if let Some(fill) = fill_price {
            let tok = if request.token.is_empty() {
                contract.token.as_str()
            } else {
                request.token.as_str()
            };
            self.apply_fill_fields(
                &request.exchange,
                tok,
                &request.trading_symbol,
                request.side,
                request.product,
                request.quantity,
                fill,
            );
            (OrderStatus::Complete, request.quantity, Some(fill))
        } else if request.retention == Retention::Ioc {
            (OrderStatus::Canceled, 0, None)
        } else if trigger_paise.is_some() {
            (OrderStatus::TriggerPending, 0, None)
        } else {
            (OrderStatus::Open, 0, None)
        };

        self.orders.push(BrokerOrder {
            order_id: order_id.clone(),
            exchange: request.exchange.clone(),
            trading_symbol: request.trading_symbol.clone(),
            side: request.side,
            product: request.product,
            status,
            requested_qty: request.quantity,
            filled_qty,
            avg_fill_price_paise,
            limit_price_paise: limit_paise,
            trigger_price_paise: trigger_paise,
            client_tag: Some(request.client_tag.clone()),
        });
        Ok(OrderPlacement { order_id })
    }

    fn cancel_order(&mut self, order_id: &str) -> Result<(), BrokerError> {
        let order = self
            .orders
            .iter_mut()
            .find(|o| o.order_id == order_id)
            .ok_or(BrokerError::Rejected {
                stage: "CancelOrder",
                category: "unknown order id",
            })?;
        if matches!(
            order.status,
            OrderStatus::Open | OrderStatus::TriggerPending | OrderStatus::Pending
        ) {
            order.status = OrderStatus::Canceled;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Month;

    struct DummyBroker {
        quote: MarketQuote,
        positions: Vec<BrokerPosition>,
    }

    impl Broker for DummyBroker {
        fn name(&self) -> &'static str {
            "dummy"
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
            now_ist: OffsetDateTime,
        ) -> Result<ExpiryChainContracts, BrokerError> {
            Ok(ExpiryChainContracts {
                expiry: now_ist.date(),
                lot_size: 65,
                tick_paise: 5,
                contracts: vec![],
            })
        }
        fn resolve_contracts_by_symbols(
            &mut self,
            _underlying: &str,
            _symbols: &[String],
            _now_ist: OffsetDateTime,
        ) -> Result<Vec<OptionContract>, BrokerError> {
            Ok(vec![])
        }
        fn get_spot_quote(&mut self, _underlying: &str) -> Result<MarketQuote, BrokerError> {
            Ok(self.quote)
        }
        fn get_option_quotes(
            &mut self,
            contracts: &[OptionContract],
        ) -> Result<Vec<(OptionContract, MarketQuote)>, BrokerError> {
            Ok(contracts.iter().cloned().map(|c| (c, self.quote)).collect())
        }
        fn get_account_limits(&mut self) -> Result<AccountLimits, BrokerError> {
            Ok(AccountLimits {
                cash_paise: 20_000_000,
                collateral_paise: 5_000_000,
                payin_paise: 0,
                payout_paise: 1_000_000,
                margin_used_paise: 4_000_000,
            })
        }
        fn get_positions(&mut self) -> Result<Vec<BrokerPosition>, BrokerError> {
            Ok(self.positions.clone())
        }
        fn get_orders(&mut self) -> Result<Vec<BrokerOrder>, BrokerError> {
            Ok(vec![])
        }
        fn place_order(&mut self, _request: &OrderRequest) -> Result<OrderPlacement, BrokerError> {
            unreachable!("PaperBroker intercepts order placement")
        }
        fn cancel_order(&mut self, _order_id: &str) -> Result<(), BrokerError> {
            unreachable!("PaperBroker intercepts order cancellation")
        }
    }

    fn sample_contract() -> OptionContract {
        OptionContract {
            exchange: "NFO".to_owned(),
            token: "1001".to_owned(),
            trading_symbol: "NIFTY01DEC26C25500".to_owned(),
            underlying: "NIFTY".to_owned(),
            expiry: Date::from_calendar_date(2026, Month::December, 1).unwrap(),
            option_kind: OptionKind::Call,
            strike_paise: 2_550_000,
            lot_size: 65,
            tick_paise: 5,
        }
    }

    #[test]
    fn currency_and_tick_helpers_handle_edges() {
        assert_eq!(paise_to_rupees(12_345), 123.45);
        assert_eq!(rupees_to_paise(123.45), Some(12_345));
        assert_eq!(rupees_to_paise(f64::NAN), None);
        assert_eq!(rupees_to_paise(f64::INFINITY), None);
        assert_eq!(rupees_to_paise(1e30), None);
        assert_eq!(round_to_tick_paise(1_003, 5), 1_005);
        assert_eq!(round_to_tick_paise(1_002, 5), 1_000);
        assert_eq!(round_to_tick_paise(-1_003, 5), -1_005);
        assert_eq!(round_to_tick_paise(1_003, 1), 1_003);
    }

    #[test]
    fn quote_validation_and_circuit_clamping_fail_closed() {
        let valid = MarketQuote {
            ltp_paise: Some(10_000),
            prev_close_paise: Some(9_500),
            bid_paise: Some(9_995),
            ask_paise: Some(10_005),
            lower_circuit_paise: Some(500),
            upper_circuit_paise: Some(25_000),
            ..MarketQuote::default()
        };
        assert_eq!(
            valid.executable_quote(),
            Ok(Quote {
                bid: 9_995,
                ask: 10_005
            })
        );
        assert_eq!(valid.reference_price_paise(false), Some((10_000, false)));
        assert_eq!(valid.clamp_limit_paise(30_000, 5), Ok(25_000));
        assert_eq!(valid.clamp_limit_paise(100, 5), Ok(500));

        let closed = MarketQuote {
            prev_close_paise: Some(9_500),
            ..MarketQuote::default()
        };
        assert!(closed.executable_quote().is_err());
        assert_eq!(closed.reference_price_paise(false), None);
        assert_eq!(closed.reference_price_paise(true), Some((9_500, true)));

        let ltp_only = MarketQuote {
            ltp_paise: Some(9_800),
            ..MarketQuote::default()
        };
        assert_eq!(ltp_only.reference_price_paise(false), Some((9_800, false)));
        assert_eq!(ltp_only.clamp_limit_paise(9_803, 5), Ok(9_805));
        assert!(ltp_only.clamp_limit_paise(0, 5).is_err());

        let crossed = MarketQuote {
            bid_paise: Some(10_100),
            ask_paise: Some(10_000),
            ..MarketQuote::default()
        };
        assert!(crossed.executable_quote().is_err());

        let bad_circuit = MarketQuote {
            bid_paise: Some(9_995),
            ask_paise: Some(10_005),
            lower_circuit_paise: Some(20_000),
            upper_circuit_paise: Some(10_000),
            ..MarketQuote::default()
        };
        assert!(bad_circuit.executable_quote().is_err());
        assert!(bad_circuit.clamp_limit_paise(15_000, 5).is_err());
    }

    #[test]
    fn fee_schedules_and_limits_compute_expected_values() {
        let shoonya = FeeSchedule::shoonya_2026();
        let zerodha = FeeSchedule::zerodha_2026();
        let zero = FeeSchedule::zero_brokerage_2026();
        let c_shoonya = shoonya.compute_charges(120.0, 60.0, 40.0, 20.0, 65, 4);
        let c_zerodha = zerodha.compute_charges(120.0, 60.0, 40.0, 20.0, 65, 4);
        let c_zero = zero.compute_charges(120.0, 60.0, 40.0, 20.0, 65, 4);
        assert!((c_shoonya.brokerage_rs - 20.0).abs() < 1e-9);
        assert!((c_zerodha.brokerage_rs - 80.0).abs() < 1e-9);
        assert_eq!(c_zero.brokerage_rs, 0.0);
        assert!(c_zerodha.total_charges_rs > c_shoonya.total_charges_rs);
        assert!(c_shoonya.total_charges_rs > c_zero.total_charges_rs);
    }

    #[test]
    fn paper_broker_simulates_limit_stop_ioc_and_cancel_against_live_quotes() {
        let quote = MarketQuote {
            ltp_paise: Some(10_000),
            bid_paise: Some(9_950),
            ask_paise: Some(10_050),
            ..MarketQuote::default()
        };
        let mut paper = PaperBroker::new(DummyBroker {
            quote,
            positions: vec![],
        });
        assert_eq!(paper.name(), "dummy");
        paper.ensure_session().unwrap();
        let now = OffsetDateTime::now_utc();
        assert_eq!(
            paper
                .discover_option_chain_contracts("NIFTY", now)
                .unwrap()
                .lot_size,
            65
        );
        assert_eq!(
            paper.get_account_limits().unwrap().available_margin_paise(),
            20_000_000
        );
        assert!(paper.get_positions().unwrap().is_empty());
        let _ = paper.get_spot_quote("NIFTY").unwrap();

        let contract = sample_contract();
        let _ = paper
            .get_option_quotes(std::slice::from_ref(&contract))
            .unwrap();

        // Zero qty rejected.
        let mut req = OrderRequest {
            exchange: "NFO".to_owned(),
            token: contract.token.clone(),
            trading_symbol: contract.trading_symbol.clone(),
            side: OrderSide::Buy,
            product: ProductType::Margin,
            quantity: 0,
            price_type: OrderPriceType::Limit {
                price_paise: 10_100,
            },
            retention: Retention::Day,
            client_tag: "t0".to_owned(),
        };
        assert!(paper.place_order(&req).is_err());

        // Resting DAY limit buy below ask -> Open, then cancel.
        req.quantity = 65;
        req.price_type = OrderPriceType::Limit { price_paise: 9_900 };
        let open_id = paper.place_order(&req).unwrap().order_id;
        assert_eq!(paper.get_orders().unwrap()[0].status, OrderStatus::Open);
        paper.cancel_order(&open_id).unwrap();
        assert_eq!(paper.get_orders().unwrap()[0].status, OrderStatus::Canceled);

        // Unmarketable IOC -> Canceled immediately.
        req.retention = Retention::Ioc;
        let _ = paper.place_order(&req).unwrap();
        assert_eq!(paper.get_orders().unwrap()[1].status, OrderStatus::Canceled);

        // StopLimit pending vs triggered.
        req.retention = Retention::Day;
        req.price_type = OrderPriceType::StopLimit {
            trigger_paise: 10_500,
            limit_paise: 10_600,
        };
        let sl_id = paper.place_order(&req).unwrap().order_id;
        assert_eq!(
            paper.get_orders().unwrap()[2].status,
            OrderStatus::TriggerPending
        );
        assert!(paper.get_orders().unwrap()[2].status.is_active());
        assert_eq!(
            paper.get_orders().unwrap()[2].status.as_str(),
            "trigger_pending"
        );
        paper.cancel_order(&sl_id).unwrap();

        // Marketable Buy then Sell updates positions cleanly.
        req.price_type = OrderPriceType::Limit {
            price_paise: 10_050,
        };
        let _ = paper.place_order(&req).unwrap();
        assert_eq!(paper.get_positions().unwrap()[0].net_qty, 65);
        req.side = OrderSide::Sell;
        req.price_type = OrderPriceType::StopLimit {
            trigger_paise: 10_000,
            limit_paise: 9_950,
        };
        let _ = paper.place_order(&req).unwrap();
        assert_eq!(paper.get_positions().unwrap()[0].net_qty, 0);
        assert_eq!(paper.get_positions().unwrap()[0].buy_avg_paise, None);
        assert_eq!(paper.get_positions().unwrap()[0].sell_avg_paise, None);

        // Resting Open order fills when subsequent get_option_quotes becomes marketable!
        req.side = OrderSide::Buy;
        req.price_type = OrderPriceType::Limit { price_paise: 9_900 };
        let resting_id = paper.place_order(&req).unwrap().order_id;
        assert!(
            paper
                .get_orders()
                .unwrap()
                .iter()
                .any(|o| o.order_id == resting_id && o.status == OrderStatus::Open)
        );
        paper.inner.quote = MarketQuote {
            ltp_paise: Some(9_850),
            bid_paise: Some(9_800),
            ask_paise: Some(9_880),
            ..MarketQuote::default()
        };
        let _ = paper
            .get_option_quotes(std::slice::from_ref(&contract))
            .unwrap();
        assert!(
            paper
                .get_orders()
                .unwrap()
                .iter()
                .any(|o| o.order_id == resting_id && o.status == OrderStatus::Complete)
        );
        assert_eq!(paper.get_positions().unwrap()[0].net_qty, 65);
        assert_eq!(paper.get_positions().unwrap()[0].buy_avg_paise, Some(9_880));

        // Verify position sign flips (+65 -> -65 -> +65 -> 0) and non-empty from_live_snapshot!
        req.side = OrderSide::Sell;
        req.quantity = 130;
        req.price_type = OrderPriceType::Limit { price_paise: 9_800 };
        let _ = paper.place_order(&req).unwrap();
        assert_eq!(paper.get_positions().unwrap()[0].net_qty, -65);
        assert_eq!(
            paper.get_positions().unwrap()[0].sell_avg_paise,
            Some(9_800)
        );
        assert_eq!(paper.get_positions().unwrap()[0].buy_avg_paise, None);

        req.side = OrderSide::Buy;
        req.quantity = 130;
        req.price_type = OrderPriceType::Limit { price_paise: 9_900 };
        let _ = paper.place_order(&req).unwrap();
        assert_eq!(paper.get_positions().unwrap()[0].net_qty, 65);
        assert_eq!(paper.get_positions().unwrap()[0].buy_avg_paise, Some(9_880));
        assert_eq!(paper.get_positions().unwrap()[0].sell_avg_paise, None);

        // Verify matches_option_underlying rejects NSE ETFs, sibling indices, and futures.
        assert!(paper.matches_option_underlying("NFO", "NIFTY01DEC26C25500", "NIFTY"));
        assert!(!paper.matches_option_underlying("NSE", "NIFTYBEES", "NIFTY"));
        assert!(!paper.matches_option_underlying("NFO", "NIFTYNXT5001DEC26C70000", "NIFTY"));
        assert!(!paper.matches_option_underlying("NFO", "NIFTY01DEC26FUT", "NIFTY"));

        // Verify from_live_snapshot seeds once without reverting.
        let mut snap = PaperBroker::from_live_snapshot(DummyBroker {
            quote,
            positions: vec![BrokerPosition {
                exchange: "NFO".to_owned(),
                token: "1001".to_owned(),
                trading_symbol: "NIFTY01DEC26C25500".to_owned(),
                product: ProductType::Margin,
                net_qty: -65,
                buy_avg_paise: None,
                sell_avg_paise: Some(10_000),
                ltp_paise: Some(9_800),
                realized_pnl_paise: 0,
                unrealized_pnl_paise: 13_000,
            }],
        });
        assert_eq!(snap.get_positions().unwrap().len(), 1);
        assert_eq!(snap.get_positions().unwrap()[0].net_qty, -65);

        // Exercise resolve_contracts_by_symbols, fee_schedule, quote refresh on existing position, and unknown cancel.
        assert_eq!(
            paper.fee_schedule().brokerage_per_order_rs,
            FeeSchedule::shoonya_2026().brokerage_per_order_rs
        );
        assert!(
            paper
                .resolve_contracts_by_symbols(
                    "NIFTY",
                    std::slice::from_ref(&contract.trading_symbol),
                    now,
                )
                .unwrap()
                .is_empty()
        );
        let _ = paper
            .get_option_quotes(std::slice::from_ref(&contract))
            .unwrap();
        assert!(paper.cancel_order("UNKNOWN-ORD").is_err());
        let _inner = paper.into_inner();
    }

    #[test]
    fn broker_error_and_status_formatting_cover_variants() {
        for err in [
            BrokerError::AuthRequired("expired".to_owned()),
            BrokerError::RateLimited,
            BrokerError::AmbiguousTimeout {
                stage: "PlaceOrder",
            },
            BrokerError::Rejected {
                stage: "PlaceOrder",
                category: "rms",
            },
            BrokerError::Transport("net".to_owned()),
            BrokerError::InvalidResponse("bad".to_owned()),
        ] {
            assert!(!err.to_string().is_empty());
        }
        for (status, active) in [
            (OrderStatus::Pending, true),
            (OrderStatus::Open, true),
            (OrderStatus::TriggerPending, true),
            (OrderStatus::Unknown, true),
            (OrderStatus::Complete, false),
            (OrderStatus::Canceled, false),
            (OrderStatus::Rejected, false),
        ] {
            assert_eq!(status.is_active(), active);
            assert!(!status.as_str().is_empty());
        }
    }
}
