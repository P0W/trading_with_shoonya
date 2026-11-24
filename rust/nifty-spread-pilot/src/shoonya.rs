//! Shoonya (Finvasia) 2026 OAuth + `NorenWClientAPI` implementation of [`Broker`].

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use time::{Date, OffsetDateTime, Time};
use zeroize::Zeroizing;

use crate::broker::{
    AccountLimits, Broker, BrokerError, BrokerOrder, BrokerPosition, ExpiryChainContracts,
    FeeSchedule, MarketQuote, OptionContract, OrderPlacement, OrderPriceType, OrderRequest,
    OrderSide, OrderStatus, ProductType, Retention,
};
use crate::connection;
use crate::market::{self, ParsedMaster};
use crate::oauth::{self, AuthenticatedSession};

const DEFAULT_API_BASE: &str = "https://api.shoonya.com/NorenWClientAPI";
const MAX_API_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ShoonyaEndpoints {
    pub api_base: String,
    pub nfo_master_url: String,
    pub spot_token: String,
    pub quote_pace: Duration,
    pub rate_limit_backoff: Duration,
}

impl Default for ShoonyaEndpoints {
    fn default() -> Self {
        Self {
            api_base: DEFAULT_API_BASE.to_owned(),
            nfo_master_url: market::DEFAULT_MASTER_URL.to_owned(),
            spot_token: "26000".to_owned(),
            quote_pace: Duration::from_millis(25),
            rate_limit_backoff: Duration::from_millis(250),
        }
    }
}

struct CachedMaster {
    underlying: String,
    date_ist: Date,
    after_bod_refresh: bool,
    before_cutoff: bool,
    parsed: ParsedMaster,
}

type LoginHook = Box<dyn FnMut() -> Result<AuthenticatedSession, String>>;

pub struct ShoonyaBroker {
    agent: ureq::Agent,
    endpoints: ShoonyaEndpoints,
    account_id_override: Option<String>,
    user_id: Option<String>,
    access_token: Option<Zeroizing<String>>,
    login_hook: Option<LoginHook>,
    cached_master: Option<CachedMaster>,
}

impl ShoonyaBroker {
    pub fn from_config_path(
        config_path: PathBuf,
        account_id_override: Option<String>,
        spot_token: String,
    ) -> Self {
        let mut endpoints = ShoonyaEndpoints::default();
        if !spot_token.is_empty() {
            endpoints.spot_token = spot_token;
        }
        Self {
            agent: connection::agent(),
            endpoints,
            account_id_override,
            user_id: None,
            access_token: None,
            login_hook: Some(Box::new(move || oauth::login_session(&config_path))),
            cached_master: None,
        }
    }

    pub fn with_custom_transport(
        agent: ureq::Agent,
        endpoints: ShoonyaEndpoints,
        initial_session: Option<AuthenticatedSession>,
        account_id_override: Option<String>,
        login_hook: Option<LoginHook>,
    ) -> Self {
        let (user_id, access_token) = match initial_session {
            Some(s) => (Some(s.user_id), Some(s.access_token)),
            None => (None, None),
        };
        Self {
            agent,
            endpoints,
            account_id_override,
            user_id,
            access_token,
            login_hook,
            cached_master: None,
        }
    }

    fn credentials(&mut self) -> Result<(String, String, Zeroizing<String>), BrokerError> {
        self.ensure_session()?;
        let uid = self
            .user_id
            .clone()
            .ok_or_else(|| BrokerError::AuthRequired("Missing Shoonya user ID.".to_owned()))?;
        let actid = self
            .account_id_override
            .clone()
            .unwrap_or_else(|| uid.clone());
        let token = self
            .access_token
            .clone()
            .ok_or_else(|| BrokerError::AuthRequired("Missing Shoonya access token.".to_owned()))?;
        Ok((uid, actid, token))
    }

    fn refresh_session(&mut self) -> Result<(), BrokerError> {
        self.access_token = None;
        let hook = self.login_hook.as_mut().ok_or_else(|| {
            BrokerError::AuthRequired(
                "Shoonya session expired and relogin is unavailable.".to_owned(),
            )
        })?;
        let session = hook().map_err(BrokerError::AuthRequired)?;
        self.user_id = Some(session.user_id);
        self.access_token = Some(session.access_token);
        Ok(())
    }

    fn post_api(
        &mut self,
        stage: &'static str,
        build_payload: impl Fn(&str, &str) -> Value,
        idempotent: bool,
    ) -> Result<Value, BrokerError> {
        let mut relogin_attempted = false;
        let mut rate_retries = 0_u8;

        loop {
            let (uid, actid, token) = self.credentials()?;
            let url = format!("{}/{stage}", self.endpoints.api_base.trim_end_matches('/'));
            let payload = build_payload(&uid, &actid);
            let body = format!("jData={payload}");
            let auth_header = Zeroizing::new(format!("Bearer {}", token.as_str()));

            let mut response = match self
                .agent
                .post(&url)
                .header("Content-Type", "text/plain")
                .header("Authorization", auth_header.as_str())
                .send(&body)
            {
                Ok(resp) => resp,
                Err(ureq::Error::StatusCode(401)) => {
                    if !relogin_attempted && self.login_hook.is_some() {
                        relogin_attempted = true;
                        self.refresh_session()?;
                        if idempotent {
                            continue;
                        }
                        return Err(BrokerError::AmbiguousTimeout { stage });
                    }
                    return Err(BrokerError::AuthRequired(format!(
                        "Shoonya {stage} returned HTTP 401."
                    )));
                }
                Err(ureq::Error::StatusCode(403)) => {
                    if idempotent && !relogin_attempted && self.login_hook.is_some() {
                        relogin_attempted = true;
                        self.refresh_session()?;
                        continue;
                    }
                    return Err(BrokerError::AuthRequired(format!(
                        "Shoonya {stage} returned HTTP 403."
                    )));
                }
                Err(ureq::Error::StatusCode(429)) => {
                    if idempotent && rate_retries < 2 {
                        rate_retries += 1;
                        if !self.endpoints.rate_limit_backoff.is_zero() {
                            std::thread::sleep(self.endpoints.rate_limit_backoff);
                        }
                        continue;
                    }
                    return Err(BrokerError::RateLimited);
                }
                Err(ureq::Error::StatusCode(code)) => {
                    return Err(BrokerError::Transport(format!(
                        "Shoonya {stage} returned HTTP {code}."
                    )));
                }
                Err(_) => {
                    if !idempotent {
                        return Err(BrokerError::AmbiguousTimeout { stage });
                    }
                    return Err(BrokerError::Transport(format!(
                        "Shoonya {stage} failed (network, TLS, or timeout)."
                    )));
                }
            };

            let status = response.status().as_u16();
            if status == 401 {
                if !relogin_attempted && self.login_hook.is_some() {
                    relogin_attempted = true;
                    self.refresh_session()?;
                    if idempotent {
                        continue;
                    }
                    return Err(BrokerError::AmbiguousTimeout { stage });
                }
                return Err(BrokerError::AuthRequired(format!(
                    "Shoonya {stage} returned HTTP 401."
                )));
            }
            if status == 403 {
                if idempotent && !relogin_attempted && self.login_hook.is_some() {
                    relogin_attempted = true;
                    self.refresh_session()?;
                    continue;
                }
                return Err(BrokerError::AuthRequired(format!(
                    "Shoonya {stage} returned HTTP 403."
                )));
            }
            if status == 429 {
                if idempotent && rate_retries < 2 {
                    rate_retries += 1;
                    if !self.endpoints.rate_limit_backoff.is_zero() {
                        std::thread::sleep(self.endpoints.rate_limit_backoff);
                    }
                    continue;
                }
                return Err(BrokerError::RateLimited);
            }
            if !response.status().is_success() {
                return Err(BrokerError::Transport(format!(
                    "Shoonya {stage} returned HTTP {status}."
                )));
            }

            let raw = response
                .body_mut()
                .with_config()
                .limit(MAX_API_RESPONSE_BYTES)
                .read_to_string()
                .map_err(|_| {
                    if !idempotent {
                        BrokerError::AmbiguousTimeout { stage }
                    } else {
                        BrokerError::InvalidResponse(format!(
                            "Cannot read Shoonya {stage} response."
                        ))
                    }
                })?;
            let value: Value = serde_json::from_str(&raw).map_err(|_| {
                BrokerError::InvalidResponse(format!("Invalid Shoonya {stage} JSON response."))
            })?;

            if let Some(emsg) = not_ok_emsg(&value) {
                if is_session_expired(&emsg) {
                    if !relogin_attempted && self.login_hook.is_some() {
                        relogin_attempted = true;
                        self.refresh_session()?;
                        if idempotent {
                            continue;
                        }
                        return Err(BrokerError::AmbiguousTimeout { stage });
                    }
                    return Err(BrokerError::AuthRequired(format!(
                        "Shoonya {stage} session expired."
                    )));
                }
                if is_rate_limited(&emsg) {
                    if idempotent && rate_retries < 2 {
                        rate_retries += 1;
                        if !self.endpoints.rate_limit_backoff.is_zero() {
                            std::thread::sleep(self.endpoints.rate_limit_backoff);
                        }
                        continue;
                    }
                    return Err(BrokerError::RateLimited);
                }
            }

            return Ok(value);
        }
    }

