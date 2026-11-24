//! Calculation primitives shared by the spread pilot's execution modes.
#![forbid(unsafe_code)]

use std::str::FromStr;

pub mod audit;
pub mod broker;
pub mod connection;
pub mod engine;
pub mod market;
pub mod math;
pub mod oauth;
pub mod shoonya;
pub mod strategy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionMode {
    #[default]
    Paper,
    Live,
}

impl FromStr for ExecutionMode {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "paper" => Ok(Self::Paper),
            "live" => Ok(Self::Live),
            _ => Err("NIFTY_EXECUTION_MODE must be 'paper' or 'live'."),
        }
    }
}

/// Hypothetical fill projection only; not a broker ledger or order admission guard.
/// Outstanding orders and unexpected real fills are not represented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SimulatedFills {
    hedge: u32,
    short: u32,
}

impl SimulatedFills {
    pub fn hedge(&self) -> u32 {
        self.hedge
    }

    pub fn short(&self) -> u32 {
        self.short
    }

    pub fn record_hedge_buy(&mut self, quantity: u32) -> Result<(), &'static str> {
        if quantity == 0 {
            return Err("Fill quantity must be positive.");
        }
        self.hedge = self
            .hedge
            .checked_add(quantity)
            .ok_or("Quantity overflow.")?;
        Ok(())
    }

    pub fn record_short_sell(&mut self, quantity: u32) -> Result<(), &'static str> {
        if quantity == 0 {
            return Err("Fill quantity must be positive.");
        }
        let short = self
            .short
            .checked_add(quantity)
            .ok_or("Quantity overflow.")?;
        if short > self.hedge {
            return Err("Short fills cannot exceed confirmed hedge fills.");
        }
        self.short = short;
        Ok(())
    }

    pub fn record_short_buy(&mut self, quantity: u32) -> Result<(), &'static str> {
        if quantity == 0 || quantity > self.short {
            return Err("Short closing fill exceeds the open short quantity.");
        }
        self.short -= quantity;
        Ok(())
    }

    pub fn record_hedge_sell(&mut self, quantity: u32) -> Result<(), &'static str> {
        if quantity == 0 || quantity > self.hedge {
            return Err("Hedge closing fill exceeds the open hedge quantity.");
        }
        if self.hedge - quantity < self.short {
            return Err("Hedge sale would leave a short position uncovered.");
        }
        self.hedge -= quantity;
        Ok(())
    }
}

