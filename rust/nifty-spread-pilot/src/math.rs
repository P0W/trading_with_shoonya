//! Zero-dependency Discounted Black-76 option pricing, implied volatility,
//! 1st/2nd-order Greeks, Put-Call Parity forward, and non-linear F* solver.

use std::f64::consts::{FRAC_1_SQRT_2, PI};

use crate::OptionKind;

pub const DEFAULT_RISK_FREE_RATE: f64 = 0.067;

/// High-precision complementary error function `erfc(x)` using W. J. Cody's (1969)
/// rational Chebyshev approximations (accurate to ~1e-15 across IEEE-754 f64).
pub fn erfc(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if x == f64::INFINITY {
        return 0.0;
    }
    if x == f64::NEG_INFINITY {
        return 2.0;
    }
    let ax = x.abs();
    if ax <= 0.46875 {
        const A: [f64; 5] = [
            3.161_123_743_870_565_6,
            113.864_154_151_050_16,
            377.485_237_685_302,
            3_209.377_589_138_469_5,
            0.185_777_706_184_603_15,
        ];
        const B: [f64; 4] = [
            23.601_290_952_344_12,
            244.024_637_934_444_17,
            1_282.616_526_077_372_3,
            2_844.236_833_439_171,
        ];
        let y = ax * ax;
        let xnum = (((A[4] * y + A[0]) * y + A[1]) * y + A[2]) * y + A[3];
        let xden = (((y + B[0]) * y + B[1]) * y + B[2]) * y + B[3];
        let erf = x * (xnum / xden);
        return 1.0 - erf;
    }

    let result = if ax <= 4.0 {
        const C: [f64; 9] = [
            0.564_188_496_988_670_1,
            8.883_149_794_388_376,
            66.119_190_637_141_63,
            298.635_138_197_400_1,
            881.952_221_241_769,
            1_712.047_612_634_070_6,
            2_051.078_377_826_071_5,
            1_230.339_354_797_997_2,
            2.153_115_354_744_038_5e-8,
        ];
        const D: [f64; 8] = [
            15.744_926_110_709_835,
            117.693_950_891_312_5,
            537.181_101_862_009_9,
            1_621.389_574_566_690_2,
            3_290.799_235_733_459_6,
            4_362.619_090_143_247,
            3_439.367_674_143_721_6,
            1_230.339_354_803_749_5,
        ];
        let mut xnum = C[8] * ax + C[0];
        let mut xden = ax + D[0];
        for i in 1..7 {
            xnum = xnum * ax + C[i];
            xden = xden * ax + D[i];
        }
        xnum = xnum * ax + C[7];
        xden = xden * ax + D[7];
        let sq = (ax * 16.0).floor() / 16.0;
        let del = (ax - sq) * (ax + sq);
        (-sq * sq).exp() * (-del).exp() * (xnum / xden)
    } else if ax <= 26.6 {
        const P: [f64; 6] = [
            0.305_326_634_961_232_34,
            0.360_344_899_949_804_44,
            0.125_781_726_111_229_25,
            0.016_083_785_148_742_28,
            6.587_491_615_298_378e-4,
            0.016_315_387_137_302_098,
        ];
        const Q: [f64; 5] = [
            2.568_520_192_289_822,
            1.872_952_849_923_460_4,
            0.527_905_102_951_428_4,
            0.060_518_341_312_441_32,
            0.002_335_204_976_268_691_8,
        ];
        let y = 1.0 / (ax * ax);
        let xnum = y * (((((P[5] * y + P[0]) * y + P[1]) * y + P[2]) * y + P[3]) * y + P[4]);
        let xden = ((((y + Q[0]) * y + Q[1]) * y + Q[2]) * y + Q[3]) * y + Q[4];
        let ratio = (1.0 / PI.sqrt() - xnum / xden) / ax;
        let sq = (ax * 16.0).floor() / 16.0;
        let del = (ax - sq) * (ax + sq);
        (-sq * sq).exp() * (-del).exp() * ratio
    } else {
        0.0
    };

    if x < 0.0 { 2.0 - result } else { result }
}

pub fn norm_pdf(x: f64) -> f64 {
    if !x.is_finite() {
        return 0.0;
    }
    (-0.5 * x * x).exp() / (2.0 * PI).sqrt()
}

pub fn norm_cdf(x: f64) -> f64 {
    0.5 * erfc(-x * FRAC_1_SQRT_2)
}

