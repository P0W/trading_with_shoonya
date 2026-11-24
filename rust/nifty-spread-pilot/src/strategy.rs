//! Broker-agnostic NIFTY 50 option spread strategy engine:
//! - Discounted Put-Call Parity forward discovery from live option chain
//! - Chain Open Interest walls, PCR, ATM IV, 1-sigma horizon, and 25D skew
//! - Two-tier candidate spread ranker (`CORE_INCOME` & `HIGH_POP_1SD`) with
//!   immediate `NO_POSITION_RECOMMENDED` gate
//! - Live position guardian with Priority-1 kill switches and 3-regime
//!   non-linear `F*` OCO solver (`NO_LOSS_PROFIT_LOCK_OCO`,
//!   `PROFIT_LOCK_INFEASIBLE`, `LOSS_MINIMIZATION_OCO`).

use std::collections::BTreeMap;
use std::str::FromStr;

use crate::OptionKind;
use crate::broker::{ChargeBreakdown, FeeSchedule, MarketQuote, OptionContract, paise_to_rupees};
use crate::math::{
    DEFAULT_RISK_FREE_RATE, Greeks, SolveStatus, compute_greeks, implied_forward_from_pair,
    implied_vol_with_status, norm_cdf, round_to_tick, solve_forward_for_spread_value,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StrategyBias {
    BearCall,
    BullPut,
    #[default]
    Auto,
}

impl FromStr for StrategyBias {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "bear_call" | "bear" | "ce" | "call" => Ok(Self::BearCall),
            "bull_put" | "bull" | "pe" | "put" => Ok(Self::BullPut),
            "auto" | "neutral" | "both" => Ok(Self::Auto),
            _ => Err("Strategy bias must be 'auto', 'bear_call', or 'bull_put'."),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChainDiagnostics {
    pub spot_rs: f64,
    pub implied_forward_rs: f64,
    pub parity_used: bool,
    pub parity_atm_strike_rs: Option<f64>,
    pub stale_close_used: bool,
    pub t_days: f64,
    pub atm_iv_pct: Option<f64>,
    pub horizon_1sigma_pts: Option<f64>,
    pub upper_1sigma_level: Option<f64>,
    pub lower_1sigma_level: Option<f64>,
    pub max_call_oi_strike_rs: Option<f64>,
    pub max_call_oi: u64,
    pub max_put_oi_strike_rs: Option<f64>,
    pub max_put_oi: u64,
    pub chain_pcr_oi: Option<f64>,
    pub put_call_skew_25d_pct: Option<f64>,
}

pub fn analyze_chain(
    spot_rs: f64,
    t_days: f64,
    quotes: &[(OptionContract, MarketQuote)],
    allow_prev_close: bool,
) -> Result<ChainDiagnostics, &'static str> {
    if !spot_rs.is_finite() || spot_rs <= 0.0 || !t_days.is_finite() || t_days <= 0.0 {
        return Err("Spot price and time to expiry must be finite positive numbers.");
    }
    let t_years = (t_days / 365.0).max(1e-6);
    let r = DEFAULT_RISK_FREE_RATE;
    let target_expiry = quotes.first().map(|(c, _)| c.expiry);

    // Group call & put reference prices by strike_paise for the single target expiry.
    type RefPrice = Option<(f64, bool)>;
    let mut by_strike: BTreeMap<i64, (RefPrice, RefPrice)> = BTreeMap::new();
    let mut stale_close_used = false;
    let mut total_call_oi = 0_u64;
    let mut total_put_oi = 0_u64;
    let mut max_call_oi = (0_u64, None);
    let mut max_put_oi = (0_u64, None);

    for (contract, quote) in quotes {
        if target_expiry.is_some_and(|exp| contract.expiry != exp) {
            continue;
        }
        let strike_rs = paise_to_rupees(contract.strike_paise);
        if let Some(oi) = quote.open_interest {
            match contract.option_kind {
                OptionKind::Call => {
                    total_call_oi = total_call_oi.saturating_add(oi);
                    if oi > max_call_oi.0 {
                        max_call_oi = (oi, Some(strike_rs));
                    }
                }
                OptionKind::Put => {
                    total_put_oi = total_put_oi.saturating_add(oi);
                    if oi > max_put_oi.0 {
                        max_put_oi = (oi, Some(strike_rs));
                    }
                }
            }
        }
        if let Some((price_paise, used_close)) = quote.reference_price_paise(allow_prev_close) {
            stale_close_used |= used_close;
            let entry = by_strike.entry(contract.strike_paise).or_default();
            let val = (paise_to_rupees(price_paise), used_close);
            match contract.option_kind {
                OptionKind::Call => entry.0 = Some(val),
                OptionKind::Put => entry.1 = Some(val),
            }
        }
    }

    // Nearest ATM strike with both Call & Put prices for Discounted Put-Call Parity.
    let spot_paise = (spot_rs * 100.0).round() as i64;
    let nearest_pair = by_strike
        .iter()
        .filter_map(|(&strike_paise, &(call, put))| {
            let ((c_rs, _), (p_rs, _)) = (call?, put?);
            Some(((strike_paise - spot_paise).abs(), strike_paise, c_rs, p_rs))
        })
        .min_by_key(|tuple| tuple.0);

    let (implied_forward_rs, parity_used, parity_atm_strike_rs) =
        if let Some((_, strike_paise, c_rs, p_rs)) = nearest_pair {
            let k_rs = paise_to_rupees(strike_paise);
            match implied_forward_from_pair(Some(spot_rs), k_rs, c_rs, p_rs, t_years, r) {
                Ok(parity) => (parity.implied_forward, true, Some(k_rs)),
                Err(_) => (spot_rs * (r * t_years).exp(), false, None),
            }
        } else {
            (spot_rs * (r * t_years).exp(), false, None)
        };

    // Solve IVs across chain to extract ATM IV and 25D Put-Call Risk-Reversal Skew.
    let mut atm_candidates: Vec<(f64, f64)> = Vec::new();
    let mut call_deltas: Vec<(f64, f64)> = Vec::new();
    let mut put_deltas: Vec<(f64, f64)> = Vec::new();

    for (contract, quote) in quotes {
        if target_expiry.is_some_and(|exp| contract.expiry != exp) {
            continue;
        }
        let Some((price_paise, _)) = quote.reference_price_paise(allow_prev_close) else {
            continue;
        };
        let strike_rs = paise_to_rupees(contract.strike_paise);
        let mid_rs = paise_to_rupees(price_paise);
        let iv_res = implied_vol_with_status(
            mid_rs,
            implied_forward_rs,
            strike_rs,
            t_years,
            contract.option_kind,
            r,
        );
        if !iv_res.reliable {
            continue;
        }
        let g = compute_greeks(
            implied_forward_rs,
            strike_rs,
            t_years,
            iv_res.iv,
            contract.option_kind,
            r,
        );
        atm_candidates.push(((strike_rs - implied_forward_rs).abs(), iv_res.iv));
        match contract.option_kind {
            OptionKind::Call if strike_rs >= implied_forward_rs * 0.99 => {
                call_deltas.push((g.delta.abs(), iv_res.iv * 100.0));
            }
            OptionKind::Put if strike_rs <= implied_forward_rs * 1.01 => {
                put_deltas.push((g.delta.abs(), iv_res.iv * 100.0));
            }
            _ => {}
        }
    }

    let atm_iv = if atm_candidates.is_empty() {
        None
    } else {
        atm_candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
        let top = &atm_candidates[..atm_candidates.len().min(2)];
        Some(top.iter().map(|x| x.1).sum::<f64>() / (top.len() as f64))
    };
    let horizon_1sigma_pts = atm_iv.map(|iv| implied_forward_rs * iv * t_years.sqrt());
    let put_call_skew_25d_pct = match (
        interp_iv_at_delta(&mut call_deltas, 0.25),
        interp_iv_at_delta(&mut put_deltas, 0.25),
    ) {
        (Some(iv_c), Some(iv_p)) => Some(iv_p - iv_c),
        _ => None,
    };
    let chain_pcr_oi = (total_call_oi > 0).then(|| total_put_oi as f64 / total_call_oi as f64);

    Ok(ChainDiagnostics {
        spot_rs,
        implied_forward_rs,
        parity_used,
        parity_atm_strike_rs,
        stale_close_used,
        t_days,
        atm_iv_pct: atm_iv.map(|v| v * 100.0),
        horizon_1sigma_pts,
        upper_1sigma_level: horizon_1sigma_pts.map(|s| implied_forward_rs + s),
        lower_1sigma_level: horizon_1sigma_pts.map(|s| implied_forward_rs - s),
        max_call_oi_strike_rs: max_call_oi.1,
        max_call_oi: max_call_oi.0,
        max_put_oi_strike_rs: max_put_oi.1,
        max_put_oi: max_put_oi.0,
        chain_pcr_oi,
        put_call_skew_25d_pct,
    })
}

fn interp_iv_at_delta(pairs: &mut [(f64, f64)], target: f64) -> Option<f64> {
    if pairs.is_empty() {
        return None;
    }
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
    if pairs.len() == 1 {
        return ((pairs[0].0 - target).abs() <= 0.12).then_some(pairs[0].1);
    }
    for window in pairs.windows(2) {
        let ((d0, iv0), (d1, iv1)) = (window[0], window[1]);
        if d0 <= target && target <= d1 {
            if (d1 - d0).abs() < 1e-9 {
                return Some(iv0);
            }
            let w = (target - d0) / (d1 - d0);
            return Some(iv0 + w * (iv1 - iv0));
        }
    }
    let nearest = pairs
        .iter()
        .copied()
        .min_by(|a, b| (a.0 - target).abs().total_cmp(&(b.0 - target).abs()))?;
    ((nearest.0 - target).abs() <= 0.10).then_some(nearest.1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeTier {
    CoreIncome,
    HighPop1Sd,
    BelowEdgeGate,
}

impl EdgeTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CoreIncome => "CORE_INCOME",
            Self::HighPop1Sd => "HIGH_POP_1SD",
            Self::BelowEdgeGate => "BELOW_EDGE_GATE",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateSpread {
    pub option_kind: OptionKind,
    pub short_contract: OptionContract,
    pub long_contract: OptionContract,
    pub short_quote: MarketQuote,
    pub long_quote: MarketQuote,
    pub touch_executable: bool,
    pub width_pts: f64,
    pub short_entry_rs: f64,
    pub long_entry_rs: f64,
    pub entry_credit_pts: f64,
    pub credit_to_width_pct: f64,
    pub otm_cushion_pts: f64,
    pub otm_sigma_multiple: Option<f64>,
    pub breakeven_net_rs: f64,
    pub pop_terminal_pct: f64,
    pub max_profit_net_rs: f64,
    pub max_loss_net_rs: f64,
    pub short_iv_pct: f64,
    pub long_iv_pct: f64,
    pub short_delta: f64,
    pub net_delta: f64,
    pub net_theta_pts_day: f64,
    pub net_theta_rs_day: f64,
    pub oi_wall_backed: bool,
    pub edge_tier: EdgeTier,
    pub composite_score: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RankOutcome {
    pub diagnostics: ChainDiagnostics,
    pub evaluated_pairs: usize,
    pub recommended: Option<CandidateSpread>,
    pub qualified: Vec<CandidateSpread>,
    pub near_misses: Vec<CandidateSpread>,
}

pub fn rank_candidate_spreads(
    spot_rs: f64,
    t_days: f64,
    bias: StrategyBias,
    lots: u32,
    quotes: &[(OptionContract, MarketQuote)],
    fees: FeeSchedule,
    require_executable_touch: bool,
) -> Result<RankOutcome, &'static str> {
    if lots == 0 {
        return Err("Lot count must be at least 1.");
    }
    let diagnostics = analyze_chain(spot_rs, t_days, quotes, !require_executable_touch)?;
    let t_years = (t_days / 365.0).max(1e-6);
    let r = DEFAULT_RISK_FREE_RATE;
    let forward = diagnostics.implied_forward_rs;

    let kinds: &[OptionKind] = match bias {
        StrategyBias::BearCall => &[OptionKind::Call],
        StrategyBias::BullPut => &[OptionKind::Put],
        StrategyBias::Auto => &[OptionKind::Call, OptionKind::Put],
    };

    let mut evaluated_pairs = 0_usize;
    let mut qualified = Vec::new();
    let mut rejected = Vec::new();

    for &kind in kinds {
        let mut by_strike: BTreeMap<i64, (OptionContract, MarketQuote)> = BTreeMap::new();
        for (contract, quote) in quotes {
            if contract.option_kind == kind {
                by_strike.insert(contract.strike_paise, (contract.clone(), *quote));
            }
        }
        let chain_has_oi = by_strike
            .values()
            .any(|(_, q)| q.open_interest.unwrap_or(0) > 0);
        let max_side_oi = by_strike
            .values()
            .filter_map(|(_, q)| q.open_interest)
            .max()
            .unwrap_or(0);

        for (&short_strike_paise, (short_contract, short_quote)) in &by_strike {
            let k_short = paise_to_rupees(short_strike_paise);
            match kind {
                OptionKind::Call if k_short < forward => continue,
                OptionKind::Put if k_short > forward => continue,
                _ => {}
            }
            for width_paise in [15_000_i64, 20_000, 25_000, 30_000] {
                let long_strike_paise = match kind {
                    OptionKind::Call => short_strike_paise + width_paise,
                    OptionKind::Put => short_strike_paise - width_paise,
                };
                let Some((long_contract, long_quote)) = by_strike.get(&long_strike_paise) else {
                    continue;
                };
                if short_contract.lot_size != long_contract.lot_size {
                    continue;
                }
                let qty = short_contract.lot_size.saturating_mul(lots);

                // Determine executable touch vs read-only reference pricing.
                let (short_entry_rs, long_entry_rs, short_mid_rs, long_mid_rs, touch_executable) =
                    match (
                        short_quote.executable_quote(),
                        long_quote.executable_quote(),
                    ) {
                        (Ok(sq), Ok(lq)) => {
                            let s_mid = paise_to_rupees(sq.bid + sq.ask) * 0.5;
                            let l_mid = paise_to_rupees(lq.bid + lq.ask) * 0.5;
                            let s_rel = paise_to_rupees(sq.ask - sq.bid) / s_mid.max(0.05);
                            let l_rel = paise_to_rupees(lq.ask - lq.bid) / l_mid.max(0.05);
                            if s_rel > 0.25 || l_rel > 0.25 {
                                if require_executable_touch {
                                    continue;
                                }
                                (
                                    paise_to_rupees(sq.bid),
                                    paise_to_rupees(lq.ask),
                                    s_mid,
                                    l_mid,
                                    false,
                                )
                            } else {
                                (
                                    paise_to_rupees(sq.bid),
                                    paise_to_rupees(lq.ask),
                                    s_mid,
                                    l_mid,
                                    true,
                                )
                            }
                        }
                        _ if !require_executable_touch => {
                            let Some((sp, _)) = short_quote.reference_price_paise(true) else {
                                continue;
                            };
                            let Some((lp, _)) = long_quote.reference_price_paise(true) else {
                                continue;
                            };
                            let s_rs = paise_to_rupees(sp);
                            let l_rs = paise_to_rupees(lp);
                            (s_rs, l_rs, s_rs, l_rs, false)
                        }
                        _ => continue,
                    };

                let width_pts = paise_to_rupees(width_paise);
                let credit = short_entry_rs - long_entry_rs;
                if credit <= 0.0 || credit >= width_pts {
                    continue;
                }

                let k_long = paise_to_rupees(long_strike_paise);
                let iv_s =
                    implied_vol_with_status(short_mid_rs, forward, k_short, t_years, kind, r);
                let iv_l = implied_vol_with_status(long_mid_rs, forward, k_long, t_years, kind, r);
                if !iv_s.reliable || !iv_l.reliable {
                    continue;
                }
                evaluated_pairs += 1;

                let gs = compute_greeks(forward, k_short, t_years, iv_s.iv, kind, r);
                let gl = compute_greeks(forward, k_long, t_years, iv_l.iv, kind, r);

                let hold_charges =
                    fees.compute_charges(short_entry_rs, long_entry_rs, 0.0, 0.0, qty, 2);
                let stop_charges =
                    fees.compute_charges(short_entry_rs, long_entry_rs, width_pts, 0.0, qty, 4);
                let q = f64::from(qty);
                let max_profit_net_rs = credit * q - hold_charges.total_charges_rs;
                let max_loss_net_rs = (width_pts - credit) * q + stop_charges.total_charges_rs;

                let breakeven_net_rs = match kind {
                    OptionKind::Call => k_short + credit - hold_charges.total_charges_rs / q,
                    OptionKind::Put => k_short - credit + hold_charges.total_charges_rs / q,
                };
                let cushion_pts = match kind {
                    OptionKind::Call => k_short - forward,
                    OptionKind::Put => forward - k_short,
                };

                let sigma_be = iv_s.iv.max(1e-4);
                let vol_sqrt_t = sigma_be * t_years.sqrt();
                let d2_be = ((forward / breakeven_net_rs).ln()
                    - 0.5 * sigma_be * sigma_be * t_years)
                    / vol_sqrt_t;
                let pop_terminal_pct = match kind {
                    OptionKind::Call => norm_cdf(-d2_be) * 100.0,
                    OptionKind::Put => norm_cdf(d2_be) * 100.0,
                };

                let net_theta_pts_day = -gs.theta_per_day + gl.theta_per_day;
                let net_theta_rs_day = net_theta_pts_day * q;
                let credit_to_width_pct = (credit / width_pts) * 100.0;
                let abs_short_delta = gs.delta.abs();
                let otm_sigma_multiple = diagnostics
                    .horizon_1sigma_pts
                    .filter(|&h| h > 0.0)
                    .map(|h| cushion_pts / h);

                let oi_wall_backed = if !chain_has_oi {
                    true
                } else {
                    let s_oi = short_quote.open_interest.unwrap_or(0);
                    let near_wall = match kind {
                        OptionKind::Call => diagnostics
                            .max_call_oi_strike_rs
                            .is_some_and(|wall| k_short >= wall - 100.0),
                        OptionKind::Put => diagnostics
                            .max_put_oi_strike_rs
                            .is_some_and(|wall| k_short <= wall + 100.0),
                    };
                    (max_side_oi > 0 && (s_oi as f64) >= 0.30 * (max_side_oi as f64)) || near_wall
                };

                let passes_core = width_paise <= 25_000
                    && credit_to_width_pct >= 18.0
                    && net_theta_pts_day >= 1.5
                    && cushion_pts >= 40.0
                    && (0.16..=0.42).contains(&abs_short_delta)
                    && max_profit_net_rs > 0.0;

                let passes_high_pop = pop_terminal_pct >= 80.0
                    && otm_sigma_multiple.is_some_and(|m| m >= 0.85)
                    && credit_to_width_pct >= 10.0
                    && net_theta_pts_day >= 1.5
                    && (0.12..=0.24).contains(&abs_short_delta)
                    && max_profit_net_rs > 0.0
                    && oi_wall_backed;

                let edge_tier = if passes_core {
                    EdgeTier::CoreIncome
                } else if passes_high_pop {
                    EdgeTier::HighPop1Sd
                } else {
                    EdgeTier::BelowEdgeGate
                };

                let pop_frac = pop_terminal_pct / 100.0;
                let rr_ratio = max_profit_net_rs / max_loss_net_rs.max(1.0);
                let ev_norm = pop_frac * rr_ratio - (1.0 - pop_frac);
                let theta_yield_pct = (net_theta_pts_day / width_pts) * 100.0;
                let composite_score = match edge_tier {
                    EdgeTier::CoreIncome => {
                        40.0 * ev_norm
                            + 25.0 * (pop_terminal_pct / 100.0)
                            + 20.0 * (credit_to_width_pct / 35.0)
                            + 15.0 * (theta_yield_pct / 2.5).min(1.5)
                            + if oi_wall_backed { 3.0 } else { 0.0 }
                    }
                    EdgeTier::HighPop1Sd => {
                        35.0 * (pop_terminal_pct / 100.0)
                            + 25.0 * ev_norm
                            + 20.0 * (otm_sigma_multiple.unwrap_or(0.0) / 1.25).min(1.4)
                            + 12.0 * (credit_to_width_pct / 20.0)
                            + 8.0 * (theta_yield_pct / 2.0).min(1.5)
                            + if oi_wall_backed { 5.0 } else { 0.0 }
                    }
                    EdgeTier::BelowEdgeGate => {
                        30.0 * ev_norm
                            + 30.0 * (pop_terminal_pct / 100.0)
                            + 20.0 * (credit_to_width_pct / 30.0)
                    }
                };

                let candidate = CandidateSpread {
                    option_kind: kind,
                    short_contract: short_contract.clone(),
                    long_contract: long_contract.clone(),
                    short_quote: *short_quote,
                    long_quote: *long_quote,
                    touch_executable,
                    width_pts,
                    short_entry_rs,
                    long_entry_rs,
                    entry_credit_pts: credit,
                    credit_to_width_pct,
                    otm_cushion_pts: cushion_pts,
                    otm_sigma_multiple,
                    breakeven_net_rs,
                    pop_terminal_pct,
                    max_profit_net_rs,
                    max_loss_net_rs,
                    short_iv_pct: gs.iv_pct,
                    long_iv_pct: gl.iv_pct,
                    short_delta: gs.delta,
                    net_delta: -gs.delta + gl.delta,
                    net_theta_pts_day,
                    net_theta_rs_day,
                    oi_wall_backed,
                    edge_tier,
                    composite_score,
                };

                if edge_tier != EdgeTier::BelowEdgeGate {
                    qualified.push(candidate);
                } else {
                    rejected.push(candidate);
                }
            }
        }
    }

    qualified.sort_by(|a, b| b.composite_score.total_cmp(&a.composite_score));
    rejected.sort_by(|a, b| b.composite_score.total_cmp(&a.composite_score));
    rejected.truncate(3);

    Ok(RankOutcome {
        diagnostics,
        evaluated_pairs,
        recommended: qualified.first().cloned(),
        qualified,
        near_misses: rejected,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcoRegime {
    NoLossProfitLockOco,
    ProfitLockInfeasible,
    LossMinimizationOco,
}

impl OcoRegime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoLossProfitLockOco => "NO_LOSS_PROFIT_LOCK_OCO",
            Self::ProfitLockInfeasible => "PROFIT_LOCK_INFEASIBLE",
            Self::LossMinimizationOco => "LOSS_MINIMIZATION_OCO",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardianVerdict {
    CloseAllImmediately,
    HoldWithNoLossOco,
    HoldWithTightOcoOrBookProfit,
    HoldWithLossMinimizationOco,
    RollToNearExpiryOrHoldWithOco,
}

impl GuardianVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CloseAllImmediately => "CLOSE_ALL_IMMEDIATELY",
            Self::HoldWithNoLossOco => "HOLD_WITH_NO_LOSS_OCO",
            Self::HoldWithTightOcoOrBookProfit => "HOLD_WITH_TIGHT_OCO_OR_BOOK_PROFIT",
            Self::HoldWithLossMinimizationOco => "HOLD_WITH_LOSS_MINIMIZATION_OCO",
            Self::RollToNearExpiryOrHoldWithOco => "ROLL_TO_NEAR_EXPIRY_OR_HOLD_WITH_OCO",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OcoTriggers {
    pub lower_target_forward_rs: f64,
    pub upper_stop_forward_rs: f64,
    pub short_lower_trigger_rs: f64,
    pub short_lower_limit_rs: f64,
    pub short_upper_trigger_rs: f64,
    pub short_upper_limit_rs: f64,
    pub long_lower_trigger_rs: f64,
    pub long_lower_limit_rs: f64,
    pub long_upper_trigger_rs: f64,
    pub long_upper_limit_rs: f64,
    pub h_min_after_short_stop_fill_rs: f64,
    pub zero_loss_floor_feasible: bool,
    pub worst_upper_limit_net_pnl_rs: f64,
    pub normal_lower_net_pnl_rs: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveSpreadEvaluation {
    pub option_kind: OptionKind,
    pub forward_rs: f64,
    pub short_strike_rs: f64,
    pub long_strike_rs: f64,
    pub qty: u32,
    pub entry_credit_pts: f64,
    pub exec_exit_debit_pts: f64,
    pub touch_executable: bool,
    pub gross_pnl_rs: f64,
    pub charges: ChargeBreakdown,
    pub net_pnl_rs: f64,
    pub max_profit_net_rs: f64,
    pub max_loss_net_rs: f64,
    pub effective_loss_cap_rs: f64,
    pub target_net_rs: f64,
    pub breakeven_net_rs: f64,
    pub otm_cushion_pts: f64,
    pub short_greeks: Option<Greeks>,
    pub long_greeks: Option<Greeks>,
    pub net_delta: Option<f64>,
    pub net_theta_rs_day: Option<f64>,
    pub oco_regime: OcoRegime,
    pub verdict: GuardianVerdict,
    pub kill_reasons: Vec<&'static str>,
    pub oco: Option<OcoTriggers>,
}

#[derive(Debug, Clone, Copy)]
pub struct LiveEvalInput {
    pub forward_rs: f64,
    pub spot_rs: f64,
    pub t_days: f64,
    pub option_kind: OptionKind,
    pub short_strike_rs: f64,
    pub long_strike_rs: f64,
    pub short_entry_rs: f64,
    pub long_entry_rs: f64,
    pub short_quote: MarketQuote,
    pub long_quote: MarketQuote,
    pub qty: u32,
    pub target_net_override_rs: Option<f64>,
    pub max_loss_override_rs: Option<f64>,
    pub invalidation_spot_level_rs: Option<f64>,
    pub intraday_exit_due: bool,
    /// When `false` (live guardian), both legs MUST have valid executable bid/ask touch quotes;
    /// when `true` (read-only `--status` outside market hours), LTP/prev-close fallback is permitted.
    pub allow_indicative_quotes: bool,
    pub fees: FeeSchedule,
}

pub fn evaluate_live_spread(input: LiveEvalInput) -> Result<LiveSpreadEvaluation, &'static str> {
    if input.qty == 0
        || !input.forward_rs.is_finite()
        || input.forward_rs <= 0.0
        || input.short_entry_rs <= 0.0
        || input.long_entry_rs <= 0.0
    {
        return Err("Invalid live spread parameters.");
    }
    let width_pts = match input.option_kind {
        OptionKind::Call => input.long_strike_rs - input.short_strike_rs,
        OptionKind::Put => input.short_strike_rs - input.long_strike_rs,
    };
    if width_pts <= 0.0 {
        return Err("Invalid credit spread strike ordering.");
    }
    let raw_credit_pts = input.short_entry_rs - input.long_entry_rs;
    if !raw_credit_pts.is_finite() || raw_credit_pts <= 0.0 || raw_credit_pts >= width_pts {
        return Err("Cannot calculate spread P&L from invalid entry credit.");
    }
    let entry_credit_pts = raw_credit_pts;

    // Executable touch exit: buy short at ask, sell long at bid.
    let (short_exit_rs, long_exit_rs, short_mid_rs, long_mid_rs, touch_executable) = match (
        input.short_quote.executable_quote(),
        input.long_quote.executable_quote(),
    ) {
        (Ok(sq), Ok(lq)) => (
            paise_to_rupees(sq.ask),
            paise_to_rupees(lq.bid),
            paise_to_rupees(sq.bid + sq.ask) * 0.5,
            paise_to_rupees(lq.bid + lq.ask) * 0.5,
            true,
        ),
        _ if input.allow_indicative_quotes => {
            let (sp, _) = input
                .short_quote
                .reference_price_paise(true)
                .ok_or("Short leg has no usable quote.")?;
            let (lp, _) = input
                .long_quote
                .reference_price_paise(true)
                .ok_or("Long leg has no usable quote.")?;
            let s_rs = paise_to_rupees(sp);
            let l_rs = paise_to_rupees(lp);
            (s_rs, l_rs, s_rs, l_rs, false)
        }
        _ => {
            return Err("Live spread quotes are missing executable bid/ask depth.");
        }
    };

    let q = f64::from(input.qty);
    let exec_exit_debit_pts = short_exit_rs - long_exit_rs;
    let charges = input.fees.compute_charges(
        input.short_entry_rs,
        input.long_entry_rs,
        short_exit_rs,
        long_exit_rs,
        input.qty,
        4,
    );
    let gross_pnl_rs = (entry_credit_pts - exec_exit_debit_pts) * q;
    let net_pnl_rs = gross_pnl_rs - charges.total_charges_rs;

    let hold_charges = input.fees.compute_charges(
        input.short_entry_rs,
        input.long_entry_rs,
        0.0,
        0.0,
        input.qty,
        2,
    );
    let stop_charges = input.fees.compute_charges(
        input.short_entry_rs,
        input.long_entry_rs,
        width_pts,
        0.0,
        input.qty,
        4,
    );
    let max_profit_net_rs = entry_credit_pts * q - hold_charges.total_charges_rs;
    let max_loss_net_rs = (width_pts - entry_credit_pts) * q + stop_charges.total_charges_rs;
    let effective_loss_cap_rs = input
        .max_loss_override_rs
        .unwrap_or_else(|| max_loss_net_rs.min((0.35 * max_loss_net_rs).max(1_500.0)));
    let target_net_rs = input.target_net_override_rs.unwrap_or_else(|| {
        let reachable_floor = 500.0_f64.min(0.85 * max_profit_net_rs.max(0.0));
        (0.50 * max_profit_net_rs.max(0.0)).max(reachable_floor)
    });

    let breakeven_net_rs = match input.option_kind {
        OptionKind::Call => {
            input.short_strike_rs + entry_credit_pts - hold_charges.total_charges_rs / q
        }
        OptionKind::Put => {
            input.short_strike_rs - entry_credit_pts + hold_charges.total_charges_rs / q
        }
    };
    let otm_cushion_pts = match input.option_kind {
        OptionKind::Call => input.short_strike_rs - input.forward_rs,
        OptionKind::Put => input.forward_rs - input.short_strike_rs,
    };

    let t_years = (input.t_days / 365.0).max(1e-6);
    let r = DEFAULT_RISK_FREE_RATE;
    let iv_s = implied_vol_with_status(
        short_mid_rs,
        input.forward_rs,
        input.short_strike_rs,
        t_years,
        input.option_kind,
        r,
    );
    let iv_l = implied_vol_with_status(
        long_mid_rs,
        input.forward_rs,
        input.long_strike_rs,
        t_years,
        input.option_kind,
        r,
    );

    let short_greeks = iv_s.reliable.then(|| {
        compute_greeks(
            input.forward_rs,
            input.short_strike_rs,
            t_years,
            iv_s.iv,
            input.option_kind,
            r,
        )
    });
    let long_greeks = iv_l.reliable.then(|| {
        compute_greeks(
            input.forward_rs,
            input.long_strike_rs,
            t_years,
            iv_l.iv,
            input.option_kind,
            r,
        )
    });
    let net_delta = match (short_greeks, long_greeks) {
        (Some(gs), Some(gl)) => Some(-gs.delta + gl.delta),
        _ => None,
    };
    let net_theta_rs_day = match (short_greeks, long_greeks) {
        (Some(gs), Some(gl)) => Some((-gs.theta_per_day + gl.theta_per_day) * q),
        _ => None,
    };

    // Priority-1 Kill Switches: MUST run safely even when IV fails to converge!
    let daily_1sigma_pts = iv_s
        .reliable
        .then(|| input.forward_rs * iv_s.iv / 365.0_f64.sqrt());
    let gamma_danger_cushion = daily_1sigma_pts.map_or(15.0, |s| (0.22 * s).max(15.0));

    let mut kill_reasons = Vec::new();
    if net_pnl_rs >= target_net_rs {
        kill_reasons.push("profit_target_reached");
    }
    if net_pnl_rs <= -effective_loss_cap_rs {
        kill_reasons.push("max_loss_cap_breached");
    }
    if otm_cushion_pts <= 0.0 {
        kill_reasons.push("short_strike_breached");
    } else if short_greeks.is_some_and(|g| g.delta.abs() >= 0.48) {
        kill_reasons.push("delta_danger_zone");
    } else if otm_cushion_pts <= gamma_danger_cushion && net_pnl_rs < 0.0 {
        kill_reasons.push("pre_strike_gamma_zone");
    }
    if let Some(inv) = input.invalidation_spot_level_rs {
        let breached = match input.option_kind {
            OptionKind::Call => input.spot_rs >= inv || input.forward_rs >= inv,
            OptionKind::Put => input.spot_rs <= inv || input.forward_rs <= inv,
        };
        if breached {
            kill_reasons.push("structural_invalidation_breached");
        }
    }
    if input.intraday_exit_due {
        kill_reasons.push("intraday_square_off_window");
    }

    let e_zero = entry_credit_pts - charges.total_charges_rs / q;
    let e_target_profit =
        (entry_credit_pts - (target_net_rs + charges.total_charges_rs) / q).max(0.50);
    let e_cap = (entry_credit_pts + (effective_loss_cap_rs - charges.total_charges_rs) / q)
        .min(width_pts - 0.50);

    // Determine OCO triggers when IVs are reliable.
    let (oco_regime, oco) = if let (true, true) = (iv_s.reliable, iv_l.reliable) {
        if exec_exit_debit_pts < e_zero {
            let profit_cushion_pts = e_zero - exec_exit_debit_pts;
            let s_buf = round_to_tick((0.25 * profit_cushion_pts).clamp(0.50, 2.50), 0.05);
            let l_buf = round_to_tick((0.15 * profit_cushion_pts).clamp(0.25, 1.50), 0.05);
            let min_buffer_needed = s_buf + l_buf + 0.50;
            let base_stressed_008 = (compute_greeks(
                input.forward_rs,
                input.short_strike_rs,
                t_years,
                iv_s.iv + 0.008,
                input.option_kind,
                r,
            )
            .price
                - compute_greeks(
                    input.forward_rs,
                    input.long_strike_rs,
                    t_years,
                    iv_l.iv + 0.008 * 0.85,
                    input.option_kind,
                    r,
                )
                .price)
                .max(0.05);
            let min_adverse_upper = exec_exit_debit_pts.max(base_stressed_008) + 0.35;
            let no_loss_candidate_upper = e_zero - (s_buf + l_buf) - 0.25;
            let lower_solve = solve_forward_for_spread_value(
                e_target_profit,
                input.forward_rs,
                t_years,
                input.option_kind,
                input.short_strike_rs,
                iv_s.iv,
                input.long_strike_rs,
                iv_l.iv,
                0.0,
                r,
            );
            let (regime, e_upper, s_up_buf, l_up_buf) = if profit_cushion_pts >= min_buffer_needed
                && no_loss_candidate_upper >= min_adverse_upper
            {
                (
                    OcoRegime::NoLossProfitLockOco,
                    no_loss_candidate_upper,
                    s_buf,
                    l_buf,
                )
            } else {
                let tight = (e_zero + 3.0)
                    .min(e_cap)
                    .max(min_adverse_upper)
                    .min(width_pts - 0.25);
                (OcoRegime::ProfitLockInfeasible, tight, 2.50, 1.50)
            };
            let upper_solve = solve_forward_for_spread_value(
                e_upper,
                input.forward_rs,
                t_years,
                input.option_kind,
                input.short_strike_rs,
                iv_s.iv,
                input.long_strike_rs,
                iv_l.iv,
                0.008,
                r,
            );
            let triggers = build_oco_triggers(
                &input,
                &lower_solve,
                &upper_solve,
                s_up_buf,
                l_up_buf,
                regime == OcoRegime::NoLossProfitLockOco,
            );
            let upgraded =
                if regime == OcoRegime::NoLossProfitLockOco && !triggers.zero_loss_floor_feasible {
                    OcoRegime::ProfitLockInfeasible
                } else {
                    regime
                };
            (upgraded, Some(triggers))
        } else {
            let e_lower_scratch = e_zero.max(0.50);
            let lower_solve = solve_forward_for_spread_value(
                e_lower_scratch,
                input.forward_rs,
                t_years,
                input.option_kind,
                input.short_strike_rs,
                iv_s.iv,
                input.long_strike_rs,
                iv_l.iv,
                -0.003,
                r,
            );
            let f_gamma_wall = match input.option_kind {
                OptionKind::Call => input.short_strike_rs - gamma_danger_cushion,
                OptionKind::Put => input.short_strike_rs + gamma_danger_cushion,
            };
            let base_stressed_010 = (compute_greeks(
                input.forward_rs,
                input.short_strike_rs,
                t_years,
                iv_s.iv + 0.01,
                input.option_kind,
                r,
            )
            .price
                - compute_greeks(
                    input.forward_rs,
                    input.long_strike_rs,
                    t_years,
                    iv_l.iv + 0.01,
                    input.option_kind,
                    r,
                )
                .price)
                .max(0.05);
            let e_at_gamma = compute_greeks(
                f_gamma_wall,
                input.short_strike_rs,
                t_years,
                iv_s.iv + 0.01,
                input.option_kind,
                r,
            )
            .price
                - compute_greeks(
                    f_gamma_wall,
                    input.long_strike_rs,
                    t_years,
                    iv_l.iv + 0.01,
                    input.option_kind,
                    r,
                )
                .price;
            // Safe bounded stop debit (never panics even if exec_exit_debit_pts already exceeds e_cap).
            let min_adverse_upper = exec_exit_debit_pts.max(base_stressed_010) + 1.0;
            let e_upper = if e_cap > min_adverse_upper {
                e_cap.min(e_at_gamma).max(min_adverse_upper)
            } else {
                e_cap.max(min_adverse_upper).min(width_pts - 0.25)
            };
            let upper_solve = solve_forward_for_spread_value(
                e_upper,
                input.forward_rs,
                t_years,
                input.option_kind,
                input.short_strike_rs,
                iv_s.iv,
                input.long_strike_rs,
                iv_l.iv,
                0.01,
                r,
            );
            let triggers = build_oco_triggers(&input, &lower_solve, &upper_solve, 4.0, 2.0, false);
            (OcoRegime::LossMinimizationOco, Some(triggers))
        }
    } else {
        let regime = if exec_exit_debit_pts < e_zero {
            OcoRegime::ProfitLockInfeasible
        } else {
            OcoRegime::LossMinimizationOco
        };
        (regime, None)
    };

    let verdict = if !kill_reasons.is_empty() {
        GuardianVerdict::CloseAllImmediately
    } else {
        let hedge_burn = match (short_greeks, long_greeks) {
            (Some(gs), Some(gl)) if gs.theta_per_day.abs() > 1e-9 => {
                gl.theta_per_day.abs() / gs.theta_per_day.abs()
            }
            _ => 0.0,
        };
        match oco_regime {
            OcoRegime::NoLossProfitLockOco if input.t_days > 20.0 && hedge_burn > 0.75 => {
                GuardianVerdict::RollToNearExpiryOrHoldWithOco
            }
            OcoRegime::NoLossProfitLockOco => GuardianVerdict::HoldWithNoLossOco,
            OcoRegime::ProfitLockInfeasible => GuardianVerdict::HoldWithTightOcoOrBookProfit,
            OcoRegime::LossMinimizationOco => GuardianVerdict::HoldWithLossMinimizationOco,
        }
    };

    Ok(LiveSpreadEvaluation {
        option_kind: input.option_kind,
        forward_rs: input.forward_rs,
        short_strike_rs: input.short_strike_rs,
        long_strike_rs: input.long_strike_rs,
        qty: input.qty,
        entry_credit_pts,
        exec_exit_debit_pts,
        touch_executable,
        gross_pnl_rs,
        charges,
        net_pnl_rs,
        max_profit_net_rs,
        max_loss_net_rs,
        effective_loss_cap_rs,
        target_net_rs,
        breakeven_net_rs,
        otm_cushion_pts,
        short_greeks,
        long_greeks,
        net_delta,
        net_theta_rs_day,
        oco_regime,
        verdict,
        kill_reasons,
        oco,
    })
}

fn build_oco_triggers(
    input: &LiveEvalInput,
    lower_solve: &crate::math::ForwardSolveResult,
    upper_solve: &crate::math::ForwardSolveResult,
    short_up_buf: f64,
    long_up_buf: f64,
    expect_zero_loss: bool,
) -> OcoTriggers {
    let short_lower_trigger = round_to_tick(lower_solve.short_model_price, 0.05).max(0.10);
    let short_lower_limit = round_to_tick(short_lower_trigger + 1.00, 0.05);
    let short_upper_trigger = round_to_tick(upper_solve.short_model_price, 0.05);
    let short_upper_limit = round_to_tick(short_upper_trigger + short_up_buf, 0.05);

    let long_lower_trigger = if lower_solve.status == SolveStatus::Solved {
        round_to_tick((lower_solve.long_model_price - 2.00).max(0.10), 0.05)
    } else {
        0.10
    };
    let long_lower_limit = round_to_tick((long_lower_trigger - 1.00).max(0.05), 0.05);
    let long_upper_trigger = round_to_tick(upper_solve.long_model_price + 2.00, 0.05);
    let long_upper_limit = round_to_tick((long_upper_trigger - long_up_buf).max(0.05), 0.05);

    let q = f64::from(input.qty);
    let entry_credit = input.short_entry_rs - input.long_entry_rs;

    let ch_norm_low = input.fees.compute_charges(
        input.short_entry_rs,
        input.long_entry_rs,
        short_lower_trigger,
        lower_solve.long_model_price,
        input.qty,
        4,
    );
    let normal_lower_net_pnl_rs =
        (entry_credit - (short_lower_trigger - lower_solve.long_model_price)) * q
            - ch_norm_low.total_charges_rs;

    let ch_worst_up = input.fees.compute_charges(
        input.short_entry_rs,
        input.long_entry_rs,
        short_upper_limit,
        long_upper_limit,
        input.qty,
        4,
    );
    let worst_upper_limit_net_pnl_rs =
        (entry_credit - (short_upper_limit - long_upper_limit)) * q - ch_worst_up.total_charges_rs;
    let zero_loss_floor_feasible = expect_zero_loss && worst_upper_limit_net_pnl_rs >= 0.0;

    let floor_rs = if zero_loss_floor_feasible {
        0.0
    } else {
        worst_upper_limit_net_pnl_rs
    };
    let h_min = round_to_tick(
        (short_upper_limit - entry_credit + (ch_worst_up.total_charges_rs + floor_rs) / q)
            .max(0.05),
        0.05,
    );

    OcoTriggers {
        lower_target_forward_rs: lower_solve.implied_forward,
        upper_stop_forward_rs: upper_solve.implied_forward,
        short_lower_trigger_rs: short_lower_trigger,
        short_lower_limit_rs: short_lower_limit,
        short_upper_trigger_rs: short_upper_trigger,
        short_upper_limit_rs: short_upper_limit,
        long_lower_trigger_rs: long_lower_trigger,
        long_lower_limit_rs: long_lower_limit,
        long_upper_trigger_rs: long_upper_trigger,
        long_upper_limit_rs: long_upper_limit,
        h_min_after_short_stop_fill_rs: h_min,
        zero_loss_floor_feasible,
        worst_upper_limit_net_pnl_rs,
        normal_lower_net_pnl_rs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::rupees_to_paise;
    use crate::math::black76_price;
    use time::{Date, Month};

    fn synthetic_chain(
        forward: f64,
        t_days: f64,
        iv_base: f64,
    ) -> Vec<(OptionContract, MarketQuote)> {
        let expiry = Date::from_calendar_date(2026, Month::December, 1).unwrap();
        let t_years = t_days / 365.0;
        let mut out = Vec::new();
        let mut tok = 1000_u32;
        for step in (-10_i32..=10).map(|i| i * 100) {
            let strike_rs = forward + f64::from(step);
            let strike_paise = rupees_to_paise(strike_rs).unwrap();
            for kind in [OptionKind::Call, OptionKind::Put] {
                tok += 1;
                let skew = match kind {
                    OptionKind::Put if step < 0 => 0.015,
                    OptionKind::Call if step > 0 => -0.005,
                    _ => 0.0,
                };
                let theo = black76_price(
                    forward,
                    strike_rs,
                    t_years,
                    (iv_base + skew).max(0.08),
                    kind,
                    DEFAULT_RISK_FREE_RATE,
                )
                .max(1.0);
                let mid_paise = rupees_to_paise(theo).unwrap();
                let half_spread = 15_i64;
                let oi = if step.abs() == 300 {
                    5_000_000
                } else {
                    1_200_000
                };
                let contract = OptionContract {
                    exchange: "NFO".to_owned(),
                    token: tok.to_string(),
                    trading_symbol: format!(
                        "NIFTY01DEC26{}{}",
                        if kind == OptionKind::Call { "C" } else { "P" },
                        strike_rs as i64
                    ),
                    underlying: "NIFTY".to_owned(),
                    expiry,
                    option_kind: kind,
                    strike_paise,
                    lot_size: 65,
                    tick_paise: 5,
                };
                let quote = MarketQuote {
                    ltp_paise: Some(mid_paise),
                    prev_close_paise: Some(mid_paise),
                    bid_paise: Some((mid_paise - half_spread).max(5)),
                    ask_paise: Some(mid_paise + half_spread),
                    bid_qty: 650,
                    ask_qty: 650,
                    open_interest: Some(oi),
                    prev_open_interest: Some(oi - 50_000),
                    volume: Some(100_000),
                    lower_circuit_paise: Some(5),
                    upper_circuit_paise: Some(mid_paise * 3),
                    feed_epoch_secs: Some(1_795_450_000),
                };
                out.push((contract, quote));
            }
        }
        out
    }

    #[test]
    fn strategy_bias_parses_all_aliases() {
        assert_eq!("auto".parse(), Ok(StrategyBias::Auto));
        assert_eq!("bear_call".parse(), Ok(StrategyBias::BearCall));
        assert_eq!("bull_put".parse(), Ok(StrategyBias::BullPut));
        assert!("unknown".parse::<StrategyBias>().is_err());
    }

    #[test]
    fn chain_analysis_and_ranker_find_qualifying_spreads_and_gate_low_edge() {
        let chain = synthetic_chain(25_400.0, 6.0, 0.16);
        let ranked = rank_candidate_spreads(
            25_390.0,
            6.0,
            StrategyBias::Auto,
            1,
            &chain,
            FeeSchedule::shoonya_2026(),
            true,
        )
        .unwrap();
        assert!(ranked.diagnostics.parity_used);
        assert!(ranked.diagnostics.atm_iv_pct.unwrap() > 10.0);
        assert!(ranked.diagnostics.chain_pcr_oi.is_some());
        assert!(ranked.diagnostics.put_call_skew_25d_pct.is_some());
        assert!(ranked.recommended.is_some());
        let best = ranked.recommended.unwrap();
        assert!(best.touch_executable);
        assert!(best.max_profit_net_rs > 0.0);
        assert_ne!(best.edge_tier.as_str(), "BELOW_EDGE_GATE");

        // Far-dated ultra-low-vol chain fails theta/credit gates -> NO_POSITION_RECOMMENDED.
        let dull_chain = synthetic_chain(25_400.0, 60.0, 0.06);
        let dull = rank_candidate_spreads(
            25_400.0,
            60.0,
            StrategyBias::BearCall,
            1,
            &dull_chain,
            FeeSchedule::shoonya_2026(),
            true,
        )
        .unwrap();
        assert!(dull.recommended.is_none());

        // After-hours LTP-only scan (require_executable_touch = false) still ranks indicative candidates.
        let ltp_only: Vec<(OptionContract, MarketQuote)> = chain
            .into_iter()
            .map(|(c, mut q)| {
                q.bid_paise = None;
                q.ask_paise = None;
                (c, q)
            })
            .collect();
        let indicative = rank_candidate_spreads(
            25_390.0,
            6.0,
            StrategyBias::BullPut,
            1,
            &ltp_only,
            FeeSchedule::zerodha_2026(),
            false,
        )
        .unwrap();
        assert!(indicative.evaluated_pairs > 0);

        // Wide bid/ask (>25% relative spread) is skipped when require_executable_touch = true,
        // and marked touch_executable = false when require_executable_touch = false.
        let wide_quotes: Vec<(OptionContract, MarketQuote)> = ltp_only
            .into_iter()
            .map(|(c, mut q)| {
                let ltp = q.ltp_paise.unwrap_or(5_000);
                q.bid_paise = Some((ltp * 70 / 100).max(5));
                q.ask_paise = Some(ltp * 130 / 100);
                (c, q)
            })
            .collect();
        let wide_strict = rank_candidate_spreads(
            25_390.0,
            6.0,
            StrategyBias::BullPut,
            1,
            &wide_quotes,
            FeeSchedule::zerodha_2026(),
            true,
        )
        .unwrap();
        assert_eq!(wide_strict.evaluated_pairs, 0);
        let wide_indicative = rank_candidate_spreads(
            25_390.0,
            6.0,
            StrategyBias::BullPut,
            1,
            &wide_quotes,
            FeeSchedule::zerodha_2026(),
            false,
        )
        .unwrap();
        assert!(wide_indicative.evaluated_pairs > 0);
    }

    #[test]
    fn live_evaluator_covers_all_three_regimes_and_non_converged_iv_kill_switches() {
        let fees = FeeSchedule::shoonya_2026();
        let make_quote = |bid_rs: f64, ask_rs: f64| MarketQuote {
            ltp_paise: rupees_to_paise((bid_rs + ask_rs) * 0.5),
            bid_paise: rupees_to_paise(bid_rs),
            ask_paise: rupees_to_paise(ask_rs),
            ..MarketQuote::default()
        };

        // 1. Regime A1: NO_LOSS_PROFIT_LOCK_OCO (healthy cushion, not yet at target).
        let base = LiveEvalInput {
            forward_rs: 25_200.0,
            spot_rs: 25_190.0,
            t_days: 7.0,
            option_kind: OptionKind::Call,
            short_strike_rs: 25_500.0,
            long_strike_rs: 25_700.0,
            short_entry_rs: 110.0,
            long_entry_rs: 50.0,
            short_quote: make_quote(64.0, 65.0),
            long_quote: make_quote(25.0, 26.0),
            qty: 65,
            target_net_override_rs: Some(2_500.0),
            max_loss_override_rs: None,
            invalidation_spot_level_rs: Some(25_480.0),
            intraday_exit_due: false,
            allow_indicative_quotes: false,
            fees,
        };
        let eval_a1 = evaluate_live_spread(base).unwrap();
        assert_eq!(eval_a1.oco_regime, OcoRegime::NoLossProfitLockOco);
        assert_eq!(eval_a1.verdict, GuardianVerdict::HoldWithNoLossOco);
        assert!(eval_a1.oco.as_ref().unwrap().zero_loss_floor_feasible);
        assert_eq!(eval_a1.oco_regime.as_str(), "NO_LOSS_PROFIT_LOCK_OCO");
        assert_eq!(eval_a1.verdict.as_str(), "HOLD_WITH_NO_LOSS_OCO");

        // 2. Regime A2: PROFIT_LOCK_INFEASIBLE (tiny profit cushion).
        let eval_a2 = evaluate_live_spread(LiveEvalInput {
            short_quote: make_quote(97.0, 98.0),
            long_quote: make_quote(39.0, 40.0),
            ..base
        })
        .unwrap();
        assert_eq!(eval_a2.oco_regime, OcoRegime::ProfitLockInfeasible);
        assert_eq!(
            eval_a2.verdict,
            GuardianVerdict::HoldWithTightOcoOrBookProfit
        );

        // 3. Regime B: LOSS_MINIMIZATION_OCO (moderately underwater inside cap).
        let eval_b = evaluate_live_spread(LiveEvalInput {
            forward_rs: 25_320.0,
            short_quote: make_quote(135.0, 136.0),
            long_quote: make_quote(64.0, 65.0),
            ..base
        })
        .unwrap();
        assert_eq!(eval_b.oco_regime, OcoRegime::LossMinimizationOco);
        assert_eq!(eval_b.verdict, GuardianVerdict::HoldWithLossMinimizationOco);

        // 2b. Bull Put spread + Loss-cap breached WITH valid convergent IVs (never panics!)
        // + far-dated hedge-burn roll verdict.
        let put_eval = evaluate_live_spread(LiveEvalInput {
            forward_rs: 25_200.0,
            spot_rs: 25_200.0,
            t_days: 30.0,
            option_kind: OptionKind::Put,
            short_strike_rs: 25_000.0,
            long_strike_rs: 24_800.0,
            short_entry_rs: 180.0,
            long_entry_rs: 120.0,
            short_quote: make_quote(110.0, 111.0),
            long_quote: make_quote(88.0, 89.0),
            qty: 65,
            target_net_override_rs: Some(3_000.0),
            max_loss_override_rs: None,
            invalidation_spot_level_rs: Some(24_950.0),
            intraday_exit_due: false,
            allow_indicative_quotes: false,
            fees,
        })
        .unwrap();
        assert!(matches!(
            put_eval.verdict,
            GuardianVerdict::RollToNearExpiryOrHoldWithOco | GuardianVerdict::HoldWithNoLossOco
        ));

        let put_cap_breach = evaluate_live_spread(LiveEvalInput {
            forward_rs: 24_980.0,
            spot_rs: 24_940.0,
            t_days: 7.0,
            option_kind: OptionKind::Put,
            short_strike_rs: 25_000.0,
            long_strike_rs: 24_800.0,
            short_entry_rs: 90.0,
            long_entry_rs: 40.0,
            short_quote: make_quote(175.0, 176.0),
            long_quote: make_quote(70.0, 71.0),
            qty: 65,
            target_net_override_rs: None,
            max_loss_override_rs: Some(1_500.0),
            invalidation_spot_level_rs: Some(24_950.0),
            intraday_exit_due: true,
            allow_indicative_quotes: false,
            fees,
        })
        .unwrap();
        assert_eq!(put_cap_breach.verdict, GuardianVerdict::CloseAllImmediately);
        assert!(
            put_cap_breach
                .kill_reasons
                .contains(&"max_loss_cap_breached")
        );
        assert!(
            put_cap_breach
                .kill_reasons
                .contains(&"structural_invalidation_breached")
        );

        // 4. Priority-1 Kill Switch even when IV fails to converge (t_days near 0 / intrinsic violation)!
        let eval_kill = evaluate_live_spread(LiveEvalInput {
            forward_rs: 25_550.0,
            spot_rs: 25_550.0,
            t_days: 1e-12,
            short_quote: make_quote(180.0, 182.0),
            long_quote: make_quote(20.0, 21.0),
            intraday_exit_due: true,
            ..base
        })
        .unwrap();
        assert_eq!(eval_kill.verdict, GuardianVerdict::CloseAllImmediately);
        assert!(eval_kill.kill_reasons.contains(&"max_loss_cap_breached"));
        assert!(eval_kill.kill_reasons.contains(&"short_strike_breached"));
        assert!(
            eval_kill
                .kill_reasons
                .contains(&"structural_invalidation_breached")
        );
        assert!(
            eval_kill
                .kill_reasons
                .contains(&"intraday_square_off_window")
        );
    }
}