    fn fetch_quote(&mut self, exchange: &str, token: &str) -> Result<MarketQuote, BrokerError> {
        let exch = exchange.to_owned();
        let tok = token.to_owned();
        let value = self.post_api(
            "GetQuotes",
            |uid, _| json!({ "uid": uid, "exch": exch, "token": tok }),
            true,
        )?;
        if value.get("stat").and_then(Value::as_str) != Some("Ok") {
            return Err(BrokerError::Rejected {
                stage: "GetQuotes",
                category: classify_rejection(not_ok_emsg(&value).as_deref().unwrap_or("")),
            });
        }
        if value.get("exch").and_then(Value::as_str) != Some(exchange)
            || value.get("token").and_then(Value::as_str) != Some(token)
        {
            let field_status = |field: &str, expected: &str| match value.get(field) {
                None | Some(Value::Null) => "missing",
                Some(Value::String(actual)) if actual == expected => "matches",
                Some(Value::String(_)) => "different",
                Some(_) => "not a string",
            };
            return Err(BrokerError::InvalidResponse(format!(
                "Shoonya GetQuotes response did not match requested instrument (exchange: {}; token: {}).",
                field_status("exch", exchange),
                field_status("token", token)
            )));
        }
        parse_market_quote(&value)
    }

    fn load_master(
        &mut self,
        underlying: &str,
        now_ist: OffsetDateTime,
    ) -> Result<&ParsedMaster, BrokerError> {
        let bod_refresh = Time::from_hms(9, 0, 0).unwrap();
        let cutoff = Time::from_hms(15, 30, 0).unwrap();
        let after_bod_refresh = now_ist.time() >= bod_refresh;
        let before_cutoff = now_ist.time() < cutoff;
        let fresh = self.cached_master.as_ref().is_some_and(|c| {
            c.underlying == underlying
                && c.date_ist == now_ist.date()
                && c.after_bod_refresh == after_bod_refresh
                && c.before_cutoff == before_cutoff
        });
        if !fresh {
            let parsed = market::download_and_parse_master(
                &self.agent,
                &self.endpoints.nfo_master_url,
                underlying,
                now_ist,
            )
            .map_err(BrokerError::InvalidResponse)?;
            self.cached_master = Some(CachedMaster {
                underlying: underlying.to_owned(),
                date_ist: now_ist.date(),
                after_bod_refresh,
                before_cutoff,
                parsed,
            });
        }
        Ok(&self.cached_master.as_ref().unwrap().parsed)
    }
}