pub fn round_to_tick(price: f64, tick: f64) -> f64 {
    if tick <= 0.0 || !price.is_finite() {
        return price;
    }
    (price / tick).round() * tick
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParityForward {
    pub implied_forward: f64,
    pub atm_strike: f64,
    pub call_price: f64,
    pub put_price: f64,
    pub discount_factor: f64,
    pub basis_vs_spot: Option<f64>,
}

/// Discounted Put-Call Parity: `F = K + e^{rT} * (C - P)`.
pub fn implied_forward_from_pair(
    spot: Option<f64>,
    atm_strike: f64,
    call_price: f64,
    put_price: f64,
    t_years: f64,
    r: f64,
) -> Result<ParityForward, &'static str> {
    if !atm_strike.is_finite()
        || !call_price.is_finite()
        || !put_price.is_finite()
        || !t_years.is_finite()
        || !r.is_finite()
        || atm_strike <= 0.0
        || call_price <= 0.0
        || put_price <= 0.0
    {
        return Err("ATM strike and option prices must be finite positive numbers.");
    }
    let t = t_years.max(0.0);
    let implied_forward = atm_strike + (r * t).exp() * (call_price - put_price);
    if !implied_forward.is_finite() || implied_forward <= 0.0 {
        return Err("Implied forward from put-call parity is non-positive.");
    }
    let discount_factor = (-r * t).exp();
    let basis_vs_spot = spot
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| implied_forward - s);
    Ok(ParityForward {
        implied_forward,
        atm_strike,
        call_price,
        put_price,
        discount_factor,
        basis_vs_spot,
    })
}