/// All prices and monetary amounts are in paise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quote {
    pub bid: i64,
    pub ask: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKind {
    Call,
    Put,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreditSpread {
    pub option_kind: OptionKind,
    pub short_strike: i64,
    pub long_strike: i64,
    pub short_entry: i64,
    pub long_entry: i64,
    pub quantity: u32,
}

impl CreditSpread {
    fn validate(self, estimated_round_trip_charges: i64) -> Result<(i64, i64), &'static str> {
        if self.short_strike <= 0
            || self.long_strike <= 0
            || self.short_entry <= 0
            || self.long_entry <= 0
            || self.quantity == 0
            || estimated_round_trip_charges < 0
        {
            return Err("Invalid spread, quantity, or estimated charges.");
        }

        if match self.option_kind {
            OptionKind::Call => self.short_strike >= self.long_strike,
            OptionKind::Put => self.short_strike <= self.long_strike,
        } {
            return Err("Strike ordering is invalid for this credit spread.");
        }
        let width = (self.short_strike - self.long_strike).abs();
        let credit = self.short_entry - self.long_entry;
        if credit <= 0 || credit >= width {
            return Err("Entry must be a credit smaller than the strike width.");
        }
        Ok((width, credit))
    }

    /// Threshold estimate assuming both legs form a matching, fully filled vertical.
    /// Contract identity and actual coverage must be verified separately.
    pub fn assumed_vertical_loss_threshold(
        self,
        estimated_round_trip_charges: i64,
    ) -> Result<i64, &'static str> {
        let (width, credit) = self.validate(estimated_round_trip_charges)?;
        let max_loss = i128::from(width - credit) * i128::from(self.quantity)
            + i128::from(estimated_round_trip_charges);
        // Monetary amounts are paise; half-paise thresholds round up.
        let proportional = (max_loss * 35 + 50) / 100;
        i64::try_from(max_loss.min(150_000_i128.max(proportional)))
            .map_err(|_| "Loss threshold exceeds the supported range.")
    }

    /// Top-of-book estimate only; does not establish liquidity for the full quantity.
    pub fn top_of_book_pnl_estimate(
        self,
        short: Quote,
        long: Quote,
        estimated_round_trip_charges: i64,
    ) -> Result<i64, &'static str> {
        if short.ask <= 0
            || short.bid < 0
            || (short.bid > 0 && short.bid > short.ask)
            || long.bid <= 0
            || long.ask < 0
            || (long.ask > 0 && long.bid > long.ask)
        {
            return Err("Exit quote sides are unavailable, invalid, or crossed.");
        }
        let (_, entry_credit) = self.validate(estimated_round_trip_charges)?;
        let exit_debit = i128::from(short.ask) - i128::from(long.bid);
        let pnl = (i128::from(entry_credit) - exit_debit) * i128::from(self.quantity)
            - i128::from(estimated_round_trip_charges);
        i64::try_from(pnl).map_err(|_| "Spread P&L exceeds the supported range.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spread() -> CreditSpread {
        CreditSpread {
            option_kind: OptionKind::Call,
            short_strike: 2_500_000,
            long_strike: 2_520_000,
            short_entry: 12_000,
            long_entry: 8_000,
            quantity: 65,
        }
    }

    #[test]
    fn dynamic_loss_threshold_matches_skill_formula() {
        // Max loss: (200 - 40) * 65 + 100 = Rs 10,500.
        assert_eq!(
            spread().assumed_vertical_loss_threshold(10_000),
            Ok(367_500)
        );
        let small = CreditSpread {
            quantity: 1,
            ..spread()
        };
        assert_eq!(small.assumed_vertical_loss_threshold(0), Ok(16_000));
        let put = CreditSpread {
            option_kind: OptionKind::Put,
            short_strike: 2_520_000,
            long_strike: 2_500_000,
            ..spread()
        };
        assert_eq!(put.assumed_vertical_loss_threshold(10_000), Ok(367_500));
        assert!(
            CreditSpread {
                option_kind: OptionKind::Call,
                ..put
            }
            .assumed_vertical_loss_threshold(10_000)
            .is_err()
        );
        assert!(
            CreditSpread {
                option_kind: OptionKind::Put,
                ..spread()
            }
            .assumed_vertical_loss_threshold(10_000)
            .is_err()
        );
    }

    #[test]
    fn dynamic_loss_threshold_applies_floor_between_full_and_proportional_loss() {
        for (quantity, expected) in [(9, 144_000), (10, 150_000), (26, 150_000), (27, 151_200)] {
            assert_eq!(
                CreditSpread {
                    quantity,
                    ..spread()
                }
                .assumed_vertical_loss_threshold(0),
                Ok(expected)
            );
        }
    }

    #[test]
    fn marks_short_at_ask_and_hedge_at_bid() {
        let short = Quote {
            bid: 8_900,
            ask: 9_000,
        };
        let long = Quote {
            bid: 6_000,
            ask: 6_100,
        };
        assert_eq!(
            spread().top_of_book_pnl_estimate(short, long, 10_000),
            Ok(55_000)
        );
        assert_eq!(
            spread().top_of_book_pnl_estimate(
                Quote { bid: 0, ask: 9_000 },
                Quote { bid: 6_000, ask: 0 },
                10_000,
            ),
            Ok(55_000)
        );
        assert!(
            spread()
                .top_of_book_pnl_estimate(
                    Quote { bid: 0, ask: 0 },
                    Quote { bid: 6_000, ask: 0 },
                    10_000
                )
                .is_err()
        );
    }

    #[test]
    fn rejects_invalid_quotes_and_credit() {
        assert!(
            spread()
                .top_of_book_pnl_estimate(
                    Quote { bid: 100, ask: 99 },
                    Quote { bid: 50, ask: 60 },
                    0,
                )
                .is_err()
        );
        assert!(
            CreditSpread {
                short_entry: 8_000,
                ..spread()
            }
            .assumed_vertical_loss_threshold(0)
            .is_err()
        );
    }

    #[test]
    fn risk_math_uses_wide_intermediates_and_explicit_rounding() {
        assert_eq!(
            spread().assumed_vertical_loss_threshold(300_000_000_000_000_000),
            Ok(105_000_000_000_364_000)
        );
        assert_eq!(
            spread().assumed_vertical_loss_threshold(10_010),
            Ok(367_504)
        );
    }

    #[test]
    fn spread_validation_and_overflow_fail_closed() {
        let invalid = [
            CreditSpread {
                quantity: 0,
                ..spread()
            },
            CreditSpread {
                short_strike: 0,
                ..spread()
            },
            CreditSpread {
                long_strike: 0,
                ..spread()
            },
            CreditSpread {
                short_entry: 0,
                ..spread()
            },
            CreditSpread {
                long_entry: 0,
                ..spread()
            },
            CreditSpread {
                long_strike: 2_500_000,
                ..spread()
            },
            CreditSpread {
                short_entry: 28_000,
                ..spread()
            },
        ];
        for value in invalid {
            assert!(value.assumed_vertical_loss_threshold(0).is_err());
        }
        assert!(spread().assumed_vertical_loss_threshold(-1).is_err());
        let huge = CreditSpread {
            option_kind: OptionKind::Call,
            short_strike: 1,
            long_strike: i64::MAX,
            short_entry: 2,
            long_entry: 1,
            quantity: u32::MAX,
        };
        assert!(huge.assumed_vertical_loss_threshold(0).is_err());
        assert!(
            huge.top_of_book_pnl_estimate(
                Quote {
                    bid: 0,
                    ask: i64::MAX
                },
                Quote { bid: 1, ask: 0 },
                0
            )
            .is_err()
        );
    }

    #[test]
    fn missing_required_or_crossed_exit_side_fails_closed() {
        let short = Quote { bid: 90, ask: 100 };
        let long = Quote { bid: 40, ask: 50 };
        for (short, long) in [
            (Quote { bid: 0, ask: 0 }, long),
            (Quote { bid: -1, ask: 100 }, long),
            (Quote { bid: 101, ask: 100 }, long),
            (short, Quote { bid: 0, ask: 0 }),
            (short, Quote { bid: 40, ask: -1 }),
            (short, Quote { bid: 51, ask: 50 }),
        ] {
            assert!(spread().top_of_book_pnl_estimate(short, long, 0).is_err());
        }
    }

    #[test]
    fn execution_mode_requires_explicit_live_configuration() {
        assert_eq!(ExecutionMode::default(), ExecutionMode::Paper);
        assert_eq!("paper".parse(), Ok(ExecutionMode::Paper));
        assert_eq!("live".parse(), Ok(ExecutionMode::Live));
        assert!("liv".parse::<ExecutionMode>().is_err());
    }

    #[test]
    fn fill_sequence_preserves_confirmed_hedge_during_partial_fills() {
        let mut fills = SimulatedFills::default();
        assert!(fills.record_short_sell(1).is_err());
        fills.record_hedge_buy(65).unwrap();
        fills.record_short_sell(40).unwrap();
        assert!(fills.record_short_sell(26).is_err());
        assert!(fills.record_hedge_sell(26).is_err());
        fills.record_short_buy(15).unwrap();
        fills.record_hedge_sell(40).unwrap();
        assert_eq!((fills.hedge(), fills.short()), (25, 25));
        fills.record_short_buy(25).unwrap();
        fills.record_hedge_sell(25).unwrap();
        assert_eq!((fills.hedge(), fills.short()), (0, 0));
    }

    #[test]
    fn simulation_rejects_invalid_fill_quantities_without_mutating_projection() {
        let mut fills = SimulatedFills::default();
        assert!(fills.record_hedge_buy(0).is_err());
        assert!(fills.record_short_sell(0).is_err());
        assert!(fills.record_short_buy(0).is_err());
        assert!(fills.record_hedge_sell(0).is_err());
        fills.record_hedge_buy(u32::MAX).unwrap();
        assert!(fills.record_hedge_buy(1).is_err());
        fills.record_short_sell(u32::MAX).unwrap();
        assert!(fills.record_short_sell(1).is_err());
        assert!(fills.record_hedge_sell(1).is_err());
        assert_eq!((fills.hedge(), fills.short()), (u32::MAX, u32::MAX));
    }
}