impl Broker for ShoonyaBroker {
    fn name(&self) -> &'static str {
        "shoonya"
    }

    fn fee_schedule(&self) -> FeeSchedule {
        FeeSchedule::shoonya_2026()
    }

    fn ensure_session(&mut self) -> Result<(), BrokerError> {
        if self.access_token.is_some() && self.user_id.is_some() {
            return Ok(());
        }
        self.refresh_session()
    }

    fn discover_option_chain_contracts(
        &mut self,
        underlying: &str,
        now_ist: OffsetDateTime,
    ) -> Result<ExpiryChainContracts, BrokerError> {
        Ok(self.load_master(underlying, now_ist)?.nearest.clone())
    }

    fn resolve_contracts_by_symbols(
        &mut self,
        underlying: &str,
        symbols: &[String],
        now_ist: OffsetDateTime,
    ) -> Result<Vec<OptionContract>, BrokerError> {
        let all = &self.load_master(underlying, now_ist)?.all_active_or_today;
        Ok(all
            .iter()
            .filter(|c| symbols.iter().any(|s| s == &c.trading_symbol))
            .cloned()
            .collect())
    }

    fn get_spot_quote(&mut self, underlying: &str) -> Result<MarketQuote, BrokerError> {
        if underlying != "NIFTY" {
            return Err(BrokerError::Rejected {
                stage: "GetQuotes",
                category: "unsupported spot underlying",
            });
        }
        let spot_token = self.endpoints.spot_token.clone();
        self.fetch_quote("NSE", &spot_token)
    }

    fn get_option_quotes(
        &mut self,
        contracts: &[OptionContract],
    ) -> Result<Vec<(OptionContract, MarketQuote)>, BrokerError> {
        let mut results = Vec::with_capacity(contracts.len());
        let pace = self.endpoints.quote_pace;
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        for (idx, contract) in contracts.iter().enumerate() {
            if std::time::Instant::now() > deadline {
                return Err(BrokerError::Transport(
                    "Shoonya GetQuotes batch exceeded 30s deadline.".to_owned(),
                ));
            }
            if idx > 0 && !pace.is_zero() {
                std::thread::sleep(pace);
            }
            let quote = self.fetch_quote(&contract.exchange, &contract.token)?;
            results.push((contract.clone(), quote));
        }
        Ok(results)
    }

    fn get_account_limits(&mut self) -> Result<AccountLimits, BrokerError> {
        let value = self.post_api(
            "Limits",
            |uid, actid| json!({ "uid": uid, "actid": actid }),
            true,
        )?;
        if value.get("stat").and_then(Value::as_str) != Some("Ok") {
            return Err(BrokerError::Rejected {
                stage: "Limits",
                category: classify_rejection(not_ok_emsg(&value).as_deref().unwrap_or("")),
            });
        }
        let field = |name: &str| -> Result<i64, BrokerError> {
            match value.get(name).and_then(Value::as_str) {
                None | Some("") => Ok(0),
                Some(raw) => parse_signed_paise(raw).ok_or_else(|| {
                    BrokerError::InvalidResponse(format!(
                        "Shoonya Limits contained invalid numeric field {name}."
                    ))
                }),
            }
        };
        // Noren `brkcollamt` and `collateral` overlap on pledged hair-cut margin; take `max`
        // rather than summing so available margin is never overstated 2x!
        let collateral = field("collateral")?.max(field("brkcollamt")?).max(0);
        let payout_paise = field("payout")?.checked_abs().ok_or_else(|| {
            BrokerError::InvalidResponse("Shoonya Limits payout overflow.".to_owned())
        })?;
        Ok(AccountLimits {
            cash_paise: field("cash")?,
            collateral_paise: collateral,
            payin_paise: field("payin")?,
            payout_paise,
            margin_used_paise: field("marginused")?,
        })
    }

    fn get_positions(&mut self) -> Result<Vec<BrokerPosition>, BrokerError> {
        let value = self.post_api(
            "PositionBook",
            |uid, actid| json!({ "uid": uid, "actid": actid }),
            true,
        )?;
        if is_no_data(&value) {
            return Ok(Vec::new());
        }
        if let Some(emsg) = not_ok_emsg(&value) {
            return Err(BrokerError::Rejected {
                stage: "PositionBook",
                category: classify_rejection(&emsg),
            });
        }
        let rows = value.as_array().ok_or_else(|| {
            BrokerError::InvalidResponse("Shoonya PositionBook was not a JSON array.".to_owned())
        })?;
        let mut positions = Vec::with_capacity(rows.len());
        for row in rows {
            if row.get("stat").and_then(Value::as_str) != Some("Ok") {
                return Err(BrokerError::InvalidResponse(
                    "Shoonya PositionBook entry had non-Ok status.".to_owned(),
                ));
            }
            let exchange = required_str(row, "exch", "PositionBook")?.to_owned();
            let token = required_str(row, "token", "PositionBook")?.to_owned();
            let trading_symbol = required_str(row, "tsym", "PositionBook")?.to_owned();
            let product = parse_product(required_str(row, "prd", "PositionBook")?);
            let net_qty = required_str(row, "netqty", "PositionBook")?
                .parse::<i32>()
                .map_err(|_| {
                    BrokerError::InvalidResponse(
                        "Shoonya PositionBook entry had invalid netqty.".to_owned(),
                    )
                })?;

            let pos_paise = |keys: &[&str]| -> Option<i64> {
                keys.iter().find_map(|k| {
                    row.get(*k)
                        .and_then(Value::as_str)
                        .and_then(parse_signed_paise)
                        .filter(|&v| v > 0)
                })
            };
            let buy_avg_paise = if net_qty > 0 {
                pos_paise(&[
                    "cforgavgprc",
                    "netavgprc",
                    "totbuyavgprc",
                    "daybuyavgprc",
                    "cfbuyavgprc",
                    "netupldprc",
                    "upldprc",
                ])
            } else {
                pos_paise(&["totbuyavgprc", "daybuyavgprc"])
            };
            let sell_avg_paise = if net_qty < 0 {
                pos_paise(&[
                    "cforgavgprc",
                    "netavgprc",
                    "totsellavgprc",
                    "daysellavgprc",
                    "cfsellavgprc",
                    "netupldprc",
                    "upldprc",
                ])
            } else {
                pos_paise(&["totsellavgprc", "daysellavgprc"])
            };
            let ltp_paise = pos_paise(&["lp"]);
            let realized_pnl_paise = row
                .get("rpnl")
                .and_then(Value::as_str)
                .and_then(parse_signed_paise)
                .unwrap_or(0);
            let unrealized_pnl_paise = row
                .get("urmtom")
                .and_then(Value::as_str)
                .and_then(parse_signed_paise)
                .unwrap_or(0);

            positions.push(BrokerPosition {
                exchange,
                token,
                trading_symbol,
                product,
                net_qty,
                buy_avg_paise,
                sell_avg_paise,
                ltp_paise,
                realized_pnl_paise,
                unrealized_pnl_paise,
            });
        }
        Ok(positions)
    }

    fn get_orders(&mut self) -> Result<Vec<BrokerOrder>, BrokerError> {
        let value = self.post_api("OrderBook", |uid, _| json!({ "uid": uid }), true)?;
        if is_no_data(&value) {
            return Ok(Vec::new());
        }
        if let Some(emsg) = not_ok_emsg(&value) {
            return Err(BrokerError::Rejected {
                stage: "OrderBook",
                category: classify_rejection(&emsg),
            });
        }
        let rows = value.as_array().ok_or_else(|| {
            BrokerError::InvalidResponse("Shoonya OrderBook was not a JSON array.".to_owned())
        })?;
        let mut orders = Vec::with_capacity(rows.len());
        for row in rows {
            if row.get("stat").and_then(Value::as_str) != Some("Ok") {
                return Err(BrokerError::InvalidResponse(
                    "Shoonya OrderBook entry had non-Ok status.".to_owned(),
                ));
            }
            let order_id = required_str(row, "norenordno", "OrderBook")?.to_owned();
            let exchange = required_str(row, "exch", "OrderBook")?.to_owned();
            let trading_symbol = required_str(row, "tsym", "OrderBook")?.to_owned();
            let side = match required_str(row, "trantype", "OrderBook")? {
                "B" => OrderSide::Buy,
                "S" => OrderSide::Sell,
                _ => {
                    return Err(BrokerError::InvalidResponse(
                        "Shoonya OrderBook had unknown trantype.".to_owned(),
                    ));
                }
            };
            let product = parse_product(row.get("prd").and_then(Value::as_str).unwrap_or(""));
            let status = parse_order_status(required_str(row, "status", "OrderBook")?);
            let requested_qty = required_str(row, "qty", "OrderBook")?
                .parse::<u32>()
                .map_err(|_| {
                    BrokerError::InvalidResponse("Shoonya OrderBook had invalid qty.".to_owned())
                })?;
            let filled_qty = match row.get("fillshares").and_then(Value::as_str) {
                None | Some("") => 0,
                Some(v) => v.parse::<u32>().map_err(|_| {
                    BrokerError::InvalidResponse(
                        "Shoonya OrderBook had invalid fillshares.".to_owned(),
                    )
                })?,
            };
            let pos_field = |name: &str| -> Option<i64> {
                row.get(name)
                    .and_then(Value::as_str)
                    .and_then(parse_signed_paise)
                    .filter(|&p| p > 0)
            };
            let client_tag = row
                .get("remarks")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);

            orders.push(BrokerOrder {
                order_id,
                exchange,
                trading_symbol,
                side,
                product,
                status,
                requested_qty,
                filled_qty,
                avg_fill_price_paise: pos_field("avgprc"),
                limit_price_paise: pos_field("prc"),
                trigger_price_paise: pos_field("trgprc"),
                client_tag,
            });
        }
        Ok(orders)
    }

    fn place_order(&mut self, request: &OrderRequest) -> Result<OrderPlacement, BrokerError> {
        if request.quantity == 0 {
            return Err(BrokerError::Rejected {
                stage: "PlaceOrder",
                category: "invalid quantity",
            });
        }
        if request.exchange.is_empty()
            || request.exchange.len() > 16
            || !request.exchange.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(BrokerError::Rejected {
                stage: "PlaceOrder",
                category: "invalid exchange",
            });
        }
        if request.trading_symbol.is_empty()
            || request.trading_symbol.len() > 100
            || !request
                .trading_symbol
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'&' | b'.'))
        {
            return Err(BrokerError::Rejected {
                stage: "PlaceOrder",
                category: "invalid trading symbol",
            });
        }
        if request.client_tag.is_empty()
            || request.client_tag.len() > 32
            || !request
                .client_tag
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(BrokerError::Rejected {
                stage: "PlaceOrder",
                category: "invalid client tag",
            });
        }
        let prd = match request.product {
            ProductType::Margin => "M",
            ProductType::Intraday => "I",
            ProductType::Other => {
                return Err(BrokerError::Rejected {
                    stage: "PlaceOrder",
                    category: "unsupported product type",
                });
            }
        };
        let trantype = match request.side {
            OrderSide::Buy => "B",
            OrderSide::Sell => "S",
        };
        let ret = match request.retention {
            Retention::Day => "DAY",
            Retention::Ioc => "IOC",
        };
        let (prctyp, prc_str, trg_str) = match request.price_type {
            OrderPriceType::Limit { price_paise } => {
                let prc = format_positive_paise(price_paise).ok_or(BrokerError::Rejected {
                    stage: "PlaceOrder",
                    category: "invalid limit price",
                })?;
                ("LMT", prc, None)
            }
            OrderPriceType::StopLimit {
                trigger_paise,
                limit_paise,
            } => {
                let trg = format_positive_paise(trigger_paise).ok_or(BrokerError::Rejected {
                    stage: "PlaceOrder",
                    category: "invalid trigger price",
                })?;
                let prc = format_positive_paise(limit_paise).ok_or(BrokerError::Rejected {
                    stage: "PlaceOrder",
                    category: "invalid limit price",
                })?;
                ("SL-LMT", prc, Some(trg))
            }
        };

        let qty_str = request.quantity.to_string();
        let exch = request.exchange.clone();
        // Noren `jData=` parser requires `%`, `&`, `+` in `tsym` (e.g. `M&M`) to be percent-encoded.
        let tsym = encode_noren_tsym(&request.trading_symbol);
        let remarks = request.client_tag.clone();

        let value = self.post_api(
            "PlaceOrder",
            |uid, actid| {
                let mut obj = json!({
                    "uid": uid,
                    "actid": actid,
                    "exch": exch,
                    "tsym": tsym,
                    "qty": qty_str,
                    "prc": prc_str,
                    "dscqty": "0",
                    "prd": prd,
                    "trantype": trantype,
                    "prctyp": prctyp,
                    "ret": ret,
                    "remarks": remarks,
                    "ordersource": "API",
                });
                if let Some(ref trg) = trg_str {
                    obj["trgprc"] = json!(trg);
                }
                obj
            },
            false,
        )?;

        if value.get("stat").and_then(Value::as_str) != Some("Ok") {
            return Err(BrokerError::Rejected {
                stage: "PlaceOrder",
                category: classify_rejection(not_ok_emsg(&value).as_deref().unwrap_or("")),
            });
        }
        let order_id = value
            .get("norenordno")
            .and_then(Value::as_str)
            .filter(|id| {
                !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            .ok_or_else(|| {
                BrokerError::InvalidResponse(
                    "Shoonya PlaceOrder did not return a valid norenordno.".to_owned(),
                )
            })?;
        Ok(OrderPlacement {
            order_id: order_id.to_owned(),
        })
    }

    fn cancel_order(&mut self, order_id: &str) -> Result<(), BrokerError> {
        if order_id.is_empty()
            || order_id.len() > 64
            || !order_id.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(BrokerError::Rejected {
                stage: "CancelOrder",
                category: "invalid order id",
            });
        }
        let oid = order_id.to_owned();
        let value = self.post_api(
            "CancelOrder",
            |uid, _| {
                json!({
                    "uid": uid,
                    "norenordno": oid,
                    "ordersource": "API",
                })
            },
            true,
        )?;
        if value.get("stat").and_then(Value::as_str) == Some("Ok") {
            return Ok(());
        }
        let emsg = not_ok_emsg(&value).unwrap_or_default().to_ascii_lowercase();
        if emsg.contains("already cancel")
            || emsg.contains("already complete")
            || emsg.contains("not open")
        {
            return Ok(());
        }
        Err(BrokerError::Rejected {
            stage: "CancelOrder",
            category: classify_rejection(&emsg),
        })
    }
}