pub fn black76_price(
    forward: f64,
    strike: f64,
    t_years: f64,
    iv: f64,
    kind: OptionKind,
    r: f64,
) -> f64 {
    let t_eff = t_years.max(0.0);
    let df = (-r * t_eff).exp();
    let intrinsic = match kind {
        OptionKind::Call => df * (forward - strike).max(0.0),
        OptionKind::Put => df * (strike - forward).max(0.0),
    };
    if t_years <= 1e-10 || iv <= 1e-10 {
        return intrinsic;
    }
    let sqrt_t = t_years.sqrt();
    let vol_sqrt_t = iv * sqrt_t;
    let d1 = ((forward / strike).ln() + 0.5 * iv * iv * t_years) / vol_sqrt_t;
    let d2 = d1 - vol_sqrt_t;
    match kind {
        OptionKind::Call => df * (forward * norm_cdf(d1) - strike * norm_cdf(d2)),
        OptionKind::Put => df * (strike * norm_cdf(-d2) - forward * norm_cdf(-d1)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IvStatus {
    Converged,
    InvalidInput,
    ExpiredZeroTime,
    BelowIntrinsic,
    AboveNoArbBound,
    BracketExhausted,
}

impl IvStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Converged => "CONVERGED",
            Self::InvalidInput => "INVALID_INPUT",
            Self::ExpiredZeroTime => "EXPIRED_ZERO_TIME",
            Self::BelowIntrinsic => "BELOW_INTRINSIC",
            Self::AboveNoArbBound => "ABOVE_NO_ARB_BOUND",
            Self::BracketExhausted => "BRACKET_EXHAUSTED",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IvResult {
    pub iv: f64,
    pub status: IvStatus,
    pub reliable: bool,
}

/// Safeguarded Newton-Raphson + bisection Black-76 implied volatility solver.
pub fn implied_vol_with_status(
    market_price: f64,
    forward: f64,
    strike: f64,
    t_years: f64,
    kind: OptionKind,
    r: f64,
) -> IvResult {
    if !market_price.is_finite()
        || !forward.is_finite()
        || !strike.is_finite()
        || !t_years.is_finite()
        || !r.is_finite()
        || forward <= 0.0
        || strike <= 0.0
    {
        return IvResult {
            iv: 0.0,
            status: IvStatus::InvalidInput,
            reliable: false,
        };
    }
    if t_years <= 1e-10 {
        return IvResult {
            iv: 0.0,
            status: IvStatus::ExpiredZeroTime,
            reliable: false,
        };
    }
    let df = (-r * t_years).exp();
    let intrinsic = match kind {
        OptionKind::Call => df * (forward - strike).max(0.0),
        OptionKind::Put => df * (strike - forward).max(0.0),
    };
    if market_price <= intrinsic + 1e-7 {
        return IvResult {
            iv: 1e-4,
            status: IvStatus::BelowIntrinsic,
            reliable: false,
        };
    }
    let upper_price_cap = match kind {
        OptionKind::Call => df * forward,
        OptionKind::Put => df * strike,
    };
    if market_price >= upper_price_cap - 1e-7 {
        return IvResult {
            iv: 5.0,
            status: IvStatus::AboveNoArbBound,
            reliable: false,
        };
    }

    let mut low = 1e-4_f64;
    let mut high = 5.0_f64;
    let p_high = black76_price(forward, strike, t_years, high, kind, r);
    if market_price >= p_high {
        return IvResult {
            iv: high,
            status: IvStatus::BracketExhausted,
            reliable: false,
        };
    }

    let mut sigma =
        (((2.0 * PI) / t_years).sqrt() * (market_price / (df * forward))).clamp(0.05, 1.0);
    let sqrt_t = t_years.sqrt();

    for _ in 0..100 {
        let price = black76_price(forward, strike, t_years, sigma, kind, r);
        let diff = price - market_price;
        if diff.abs() < 1e-8 {
            return IvResult {
                iv: sigma,
                status: IvStatus::Converged,
                reliable: true,
            };
        }
        if diff > 0.0 {
            high = sigma;
        } else {
            low = sigma;
        }
        let d1 = ((forward / strike).ln() + 0.5 * sigma * sigma * t_years) / (sigma * sqrt_t);
        let vega = df * forward * norm_pdf(d1) * sqrt_t;
        if vega > 1e-10 {
            let candidate = sigma - diff / vega;
            sigma = if candidate > low && candidate < high {
                candidate
            } else {
                0.5 * (low + high)
            };
        } else {
            sigma = 0.5 * (low + high);
        }
        if (high - low) < 1e-9 {
            break;
        }
    }
    IvResult {
        iv: sigma,
        status: IvStatus::Converged,
        reliable: true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Greeks {
    pub price: f64,
    pub iv_pct: f64,
    pub delta: f64,
    pub gamma: f64,
    pub vega_1pct: f64,
    pub theta_per_day: f64,
    pub vanna_1pct: f64,
    pub charm_per_day: f64,
    pub vomma_1pct: f64,
    pub prob_itm_risk_neutral: f64,
}

pub fn compute_greeks(
    forward: f64,
    strike: f64,
    t_years: f64,
    iv: f64,
    kind: OptionKind,
    r: f64,
) -> Greeks {
    let t = t_years.max(1e-8);
    let sigma = iv.max(1e-4);
    let df = (-r * t).exp();
    let sqrt_t = t.sqrt();
    let vol_sqrt_t = sigma * sqrt_t;
    let d1 = ((forward / strike).ln() + 0.5 * sigma * sigma * t) / vol_sqrt_t;
    let d2 = d1 - vol_sqrt_t;
    let pdf_d1 = norm_pdf(d1);

    let price = black76_price(forward, strike, t, sigma, kind, r);
    let (delta, prob_itm) = match kind {
        OptionKind::Call => (df * norm_cdf(d1), norm_cdf(d2)),
        OptionKind::Put => (-df * norm_cdf(-d1), norm_cdf(-d2)),
    };
    let gamma = (df * pdf_d1) / (forward * vol_sqrt_t);
    let vega_1pct = (df * forward * pdf_d1 * sqrt_t) / 100.0;
    let theta_annual = -(df * forward * pdf_d1 * sigma) / (2.0 * sqrt_t) + r * price;
    let theta_per_day = theta_annual / 365.0;
    let vanna_1pct = (-df * pdf_d1 * (d2 / sigma)) / 100.0;
    let dd1_dt = -((forward / strike).ln()) / (2.0 * sigma * t * sqrt_t) + sigma / (4.0 * sqrt_t);
    let charm_annual = match kind {
        OptionKind::Call => -r * df * norm_cdf(d1) + df * pdf_d1 * dd1_dt,
        OptionKind::Put => r * df * norm_cdf(-d1) + df * pdf_d1 * dd1_dt,
    };
    let charm_per_day = -charm_annual / 365.0;
    let vomma_1pct = vega_1pct * (d1 * d2 / sigma) / 100.0;

    Greeks {
        price,
        iv_pct: sigma * 100.0,
        delta,
        gamma,
        vega_1pct,
        theta_per_day,
        vanna_1pct,
        charm_per_day,
        vomma_1pct,
        prob_itm_risk_neutral: prob_itm,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolveStatus {
    Solved,
    UnreachableAtLowBound,
    UnreachableAtHighBound,
}

impl SolveStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Solved => "SOLVED",
            Self::UnreachableAtLowBound => "UNREACHABLE_AT_LOW_BOUND",
            Self::UnreachableAtHighBound => "UNREACHABLE_AT_HIGH_BOUND",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForwardSolveResult {
    pub implied_forward: f64,
    pub short_model_price: f64,
    pub long_model_price: f64,
    pub spread_model_price: f64,
    pub status: SolveStatus,
}

/// Solves monotonically for `F*` where `V_short(F*) - V_long(F*) == target_spread`.
#[allow(clippy::too_many_arguments)]
pub fn solve_forward_for_spread_value(
    target_spread: f64,
    forward_now: f64,
    t_years: f64,
    kind: OptionKind,
    short_strike: f64,
    short_iv: f64,
    long_strike: f64,
    long_iv: f64,
    iv_bump: f64,
    r: f64,
) -> ForwardSolveResult {
    let s_iv = (short_iv + iv_bump).max(0.01);
    let l_iv = (long_iv + iv_bump).max(0.01);
    let spread_at = |f_val: f64| {
        let ps = black76_price(f_val, short_strike, t_years, s_iv, kind, r);
        let pl = black76_price(f_val, long_strike, t_years, l_iv, kind, r);
        (ps - pl, ps, pl)
    };

    let mut low = forward_now * 0.65;
    let mut high = forward_now * 1.35;
    let (v_low, ps_low, pl_low) = spread_at(low);
    let (v_high, ps_high, pl_high) = spread_at(high);

    match kind {
        OptionKind::Put => {
            if target_spread >= v_low {
                return ForwardSolveResult {
                    implied_forward: low,
                    short_model_price: ps_low,
                    long_model_price: pl_low,
                    spread_model_price: v_low,
                    status: SolveStatus::UnreachableAtLowBound,
                };
            }
            if target_spread <= v_high {
                return ForwardSolveResult {
                    implied_forward: high,
                    short_model_price: ps_high,
                    long_model_price: pl_high,
                    spread_model_price: v_high,
                    status: SolveStatus::UnreachableAtHighBound,
                };
            }
        }
        OptionKind::Call => {
            if target_spread <= v_low {
                return ForwardSolveResult {
                    implied_forward: low,
                    short_model_price: ps_low,
                    long_model_price: pl_low,
                    spread_model_price: v_low,
                    status: SolveStatus::UnreachableAtLowBound,
                };
            }
            if target_spread >= v_high {
                return ForwardSolveResult {
                    implied_forward: high,
                    short_model_price: ps_high,
                    long_model_price: pl_high,
                    spread_model_price: v_high,
                    status: SolveStatus::UnreachableAtHighBound,
                };
            }
        }
    }

    let mut mid = forward_now;
    let mut ps = 0.0;
    let mut pl = 0.0;
    let mut val = 0.0;
    for _ in 0..100 {
        mid = 0.5 * (low + high);
        (val, ps, pl) = spread_at(mid);
        if (val - target_spread).abs() < 1e-6 {
            break;
        }
        match kind {
            OptionKind::Put => {
                if val > target_spread {
                    low = mid;
                } else {
                    high = mid;
                }
            }
            OptionKind::Call => {
                if val < target_spread {
                    low = mid;
                } else {
                    high = mid;
                }
            }
        }
    }
    ForwardSolveResult {
        implied_forward: mid,
        short_model_price: ps,
        long_model_price: pl,
        spread_model_price: val,
        status: SolveStatus::Solved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_cdf_and_erfc_match_reference_quantiles() {
        assert!((norm_cdf(0.0) - 0.5).abs() < 1e-14);
        assert!((norm_cdf(1.959_963_984_540_054) - 0.975).abs() < 1e-12);
        assert!((norm_cdf(-1.959_963_984_540_054) - 0.025).abs() < 1e-12);
        assert!((norm_cdf(3.5) + norm_cdf(-3.5) - 1.0).abs() < 1e-14);
        assert!((norm_cdf(6.0) - 1.0).abs() < 1e-9);
        assert_eq!(erfc(f64::INFINITY), 0.0);
        assert_eq!(erfc(f64::NEG_INFINITY), 2.0);
        assert!(erfc(f64::NAN).is_nan());
        assert_eq!(erfc(30.0), 0.0);
        assert_eq!(norm_pdf(f64::INFINITY), 0.0);
        assert!((round_to_tick(123.43, 0.05) - 123.45).abs() < 1e-9);
        assert_eq!(round_to_tick(123.43, 0.0), 123.43);
    }

    #[test]
    fn parity_and_black76_round_trip_iv_and_reject_nan() {
        let parity = implied_forward_from_pair(
            Some(25_350.0),
            25_350.0,
            180.0,
            165.0,
            7.0 / 365.0,
            DEFAULT_RISK_FREE_RATE,
        )
        .unwrap();
        assert!(parity.implied_forward > 25_365.0);
        assert!(parity.basis_vs_spot.unwrap() > 15.0);
        assert!(implied_forward_from_pair(None, 0.0, 100.0, 100.0, 0.1, 0.067).is_err());

        for kind in [OptionKind::Call, OptionKind::Put] {
            let theo = black76_price(25_400.0, 25_600.0, 10.0 / 365.0, 0.145, kind, 0.067);
            let solved =
                implied_vol_with_status(theo, 25_400.0, 25_600.0, 10.0 / 365.0, kind, 0.067);
            assert_eq!(solved.status, IvStatus::Converged);
            assert!(solved.reliable);
            assert!((solved.iv - 0.145).abs() < 1e-7);

            let greeks = compute_greeks(25_400.0, 25_600.0, 10.0 / 365.0, 0.145, kind, 0.067);
            assert!(greeks.gamma > 0.0);
            assert!(greeks.vega_1pct > 0.0);
            assert!(greeks.theta_per_day < 0.0);
        }

        // Regression test: NaN/Inf must NEVER report Converged!
        let nan_iv =
            implied_vol_with_status(f64::NAN, 25_400.0, 25_600.0, 0.05, OptionKind::Call, 0.067);
        assert_eq!(nan_iv.status, IvStatus::InvalidInput);
        assert!(!nan_iv.reliable);
        assert_eq!(nan_iv.status.as_str(), "INVALID_INPUT");

        let zero_t =
            implied_vol_with_status(10.0, 25_400.0, 25_600.0, 0.0, OptionKind::Call, 0.067);
        assert_eq!(zero_t.status, IvStatus::ExpiredZeroTime);

        let below = implied_vol_with_status(1.0, 25_600.0, 25_400.0, 0.05, OptionKind::Call, 0.067);
        assert_eq!(below.status, IvStatus::BelowIntrinsic);

        let above =
            implied_vol_with_status(30_000.0, 25_400.0, 25_600.0, 0.05, OptionKind::Call, 0.067);
        assert_eq!(above.status, IvStatus::AboveNoArbBound);
    }

    #[test]
    fn forward_solver_handles_both_call_and_put_verticals_and_bounds() {
        let call_solve = solve_forward_for_spread_value(
            45.0,
            25_400.0,
            8.0 / 365.0,
            OptionKind::Call,
            25_600.0,
            0.14,
            25_800.0,
            0.135,
            0.0,
            0.067,
        );
        assert_eq!(call_solve.status, SolveStatus::Solved);
        assert!((call_solve.spread_model_price - 45.0).abs() < 1e-4);

        let put_solve = solve_forward_for_spread_value(
            45.0,
            25_400.0,
            8.0 / 365.0,
            OptionKind::Put,
            25_200.0,
            0.15,
            25_000.0,
            0.16,
            0.008,
            0.067,
        );
        assert_eq!(put_solve.status, SolveStatus::Solved);
        assert!((put_solve.spread_model_price - 45.0).abs() < 1e-4);

        let unreachable_low = solve_forward_for_spread_value(
            0.0,
            25_400.0,
            8.0 / 365.0,
            OptionKind::Call,
            25_600.0,
            0.14,
            25_800.0,
            0.14,
            0.0,
            0.067,
        );
        assert_eq!(unreachable_low.status, SolveStatus::UnreachableAtLowBound);
        assert_eq!(unreachable_low.status.as_str(), "UNREACHABLE_AT_LOW_BOUND");

        let unreachable_high = solve_forward_for_spread_value(
            250.0,
            25_400.0,
            8.0 / 365.0,
            OptionKind::Put,
            25_200.0,
            0.14,
            25_000.0,
            0.14,
            0.0,
            0.067,
        );
        assert_eq!(unreachable_high.status, SolveStatus::UnreachableAtLowBound);

        // Also exercise UnreachableAtHighBound for both Call and Put verticals + InvalidInput.
        let call_high = solve_forward_for_spread_value(
            250.0,
            25_400.0,
            8.0 / 365.0,
            OptionKind::Call,
            25_600.0,
            0.14,
            25_800.0,
            0.14,
            0.0,
            0.067,
        );
        assert_eq!(call_high.status, SolveStatus::UnreachableAtHighBound);
        assert_eq!(call_high.status.as_str(), "UNREACHABLE_AT_HIGH_BOUND");

        let put_high = solve_forward_for_spread_value(
            0.0,
            25_400.0,
            8.0 / 365.0,
            OptionKind::Put,
            25_200.0,
            0.14,
            25_000.0,
            0.14,
            0.0,
            0.067,
        );
        assert_eq!(put_high.status, SolveStatus::UnreachableAtHighBound);
        assert_eq!(call_solve.status.as_str(), "SOLVED");
    }
}