fn parse_market_quote(value: &Value) -> Result<MarketQuote, BrokerError> {
    let opt_pos_paise = |name: &str| -> Result<Option<i64>, BrokerError> {
        match value.get(name).and_then(Value::as_str) {
            None | Some("") => Ok(None),
            Some(raw) => {
                let parsed = parse_signed_paise(raw).ok_or_else(|| {
                    BrokerError::InvalidResponse(format!("Invalid Shoonya quote field {name}."))
                })?;
                Ok((parsed > 0).then_some(parsed))
            }
        }
    };
    let opt_u64 = |name: &str| -> Option<u64> {
        value.get(name).and_then(Value::as_str)?.parse::<u64>().ok()
    };
    let opt_u32 = |name: &str| -> u32 {
        value
            .get(name)
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0)
    };

    Ok(MarketQuote {
        ltp_paise: opt_pos_paise("lp")?,
        prev_close_paise: opt_pos_paise("c")?,
        bid_paise: opt_pos_paise("bp1")?,
        ask_paise: opt_pos_paise("sp1")?,
        bid_qty: opt_u32("bq1"),
        ask_qty: opt_u32("sq1"),
        open_interest: opt_u64("oi"),
        prev_open_interest: opt_u64("poi"),
        volume: opt_u64("v"),
        lower_circuit_paise: opt_pos_paise("lc")?,
        upper_circuit_paise: opt_pos_paise("uc")?,
        feed_epoch_secs: opt_u64("ft"),
    })
}

fn required_str<'a>(
    row: &'a Value,
    field: &'static str,
    stage: &'static str,
) -> Result<&'a str, BrokerError> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            BrokerError::InvalidResponse(format!("Shoonya {stage} entry missing {field}."))
        })
}

fn parse_product(raw: &str) -> ProductType {
    match raw.trim() {
        "M" => ProductType::Margin,
        "I" => ProductType::Intraday,
        _ => ProductType::Other,
    }
}

fn encode_noren_tsym(tsym: &str) -> String {
    let mut out = String::with_capacity(tsym.len());
    for ch in tsym.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '&' => out.push_str("%26"),
            '+' => out.push_str("%2B"),
            _ => out.push(ch),
        }
    }
    out
}

fn parse_order_status(raw: &str) -> OrderStatus {
    match raw.trim().to_ascii_uppercase().as_str() {
        "COMPLETE" => OrderStatus::Complete,
        "OPEN" => OrderStatus::Open,
        "TRIGGER_PENDING" | "TRIGGER PENDING" => OrderStatus::TriggerPending,
        "CANCELED" | "CANCELLED" | "EXPIRED" => OrderStatus::Canceled,
        "REJECTED" | "REJECT" => OrderStatus::Rejected,
        "PENDING"
        | "OPEN_PENDING"
        | "OPEN PENDING"
        | "VALIDATION_PENDING"
        | "VALIDATION PENDING"
        | "PUT_ORDER_REQ_RECEIVED"
        | "PUT ORDER REQ RECEIVED"
        | "MODIFY_PENDING"
        | "MODIFY PENDING"
        | "CANCEL_PENDING"
        | "CANCEL PENDING"
        | "AFTER MARKET ORDER REQ RECEIVED" => OrderStatus::Pending,
        _ => OrderStatus::Unknown,
    }
}

fn not_ok_emsg(value: &Value) -> Option<String> {
    if value.get("stat").and_then(Value::as_str) == Some("Not_Ok") {
        Some(
            value
                .get("emsg")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        )
    } else {
        None
    }
}

fn is_no_data(value: &Value) -> bool {
    not_ok_emsg(value).is_some_and(|emsg| {
        let lower = emsg.trim().to_ascii_lowercase();
        lower == "no data" || lower.starts_with("no data") || lower.contains(": no data")
    })
}

fn is_session_expired(emsg: &str) -> bool {
    let lower = emsg.to_ascii_lowercase();
    lower.contains("session expired")
        || lower.contains("invalid session")
        || lower.contains("session_expired")
}

fn is_rate_limited(emsg: &str) -> bool {
    let lower = emsg.to_ascii_lowercase();
    lower.contains("rate_limited")
        || lower.contains("too many requests")
        || lower.contains("request limit")
}

fn classify_rejection(emsg: &str) -> &'static str {
    let lower = emsg.to_ascii_lowercase();
    if lower.contains("margin") || lower.contains("insufficient") || lower.contains("shortfall") {
        "insufficient margin"
    } else if lower.contains("circuit") || lower.contains("freeze") || lower.contains("price band")
    {
        "circuit or freeze band"
    } else if lower.contains("rms") {
        "rms rejection"
    } else if lower.contains("invalid") {
        "invalid input"
    } else {
        "broker rejected request"
    }
}

pub fn format_positive_paise(paise: i64) -> Option<String> {
    if paise <= 0 {
        return None;
    }
    Some(format!("{}.{:02}", paise / 100, paise % 100))
}

/// Exact integer parser for signed/unsigned decimal rupee strings (up to 6 decimal places,
/// half-up rounded to integer paise).
pub fn parse_signed_paise(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (negative, rest) = if let Some(stripped) = trimmed.strip_prefix('-') {
        (true, stripped)
    } else if let Some(stripped) = trimmed.strip_prefix('+') {
        (false, stripped)
    } else {
        (false, trimmed)
    };
    let (whole, frac) = rest.split_once('.').unwrap_or((rest, ""));
    if whole.is_empty()
        || whole.len() > 14
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || frac.len() > 6
        || !frac.bytes().all(|b| b.is_ascii_digit())
        || (rest.contains('.') && frac.is_empty())
    {
        return None;
    }
    let whole_paise = whole.parse::<i64>().ok()?.checked_mul(100)?;
    let mut padded = [b'0'; 6];
    for (idx, b) in frac.bytes().enumerate() {
        padded[idx] = b;
    }
    let micro_rupees = std::str::from_utf8(&padded).ok()?.parse::<i64>().ok()?;
    // 1 paise = 10_000 micro-rupees; half-up round.
    let frac_paise = (micro_rupees + 5_000) / 10_000;
    let abs_paise = whole_paise.checked_add(frac_paise)?;
    if negative {
        abs_paise.checked_neg()
    } else {
        Some(abs_paise)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use time::Month;

    fn mock_http_sequence(
        responses: Vec<(u16, Vec<u8>)>,
    ) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0_u8; 4096];
                let (header_end, content_len) = loop {
                    let n = stream.read(&mut chunk).unwrap();
                    assert_ne!(n, 0);
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
                        let len = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        break (pos + 4, len);
                    }
                };
                while buf.len() < header_end + content_len {
                    let n = stream.read(&mut chunk).unwrap();
                    assert_ne!(n, 0);
                    buf.extend_from_slice(&chunk[..n]);
                }
                seen.push(String::from_utf8_lossy(&buf).into_owned());
                write!(
                    stream,
                    "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
            seen
        });
        (base, handle)
    }

    fn test_agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_millis(400)))
            .build()
            .into()
    }

    fn make_zip_master() -> Vec<u8> {
        let csv = "Exchange,Token,LotSize,Symbol,TradingSymbol,Expiry,Instrument,OptionType,StrikePrice,TickSize,\n\
                   NFO,101,65,NIFTY,NIFTY01DEC26C25500,01-DEC-2026,OPTIDX,CE,25500,0.05,\n\
                   NFO,102,65,NIFTY,NIFTY01DEC26P25500,01-DEC-2026,OPTIDX,PE,25500,0.05,\n\
                   NFO,201,65,NIFTY,NIFTY29DEC26C26000,29-DEC-2026,OPTIDX,CE,26000,0.05,\n\
                   NFO,202,65,NIFTY,NIFTY29DEC26P26000,29-DEC-2026,OPTIDX,PE,26000,0.05,\n";
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("NFO_symbols.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(csv.as_bytes()).unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn quote_identity_errors_describe_field_shape_without_echoing_values() {
        let responses = [
            r#"{"stat":"Ok","token":"101"}"#,
            r#"{"stat":"Ok","exch":"NFO","token":101}"#,
            r#"{"stat":"Ok","exch":"private-value","token":"private-value"}"#,
        ];
        let (base, server) = mock_http_sequence(
            responses
                .iter()
                .map(|body| (200, body.as_bytes().to_vec()))
                .collect(),
        );
        let mut broker = ShoonyaBroker::with_custom_transport(
            test_agent(),
            ShoonyaEndpoints {
                api_base: base,
                ..ShoonyaEndpoints::default()
            },
            Some(AuthenticatedSession {
                user_id: "U1".to_owned(),
                access_token: Zeroizing::new("token".to_owned()),
            }),
            None,
            None,
        );
        for expected in [
            "exchange: missing; token: matches",
            "exchange: matches; token: not a string",
            "exchange: different; token: different",
        ] {
            let err = broker.fetch_quote("NFO", "101").unwrap_err().to_string();
            assert!(err.contains(expected), "{err}");
            assert!(!err.contains("private-value"));
        }
        server.join().unwrap();
    }

    #[test]
    fn decimal_parsers_and_formatters_handle_4dp_and_negatives() {
        assert_eq!(parse_signed_paise("123.45"), Some(12_345));
        assert_eq!(parse_signed_paise("145.3250"), Some(14_533));
        assert_eq!(parse_signed_paise("-1250.5"), Some(-125_050));
        assert_eq!(parse_signed_paise("+10"), Some(1_000));
        assert_eq!(parse_signed_paise("0.00"), Some(0));
        assert_eq!(parse_signed_paise(""), None);
        assert_eq!(parse_signed_paise("12."), None);
        assert_eq!(parse_signed_paise("abc"), None);
        assert_eq!(format_positive_paise(6_505), Some("65.05".to_owned()));
        assert_eq!(format_positive_paise(0), None);
    }

    #[test]
    fn shoonya_broker_handles_relogin_rate_limits_books_and_orders() {
        let zip_bytes = make_zip_master();
        let responses = vec![
            // 1. NFO master ZIP download (cached across subsequent calls)
            (200, zip_bytes),
            // 2. Spot quote fails with Session Expired -> triggers relogin hook -> retries & succeeds
            (
                200,
                br#"{"stat":"Not_Ok","emsg":"Session Expired : Invalid Session Key"}"#.to_vec(),
            ),
            (
                200,
                br#"{"stat":"Ok","exch":"NSE","token":"26000","lp":"25349.65","c":"25200.00"}"#
                    .to_vec(),
            ),
            // 3. Option quote hits Rate_Limited once -> backs off -> succeeds on retry
            (
                200,
                br#"{"stat":"Not_Ok","emsg":"Rate_Limited: Exceeded per-second limit"}"#.to_vec(),
            ),
            (
                200,
                br#"{"stat":"Ok","exch":"NFO","token":"101","lp":"110.25","bp1":"110.00","sp1":"110.50","bq1":"130","sq1":"65","oi":"1500000","lc":"0.05","uc":"450.00"}"#.to_vec(),
            ),
            // 4. Limits
            (
                200,
                br#"{"stat":"Ok","cash":"150000.50","collateral":"50000.00","brkcollamt":"10000.00","marginused":"25000.25"}"#.to_vec(),
            ),
            // 5. Empty PositionBook ("no data")
            (
                200,
                br#"{"stat":"Not_Ok","emsg":"Error : no data"}"#.to_vec(),
            ),
            // 6. Populated PositionBook (with 4-decimal totbuyavgprc)
            (
                200,
                br#"[{"stat":"Ok","exch":"NFO","tsym":"NIFTY01DEC26C25500","token":"101","prd":"M","netqty":"-65","totsellavgprc":"110.1250","lp":"95.00","rpnl":"0.00","urmtom":"983.13"}]"#.to_vec(),
            ),
            // 7. OrderBook
            (
                200,
                br#"[{"stat":"Ok","norenordno":"2604170001","exch":"NFO","tsym":"NIFTY01DEC26C25500","trantype":"S","prd":"M","status":"COMPLETE","qty":"65","fillshares":"65","avgprc":"110.1250","prc":"110.00","remarks":"nsp-1"}]"#.to_vec(),
            ),
            // 8. PlaceOrder (SL-LMT)
            (200, br#"{"stat":"Ok","norenordno":"2604170002"}"#.to_vec()),
            // 9. CancelOrder ("already cancelled" treated as non-fatal Ok)
            (
                200,
                br#"{"stat":"Not_Ok","emsg":"Order Already Cancelled"}"#.to_vec(),
            ),
        ];

        let (base, server) = mock_http_sequence(responses);
        let relogin_count = Arc::new(AtomicUsize::new(0));
        let rc_clone = Arc::clone(&relogin_count);

        let endpoints = ShoonyaEndpoints {
            api_base: base.clone(),
            nfo_master_url: format!("{base}/NFO_symbols.txt.zip"),
            spot_token: "26000".to_owned(),
            quote_pace: Duration::ZERO,
            rate_limit_backoff: Duration::ZERO,
        };
        let initial = AuthenticatedSession {
            user_id: "USER1".to_owned(),
            access_token: Zeroizing::new("expired-token".to_owned()),
        };
        let mut broker = ShoonyaBroker::with_custom_transport(
            test_agent(),
            endpoints,
            Some(initial),
            None,
            Some(Box::new(move || {
                rc_clone.fetch_add(1, Ordering::SeqCst);
                Ok(AuthenticatedSession {
                    user_id: "USER1".to_owned(),
                    access_token: Zeroizing::new("fresh-token".to_owned()),
                })
            })),
        );

        assert_eq!(broker.name(), "shoonya");
        let now_ist = OffsetDateTime::from_unix_timestamp(1_795_400_000)
            .unwrap()
            .replace_date(Date::from_calendar_date(2026, Month::November, 24).unwrap());
        let chain = broker
            .discover_option_chain_contracts("NIFTY", now_ist)
            .unwrap();
        assert_eq!(chain.contracts.len(), 2);
        // Second lookup on later expiry hits in-memory cache without another HTTP download!
        let resolved = broker
            .resolve_contracts_by_symbols("NIFTY", &["NIFTY29DEC26C26000".to_owned()], now_ist)
            .unwrap();
        assert_eq!(resolved.len(), 1);

        let spot = broker.get_spot_quote("NIFTY").unwrap();
        assert_eq!(spot.ltp_paise, Some(2_534_965));
        assert_eq!(relogin_count.load(Ordering::SeqCst), 1);

        let opt_quotes = broker.get_option_quotes(&chain.contracts[..1]).unwrap();
        assert_eq!(opt_quotes[0].1.bid_paise, Some(11_000));
        assert_eq!(opt_quotes[0].1.ask_paise, Some(11_050));

        let limits = broker.get_account_limits().unwrap();
        assert_eq!(limits.available_margin_paise(), 17_500_025);

        assert!(broker.get_positions().unwrap().is_empty());
        let positions = broker.get_positions().unwrap();
        assert_eq!(positions[0].net_qty, -65);
        assert_eq!(positions[0].sell_avg_paise, Some(11_013));

        let orders = broker.get_orders().unwrap();
        assert_eq!(orders[0].status, OrderStatus::Complete);
        assert_eq!(orders[0].filled_qty, 65);
        assert_eq!(orders[0].client_tag.as_deref(), Some("nsp-1"));

        let placed = broker
            .place_order(&OrderRequest {
                exchange: "NFO".to_owned(),
                token: "100".to_owned(),
                trading_symbol: "NIFTY01DEC26C25500".to_owned(),
                side: OrderSide::Buy,
                product: ProductType::Margin,
                quantity: 65,
                price_type: OrderPriceType::StopLimit {
                    trigger_paise: 13_000,
                    limit_paise: 13_200,
                },
                retention: Retention::Day,
                client_tag: "nsp-sl".to_owned(),
            })
            .unwrap();
        assert_eq!(placed.order_id, "2604170002");

        broker.cancel_order("2604170002").unwrap();

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 11);
        // Verify NorenWClientAPI requests never send jKey!
        for req in &requests[1..] {
            assert!(!req.contains("jKey="));
        }
    }

    #[test]
    fn place_order_timeout_returns_ambiguous_timeout_for_reconciliation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let stall = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(700));
        });
        let agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_millis(120)))
            .build()
            .into();
        let mut broker = ShoonyaBroker::with_custom_transport(
            agent,
            ShoonyaEndpoints {
                api_base: base,
                ..ShoonyaEndpoints::default()
            },
            Some(AuthenticatedSession {
                user_id: "USER1".to_owned(),
                access_token: Zeroizing::new("tok".to_owned()),
            }),
            None,
            None,
        );
        let err = broker
            .place_order(&OrderRequest {
                exchange: "NFO".to_owned(),
                token: "100".to_owned(),
                trading_symbol: "NIFTY01DEC26C25500".to_owned(),
                side: OrderSide::Buy,
                product: ProductType::Margin,
                quantity: 65,
                price_type: OrderPriceType::Limit { price_paise: 5_000 },
                retention: Retention::Day,
                client_tag: "nsp-timeout".to_owned(),
            })
            .unwrap_err();
        assert_eq!(
            err,
            BrokerError::AmbiguousTimeout {
                stage: "PlaceOrder"
            }
        );
        stall.join().unwrap();
    }

    #[test]
    fn shoonya_error_paths_status_mappings_and_validations_fail_closed() {
        // 1. from_config_path with missing config returns AuthRequired on session check.
        let missing_cfg = std::env::temp_dir().join(format!("missing-{}.env", std::process::id()));
        let mut unconfigured = ShoonyaBroker::from_config_path(
            missing_cfg,
            Some("ACT1".to_owned()),
            "26000".to_owned(),
        );
        assert!(matches!(
            unconfigured.ensure_session(),
            Err(BrokerError::AuthRequired(_))
        ));
        assert!(matches!(
            unconfigured.get_spot_quote("BANKNIFTY"),
            Err(BrokerError::Rejected { .. })
        ));

        // 2. PlaceOrder / CancelOrder client-side input rejections before network hit.
        let mut base_req = OrderRequest {
            exchange: "NFO".to_owned(),
            token: "100".to_owned(),
            trading_symbol: "NIFTY01DEC26C25500".to_owned(),
            side: OrderSide::Buy,
            product: ProductType::Margin,
            quantity: 0,
            price_type: OrderPriceType::Limit { price_paise: 5_000 },
            retention: Retention::Ioc,
            client_tag: "nsp-val".to_owned(),
        };
        assert_eq!(encode_noren_tsym("M&M01DEC26C3000"), "M%26M01DEC26C3000");
        assert_eq!(parse_order_status("REJECT"), OrderStatus::Rejected);
        assert_eq!(parse_order_status("EXPIRED"), OrderStatus::Canceled);
        assert_eq!(
            parse_order_status("TRIGGER PENDING"),
            OrderStatus::TriggerPending
        );
        assert_eq!(parse_order_status("OPEN PENDING"), OrderStatus::Pending);
        assert!(matches!(
            unconfigured.place_order(&base_req),
            Err(BrokerError::Rejected {
                category: "invalid quantity",
                ..
            })
        ));
        base_req.quantity = 65;
        base_req.product = ProductType::Other;
        assert!(matches!(
            unconfigured.place_order(&base_req),
            Err(BrokerError::Rejected {
                category: "unsupported product type",
                ..
            })
        ));
        base_req.product = ProductType::Intraday;
        base_req.price_type = OrderPriceType::Limit { price_paise: 0 };
        assert!(matches!(
            unconfigured.place_order(&base_req),
            Err(BrokerError::Rejected {
                category: "invalid limit price",
                ..
            })
        ));
        base_req.price_type = OrderPriceType::StopLimit {
            trigger_paise: -5,
            limit_paise: 100,
        };
        assert!(matches!(
            unconfigured.place_order(&base_req),
            Err(BrokerError::Rejected {
                category: "invalid trigger price",
                ..
            })
        ));
        assert!(matches!(
            unconfigured.cancel_order("bad id!"),
            Err(BrokerError::Rejected {
                category: "invalid order id",
                ..
            })
        ));

        // 3. HTTP status & rejection classifications across endpoints.
        let responses = vec![
            // HTTP 401 -> relogin -> HTTP 200 mismatched quote token -> InvalidResponse
            (401, b"Unauthorized".to_vec()),
            (
                200,
                br#"{"stat":"Ok","exch":"NSE","token":"99999","lp":"25000.00"}"#.to_vec(),
            ),
            // HTTP 429 x3 -> RateLimited
            (429, b"Too Many Requests".to_vec()),
            (429, b"Too Many Requests".to_vec()),
            (429, b"Too Many Requests".to_vec()),
            // HTTP 502 -> Transport
            (502, b"Bad Gateway".to_vec()),
            // Malformed JSON -> InvalidResponse
            (200, b"not-json".to_vec()),
            // Limits Not_Ok margin rejection
            (
                200,
                br#"{"stat":"Not_Ok","emsg":"Insufficient Margin Shortfall"}"#.to_vec(),
            ),
            // PositionBook with Intraday, Overnight CF (`cfsellavgprc`), & Other products
            (
                200,
                br#"[{"stat":"Ok","exch":"NFO","tsym":"NIFTY01DEC26P25000","token":"102","prd":"I","netqty":"65","netavgprc":"45.25"},{"stat":"Ok","exch":"NFO","tsym":"NIFTY01DEC26C25500","token":"103","prd":"M","netqty":"-65","cfsellavgprc":"92.50","totsellavgprc":"0.00"},{"stat":"Ok","exch":"NSE","tsym":"INFY","token":"1594","prd":"C","netqty":"10","netavgprc":"1500.00"}]"#.to_vec(),
            ),
            // OrderBook with multiple statuses (OPEN, TRIGGER_PENDING, CANCELED, REJECTED, PENDING, UNKNOWN)
            (
                200,
                br#"[
                    {"stat":"Ok","norenordno":"O1","exch":"NFO","tsym":"A","trantype":"B","prd":"I","status":"OPEN","qty":"65"},
                    {"stat":"Ok","norenordno":"O2","exch":"NFO","tsym":"A","trantype":"S","prd":"M","status":"TRIGGER_PENDING","qty":"65"},
                    {"stat":"Ok","norenordno":"O3","exch":"NFO","tsym":"A","trantype":"B","prd":"M","status":"CANCELLED","qty":"65"},
                    {"stat":"Ok","norenordno":"O4","exch":"NFO","tsym":"A","trantype":"B","prd":"M","status":"REJECTED","qty":"65"},
                    {"stat":"Ok","norenordno":"O5","exch":"NFO","tsym":"A","trantype":"B","prd":"M","status":"OPEN_PENDING","qty":"65"},
                    {"stat":"Ok","norenordno":"O6","exch":"NFO","tsym":"A","trantype":"B","prd":"M","status":"MYSTERY","qty":"65"}
                ]"#
                .to_vec(),
            ),
            // PlaceOrder circuit rejection
            (
                200,
                br#"{"stat":"Not_Ok","emsg":"Order price outside circuit freeze band"}"#.to_vec(),
            ),
            // CancelOrder RMS rejection
            (200, br#"{"stat":"Not_Ok","emsg":"RMS blocked"}"#.to_vec()),
        ];

        let (base, server) = mock_http_sequence(responses);
        let mut broker = ShoonyaBroker::with_custom_transport(
            test_agent(),
            ShoonyaEndpoints {
                api_base: base,
                quote_pace: Duration::ZERO,
                rate_limit_backoff: Duration::ZERO,
                ..ShoonyaEndpoints::default()
            },
            Some(AuthenticatedSession {
                user_id: "U1".to_owned(),
                access_token: Zeroizing::new("tok1".to_owned()),
            }),
            Some("ACT1".to_owned()),
            Some(Box::new(|| {
                Ok(AuthenticatedSession {
                    user_id: "U1".to_owned(),
                    access_token: Zeroizing::new("tok2".to_owned()),
                })
            })),
        );

        assert!(matches!(
            broker.get_spot_quote("NIFTY"),
            Err(BrokerError::InvalidResponse(_))
        ));
        assert_eq!(
            broker.get_spot_quote("NIFTY"),
            Err(BrokerError::RateLimited)
        );
        assert!(matches!(
            broker.get_spot_quote("NIFTY"),
            Err(BrokerError::Transport(_))
        ));
        assert!(matches!(
            broker.get_spot_quote("NIFTY"),
            Err(BrokerError::InvalidResponse(_))
        ));
        assert_eq!(
            broker.get_account_limits(),
            Err(BrokerError::Rejected {
                stage: "Limits",
                category: "insufficient margin"
            })
        );

        let pos = broker.get_positions().unwrap();
        assert_eq!(pos.len(), 3);
        assert_eq!(pos[0].product, ProductType::Intraday);
        assert_eq!(pos[0].buy_avg_paise, Some(4_525));
        assert_eq!(pos[1].product, ProductType::Margin);
        assert_eq!(pos[1].sell_avg_paise, Some(9_250));
        assert_eq!(pos[2].product, ProductType::Other);

        let ords = broker.get_orders().unwrap();
        assert_eq!(ords.len(), 6);
        assert_eq!(ords[0].status, OrderStatus::Open);
        assert_eq!(ords[1].status, OrderStatus::TriggerPending);
        assert_eq!(ords[2].status, OrderStatus::Canceled);
        assert_eq!(ords[3].status, OrderStatus::Rejected);
        assert_eq!(ords[4].status, OrderStatus::Pending);
        assert_eq!(ords[5].status, OrderStatus::Unknown);

        base_req.price_type = OrderPriceType::Limit { price_paise: 5_000 };
        assert_eq!(
            broker.place_order(&base_req),
            Err(BrokerError::Rejected {
                stage: "PlaceOrder",
                category: "circuit or freeze band"
            })
        );
        assert_eq!(
            broker.cancel_order("ORD123"),
            Err(BrokerError::Rejected {
                stage: "CancelOrder",
                category: "rms rejection"
            })
        );

        let _ = server.join().unwrap();

        // 4. Session Expired without login_hook, OrderBook "no data", CancelOrder "Ok", and malformed book rows.
        let responses2 = vec![
            (
                200,
                br#"{"stat":"Not_Ok","emsg":"Session Expired"}"#.to_vec(),
            ),
            (
                200,
                br#"{"stat":"Not_Ok","emsg":"Error : no data"}"#.to_vec(),
            ),
            (200, br#"{"stat":"Ok","result":"ORD1"}"#.to_vec()),
            (
                200,
                br#"[{"stat":"Ok","exch":"NFO","tsym":"A","token":"1","prd":"M","netqty":"bad"}]"#
                    .to_vec(),
            ),
            (
                200,
                br#"[{"stat":"Ok","norenordno":"O1","exch":"NFO","tsym":"A","trantype":"X","status":"OPEN","qty":"65"}]"#
                    .to_vec(),
            ),
        ];
        let (base2, server2) = mock_http_sequence(responses2);
        let mut no_hook = ShoonyaBroker::with_custom_transport(
            test_agent(),
            ShoonyaEndpoints {
                api_base: base2,
                quote_pace: Duration::ZERO,
                rate_limit_backoff: Duration::ZERO,
                ..ShoonyaEndpoints::default()
            },
            Some(AuthenticatedSession {
                user_id: "U1".to_owned(),
                access_token: Zeroizing::new("tok1".to_owned()),
            }),
            None,
            None,
        );
        assert!(matches!(
            no_hook.get_spot_quote("NIFTY"),
            Err(BrokerError::AuthRequired(_))
        ));
        assert!(no_hook.get_orders().unwrap().is_empty());
        assert!(no_hook.cancel_order("ORD1").is_ok());
        assert!(matches!(
            no_hook.get_positions(),
            Err(BrokerError::InvalidResponse(_))
        ));
        assert!(matches!(
            no_hook.get_orders(),
            Err(BrokerError::InvalidResponse(_))
        ));
        let _ = server2.join().unwrap();

        // Test default ureq status-as-error agent (`http_status_as_error(true)`) + remaining book validation branches.
        let responses3: Vec<(u16, Vec<u8>)> = vec![
            (401, b"{}".to_vec()),
            (200, br#"{"stat":"Ok","cash":"1000.00"}"#.to_vec()),
            (429, b"{}".to_vec()),
            (429, b"{}".to_vec()),
            (429, b"{}".to_vec()),
            (500, b"{}".to_vec()),
            (200, br#"{"stat":"Ok","cash":"not_a_number"}"#.to_vec()),
            (
                200,
                br#"{"stat":"Ok","collateral":"92233720368547750.00","brkcollamt":"1000.00"}"#
                    .to_vec(),
            ),
            (
                200,
                br#"{"stat":"Not_Ok","emsg":"Backend problem"}"#.to_vec(),
            ),
            (200, br#"{"stat":"Ok"}"#.to_vec()),
            (200, br#"[{"stat":"Not_Ok"}]"#.to_vec()),
            (
                200,
                br#"{"stat":"Not_Ok","emsg":"Backend problem"}"#.to_vec(),
            ),
            (200, br#"{"stat":"Ok"}"#.to_vec()),
            (200, br#"[{"stat":"Not_Ok"}]"#.to_vec()),
            (
                200,
                br#"[{"stat":"Ok","norenordno":"O1","exch":"NFO","tsym":"A","trantype":"B","status":"OPEN","qty":"bad"}]"#
                    .to_vec(),
            ),
            (
                200,
                br#"[{"stat":"Ok","norenordno":"O1","exch":"NFO","tsym":"A","trantype":"B","status":"OPEN","qty":"65","fillshares":"bad"}]"#
                    .to_vec(),
            ),
            (
                200,
                br#"{"stat":"Ok","exch":"WRONG","token":"26000","lp":"25000.00"}"#.to_vec(),
            ),
        ];
        let (base3, server3) = mock_http_sequence(responses3);
        let strict_agent: ureq::Agent = ureq::Agent::config_builder().build().into();
        let mut strict_broker = ShoonyaBroker::with_custom_transport(
            strict_agent.clone(),
            ShoonyaEndpoints {
                api_base: base3.clone(),
                quote_pace: Duration::ZERO,
                rate_limit_backoff: Duration::from_millis(1),
                ..ShoonyaEndpoints::default()
            },
            Some(AuthenticatedSession {
                user_id: "U1".to_owned(),
                access_token: Zeroizing::new("expired".to_owned()),
            }),
            Some("ACT1".to_owned()),
            Some(Box::new(|| {
                Ok(AuthenticatedSession {
                    user_id: "U1".to_owned(),
                    access_token: Zeroizing::new("fresh".to_owned()),
                })
            })),
        );
        // 401 triggers relogin hook -> 200 Limits succeeds!
        assert_eq!(
            strict_broker.get_account_limits().unwrap().cash_paise,
            100_000
        );
        // 3x 429 exhausts retries -> RateLimited!
        assert!(matches!(
            strict_broker.get_account_limits(),
            Err(BrokerError::RateLimited)
        ));
        // 500 -> Transport!
        assert!(matches!(
            strict_broker.get_account_limits(),
            Err(BrokerError::Transport(_))
        ));
        // Invalid numeric field & collateral overflow in Limits!
        assert!(matches!(
            strict_broker.get_account_limits(),
            Err(BrokerError::InvalidResponse(_))
        ));
        assert!(matches!(
            strict_broker.get_account_limits(),
            Err(BrokerError::InvalidResponse(_))
        ));
        // PositionBook: Not_Ok, non-array, non-Ok entry!
        assert!(matches!(
            strict_broker.get_positions(),
            Err(BrokerError::Rejected { .. })
        ));
        assert!(matches!(
            strict_broker.get_positions(),
            Err(BrokerError::InvalidResponse(_))
        ));
        assert!(matches!(
            strict_broker.get_positions(),
            Err(BrokerError::InvalidResponse(_))
        ));
        // OrderBook: Not_Ok, non-array, non-Ok entry, bad qty, bad fillshares!
        assert!(matches!(
            strict_broker.get_orders(),
            Err(BrokerError::Rejected { .. })
        ));
        assert!(matches!(
            strict_broker.get_orders(),
            Err(BrokerError::InvalidResponse(_))
        ));
        assert!(matches!(
            strict_broker.get_orders(),
            Err(BrokerError::InvalidResponse(_))
        ));
        assert!(matches!(
            strict_broker.get_orders(),
            Err(BrokerError::InvalidResponse(_))
        ));
        assert!(matches!(
            strict_broker.get_orders(),
            Err(BrokerError::InvalidResponse(_))
        ));
        // GetQuotes mismatched exchange!
        assert!(matches!(
            strict_broker.get_spot_quote("NIFTY"),
            Err(BrokerError::InvalidResponse(_))
        ));
        let _ = server3.join().unwrap();

        // 401 without relogin hook on strict agent -> AuthRequired!
        let (base4, server4) = mock_http_sequence(vec![(401, b"{}".to_vec())]);
        let mut strict_no_hook = ShoonyaBroker::with_custom_transport(
            strict_agent,
            ShoonyaEndpoints {
                api_base: base4,
                quote_pace: Duration::ZERO,
                rate_limit_backoff: Duration::ZERO,
                ..ShoonyaEndpoints::default()
            },
            Some(AuthenticatedSession {
                user_id: "U1".to_owned(),
                access_token: Zeroizing::new("expired".to_owned()),
            }),
            None,
            None,
        );
        assert!(matches!(
            strict_no_hook.get_account_limits(),
            Err(BrokerError::AuthRequired(_))
        ));
        let _ = server4.join().unwrap();
    }

    #[test]
    fn non_idempotent_place_order_401_and_403_never_blindly_replay_and_tsym_encodes_safely() {
        assert_eq!(encode_noren_tsym("M&M28MAY26C3000"), "M%26M28MAY26C3000");
        assert_eq!(encode_noren_tsym("BAJAJ+100%"), "BAJAJ%2B100%25");

        let relogins = Arc::new(Mutex::new(0usize));
        let relogins_hook = Arc::clone(&relogins);
        let responses = vec![
            // 1. PlaceOrder HTTP 401 -> refreshes session via hook -> returns AmbiguousTimeout (NO replay!).
            (401, b"Unauthorized".to_vec()),
            // 2. PlaceOrder HTTP 403 -> returns AuthRequired immediately WITHOUT calling login_hook!
            (403, b"Forbidden IP".to_vec()),
            // 3. Limits with overlapping `collateral` & `brkcollamt` + negative `payout` -> no 2x double count!
            (
                200,
                br#"{"stat":"Ok","cash":"100000.00","collateral":"50000.00","brkcollamt":"50000.00","payout":"-2500.00","marginused":"10000.00"}"#.to_vec(),
            ),
        ];
        let (base, server) = mock_http_sequence(responses);
        let mut broker = ShoonyaBroker::with_custom_transport(
            test_agent(),
            ShoonyaEndpoints {
                api_base: base,
                quote_pace: Duration::ZERO,
                rate_limit_backoff: Duration::ZERO,
                ..ShoonyaEndpoints::default()
            },
            Some(AuthenticatedSession {
                user_id: "U1".to_owned(),
                access_token: Zeroizing::new("tok0".to_owned()),
            }),
            Some("U1".to_owned()),
            Some(Box::new(move || {
                *relogins_hook.lock().unwrap() += 1;
                Ok(AuthenticatedSession {
                    user_id: "U1".to_owned(),
                    access_token: Zeroizing::new("tok1".to_owned()),
                })
            })),
        );

        let mut req = OrderRequest {
            exchange: "NFO".to_owned(),
            token: "101".to_owned(),
            trading_symbol: "NIFTY01DEC26C25500".to_owned(),
            side: OrderSide::Buy,
            product: ProductType::Margin,
            quantity: 65,
            price_type: OrderPriceType::Limit { price_paise: 5_000 },
            retention: Retention::Day,
            client_tag: "nspEL1234567890ab".to_owned(),
        };

        // 401 refreshes session once and returns AmbiguousTimeout so engine checks OrderBook first!
        assert_eq!(
            broker.place_order(&req),
            Err(BrokerError::AmbiguousTimeout {
                stage: "PlaceOrder"
            })
        );
        assert_eq!(*relogins.lock().unwrap(), 1);

        // 403 does NOT trigger relogin on non-idempotent PlaceOrder!
        assert!(matches!(
            broker.place_order(&req),
            Err(BrokerError::AuthRequired(_))
        ));
        assert_eq!(*relogins.lock().unwrap(), 1);

        // Limits avoids 2x collateral double-count and handles negative payout!
        let lim = broker.get_account_limits().unwrap();
        assert_eq!(lim.collateral_paise, 5_000_000);
        assert_eq!(lim.payout_paise, 250_000);
        assert_eq!(lim.available_margin_paise(), 13_750_000);

        // Client-side validation rejects invalid exchange, symbol, and non-alphanumeric remarks!
        req.exchange = "BAD EXCH".to_owned();
        assert!(matches!(
            broker.place_order(&req),
            Err(BrokerError::Rejected {
                category: "invalid exchange",
                ..
            })
        ));
        req.exchange = "NFO".to_owned();
        req.trading_symbol = "NIFTY\nBAD".to_owned();
        assert!(matches!(
            broker.place_order(&req),
            Err(BrokerError::Rejected {
                category: "invalid trading symbol",
                ..
            })
        ));
        req.trading_symbol = "NIFTY01DEC26C25500".to_owned();
        req.client_tag = "bad tag!".to_owned();
        assert!(matches!(
            broker.place_order(&req),
            Err(BrokerError::Rejected {
                category: "invalid client tag",
                ..
            })
        ));

        let _ = server.join().unwrap();

        // Also verify `http_status_as_error(false)` paths (`post_api` status 401/403/429/502 + Not_Ok Session Expired on PlaceOrder)!
        let no_err_agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(2)))
            .build()
            .into();
        let responses_no_err: Vec<(u16, Vec<u8>)> = vec![
            // PlaceOrder 401 -> relogin + AmbiguousTimeout
            (401, b"{}".to_vec()),
            // GetQuotes 403 -> relogin + 200 Ok
            (403, b"{}".to_vec()),
            (
                200,
                br#"{"stat":"Ok","exch":"NSE","token":"26000","lp":"25400.00"}"#.to_vec(),
            ),
            // GetQuotes 429 -> retry + 502 Transport
            (429, b"{}".to_vec()),
            (502, b"{}".to_vec()),
            // PlaceOrder 200 Not_Ok Session Expired -> relogin + AmbiguousTimeout
            (
                200,
                br#"{"stat":"Not_Ok","emsg":"Session Expired"}"#.to_vec(),
            ),
        ];
        let (base_ne, srv_ne) = mock_http_sequence(responses_no_err);
        let mut ne_broker = ShoonyaBroker::with_custom_transport(
            no_err_agent.clone(),
            ShoonyaEndpoints {
                api_base: base_ne,
                quote_pace: Duration::ZERO,
                rate_limit_backoff: Duration::ZERO,
                ..ShoonyaEndpoints::default()
            },
            Some(AuthenticatedSession {
                user_id: "U1".to_owned(),
                access_token: Zeroizing::new("tok0".to_owned()),
            }),
            Some("U1".to_owned()),
            Some(Box::new(|| {
                Ok(AuthenticatedSession {
                    user_id: "U1".to_owned(),
                    access_token: Zeroizing::new("tok_refreshed".to_owned()),
                })
            })),
        );
        req.client_tag = "nspEL1234567890ab".to_owned();
        assert_eq!(
            ne_broker.place_order(&req),
            Err(BrokerError::AmbiguousTimeout {
                stage: "PlaceOrder"
            })
        );
        // Reset relogin capability by testing fresh instance on 403/429/502 + Not_Ok Session Expired
        assert!(ne_broker.get_spot_quote("NIFTY").is_ok());
        assert!(matches!(
            ne_broker.get_spot_quote("NIFTY"),
            Err(BrokerError::Transport(_))
        ));
        assert_eq!(
            ne_broker.place_order(&req),
            Err(BrokerError::AmbiguousTimeout {
                stage: "PlaceOrder"
            })
        );
        assert_eq!(ne_broker.name(), "shoonya");
        assert_eq!(
            ne_broker.fee_schedule().brokerage_per_order_rs,
            FeeSchedule::shoonya_2026().brokerage_per_order_rs
        );
        assert!(ne_broker.ensure_session().is_ok());
        assert_eq!(
            ne_broker.get_spot_quote("BANKNIFTY"),
            Err(BrokerError::Rejected {
                stage: "GetQuotes",
                category: "unsupported spot underlying"
            })
        );
        let _ = srv_ne.join().unwrap();

        // Also verify default `http_status_as_error(true)` 401/403/429/502 + no-login-hook + quote mismatch!
        let err_responses: Vec<(u16, Vec<u8>)> = vec![
            (401, b"{}".to_vec()),
            (403, b"{}".to_vec()),
            (403, b"{}".to_vec()),
            (
                200,
                br#"{"stat":"Ok","exch":"NSE","token":"26000","lp":"25400.00"}"#.to_vec(),
            ),
            (502, b"{}".to_vec()),
            (429, b"{}".to_vec()),
            (429, b"{}".to_vec()),
            (429, b"{}".to_vec()),
            (
                200,
                br#"{"stat":"Not_Ok","emsg":"Too many requests"}"#.to_vec(),
            ),
            (
                200,
                br#"{"stat":"Not_Ok","emsg":"Too many requests"}"#.to_vec(),
            ),
            (
                200,
                br#"{"stat":"Not_Ok","emsg":"Too many requests"}"#.to_vec(),
            ),
            (
                200,
                br#"{"stat":"Ok","exch":"NSE","token":"99999","lp":"25400.00"}"#.to_vec(),
            ),
        ];
        let (base_err, srv_err) = mock_http_sequence(err_responses);
        let err_agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(2)))
            .build()
            .into();
        let mut err_broker = ShoonyaBroker::with_custom_transport(
            err_agent.clone(),
            ShoonyaEndpoints {
                api_base: base_err,
                quote_pace: Duration::ZERO,
                rate_limit_backoff: Duration::ZERO,
                ..ShoonyaEndpoints::default()
            },
            Some(AuthenticatedSession {
                user_id: "U1".to_owned(),
                access_token: Zeroizing::new("tok0".to_owned()),
            }),
            Some("U1".to_owned()),
            Some(Box::new(|| {
                Ok(AuthenticatedSession {
                    user_id: "U1".to_owned(),
                    access_token: Zeroizing::new("tok_refreshed".to_owned()),
                })
            })),
        );
        assert_eq!(
            err_broker.place_order(&req),
            Err(BrokerError::AmbiguousTimeout {
                stage: "PlaceOrder"
            })
        );
        assert!(matches!(
            err_broker.place_order(&req),
            Err(BrokerError::AuthRequired(_))
        ));
        assert!(err_broker.get_spot_quote("NIFTY").is_ok());
        assert!(matches!(
            err_broker.get_spot_quote("NIFTY"),
            Err(BrokerError::Transport(_))
        ));
        assert_eq!(
            err_broker.get_spot_quote("NIFTY"),
            Err(BrokerError::RateLimited)
        );
        assert_eq!(
            err_broker.get_spot_quote("NIFTY"),
            Err(BrokerError::RateLimited)
        );
        assert!(matches!(
            err_broker.get_spot_quote("NIFTY"),
            Err(BrokerError::InvalidResponse(_))
        ));
        let _ = srv_err.join().unwrap();

        let mut no_hook_broker = ShoonyaBroker::with_custom_transport(
            err_agent,
            ShoonyaEndpoints {
                api_base: "http://127.0.0.1:1".to_owned(),
                ..ShoonyaEndpoints::default()
            },
            None,
            None,
            None,
        );
        assert!(matches!(
            no_hook_broker.ensure_session(),
            Err(BrokerError::AuthRequired(_))
        ));
        no_hook_broker.user_id = Some("U1".to_owned());
        no_hook_broker.access_token = Some(Zeroizing::new("tok0".to_owned()));
        assert!(matches!(
            no_hook_broker.get_spot_quote("NIFTY"),
            Err(BrokerError::Transport(_))
        ));

        let (base_ne2, srv_ne2) = mock_http_sequence(vec![
            (403, b"{}".to_vec()),
            (429, b"{}".to_vec()),
            (429, b"{}".to_vec()),
            (429, b"{}".to_vec()),
        ]);
        let mut ne2 = ShoonyaBroker::with_custom_transport(
            no_err_agent,
            ShoonyaEndpoints {
                api_base: base_ne2,
                quote_pace: Duration::ZERO,
                rate_limit_backoff: Duration::ZERO,
                ..ShoonyaEndpoints::default()
            },
            Some(AuthenticatedSession {
                user_id: "U1".to_owned(),
                access_token: Zeroizing::new("tok0".to_owned()),
            }),
            Some("U1".to_owned()),
            None,
        );
        assert!(matches!(
            ne2.place_order(&req),
            Err(BrokerError::AuthRequired(_))
        ));
        assert_eq!(ne2.get_spot_quote("NIFTY"), Err(BrokerError::RateLimited));
        let _ = srv_ne2.join().unwrap();
    }
}
