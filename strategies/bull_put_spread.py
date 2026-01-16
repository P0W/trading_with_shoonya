"""
Bull Put Spread (BPS) Strategy Implementation

A credit spread where we:
- SELL a higher strike PUT (closer to ATM)
- BUY a lower strike PUT (further OTM)

Profit when: Market stays above short strike
Max Profit: Net premium received
Max Loss: Spread width - Premium received
"""
import logging
from typing import Optional, Tuple
import datetime
import pandas as pd

from strategies.base_strategy import (
    BaseStrategy, SpreadOrder, SpreadLeg, SpreadPosition,
    OrderSide, OptionType
)

logger = logging.getLogger(__name__)


class BullPutSpread(BaseStrategy):
    """
    Bull Put Spread (Credit Spread) Strategy.
    
    Best used when:
    - VIX > 15 (rich premiums)
    - RSI 30-50 (recovering from oversold)
    - Price above 20 EMA (uptrend)
    """
    
    @property
    def strategy_name(self) -> str:
        return "BULL_PUT_SPREAD"
    
    def calculate_strikes(
        self, 
        spot_price: float,
        otm_percent: float = 1.5,
        spread_width_points: int = 100
    ) -> Tuple[float, float]:
        """
        Calculate strike prices for Bull Put Spread.
        
        Args:
            spot_price: Current spot price
            otm_percent: How far OTM to sell put (default 1.5%)
            spread_width_points: Width between strikes in points
        
        Returns:
            Tuple of (short_strike, long_strike)
        """
        # Short strike: 1-2% OTM (below spot)
        short_strike_raw = spot_price * (1 - otm_percent / 100)
        short_strike = self.round_to_strike(short_strike_raw)
        
        # Long strike: spread_width points below short strike
        long_strike = short_strike - spread_width_points
        
        logger.info(
            f"BPS Strikes - Spot: {spot_price:.2f}, "
            f"Short PUT: {short_strike}, Long PUT: {long_strike}"
        )
        
        return short_strike, long_strike
    
    def create_spread_order(
        self,
        quantity: int = 1,
        otm_percent: float = 1.5,
        spread_width_points: int = 100
    ) -> Optional[SpreadOrder]:
        """
        Create a Bull Put Spread order.
        
        Args:
            quantity: Number of lots
            otm_percent: OTM percentage for short strike
            spread_width_points: Spread width in points
        
        Returns:
            SpreadOrder or None
        """
        from utils import get_closest_expiry, get_strike_tsym, download_scrip_master
        
        try:
            # Get spot price
            spot_price = self.get_spot_price()
            if not spot_price:
                logger.error("Cannot get spot price")
                return None
            
            # Calculate strikes
            short_strike, long_strike = self.calculate_strikes(
                spot_price, otm_percent, spread_width_points
            )
            
            # Get expiry
            expiry_date, df = get_closest_expiry(self.symbol)
            expiry_str = expiry_date.strftime("%d-%b-%Y").upper()
            
            # Convert expiry for lookup
            df['Expiry'] = pd.to_datetime(df['Expiry'], format="%d-%b-%Y")
            
            # Get short put details
            short_tsym = get_strike_tsym(df, expiry_date, short_strike, "PE")
            short_token = df[df['TradingSymbol'] == short_tsym]['Token'].values[0]
            short_quote = self.api.get_quotes(exchange=self.exchange, token=str(short_token))
            
            if not short_quote or 'lp' not in short_quote:
                logger.error(f"Cannot get quote for short put {short_tsym}")
                return None
            
            # Get long put details
            long_tsym = get_strike_tsym(df, expiry_date, long_strike, "PE")
            long_token = df[df['TradingSymbol'] == long_tsym]['Token'].values[0]
            long_quote = self.api.get_quotes(exchange=self.exchange, token=str(long_token))
            
            if not long_quote or 'lp' not in long_quote:
                logger.error(f"Cannot get quote for long put {long_tsym}")
                return None
            
            short_ltp = float(short_quote['lp'])
            long_ltp = float(long_quote['lp'])
            
            # Calculate spread metrics
            total_qty = quantity * self.lot_size
            net_premium = short_ltp - long_ltp  # Credit received
            spread_width = short_strike - long_strike
            max_profit = net_premium * total_qty
            max_loss = (spread_width - net_premium) * total_qty
            breakeven = short_strike - net_premium
            
            # Create leg objects
            short_leg = SpreadLeg(
                trading_symbol=short_tsym,
                token=str(short_token),
                strike=short_strike,
                option_type=OptionType.PUT,
                side=OrderSide.SELL,
                quantity=total_qty,
                ltp=short_ltp
            )
            
            long_leg = SpreadLeg(
                trading_symbol=long_tsym,
                token=str(long_token),
                strike=long_strike,
                option_type=OptionType.PUT,
                side=OrderSide.BUY,
                quantity=total_qty,
                ltp=long_ltp
            )
            
            spread_order = SpreadOrder(
                strategy_name=self.strategy_name,
                symbol=self.symbol,
                expiry_date=expiry_str,
                long_leg=long_leg,
                short_leg=short_leg,
                spread_width=spread_width,
                net_premium=net_premium,
                max_profit=max_profit,
                max_loss=max_loss,
                breakeven=breakeven
            )
            
            logger.info(
                f"\n{'='*50}\n"
                f"BULL PUT SPREAD ORDER\n"
                f"{'='*50}\n"
                f"Spot: {spot_price:.2f} | Expiry: {expiry_str}\n"
                f"SELL {short_tsym} @ {short_ltp:.2f}\n"
                f"BUY  {long_tsym} @ {long_ltp:.2f}\n"
                f"Net Credit: {net_premium:.2f} x {total_qty} = ₹{max_profit:.2f}\n"
                f"Max Loss: ₹{max_loss:.2f}\n"
                f"Breakeven: {breakeven:.2f}\n"
                f"Risk:Reward = 1:{max_profit/max_loss:.2f}\n"
                f"{'='*50}"
            )
            
            return spread_order
            
        except Exception as e:
            logger.error(f"Error creating BPS order: {e}")
            return None
    
    def get_delta_based_strike(
        self,
        target_delta: float = 0.20
    ) -> Optional[float]:
        """
        Find strike with approximate delta (simplified calculation).
        
        For puts, delta ~0.20 means ~80% probability of profit.
        
        Args:
            target_delta: Target delta for short put (0.15-0.25 typical)
        
        Returns:
            Strike price or None
        """
        spot = self.get_spot_price()
        if not spot:
            return None
        
        # Simplified: Delta 0.20 is roughly 1-1.5 standard deviations OTM
        # For weekly options with ~5 days, this is approximately 1.5-2% OTM
        otm_percent = (1 - target_delta) * 2.5  # Rough approximation
        strike = spot * (1 - otm_percent / 100)
        
        return self.round_to_strike(strike)
    
    def calculate_stop_loss(self, spread_order: SpreadOrder, sl_multiplier: float = 2.0) -> float:
        """
        Calculate stop loss price for the spread.
        
        Args:
            spread_order: The spread order
            sl_multiplier: Multiple of premium received for SL
        
        Returns:
            Stop loss premium level
        """
        # Exit when premium expands to sl_multiplier times received credit
        return spread_order.net_premium * sl_multiplier
    
    def calculate_target(self, spread_order: SpreadOrder, target_percent: float = 0.50) -> float:
        """
        Calculate target exit price.
        
        Args:
            spread_order: The spread order
            target_percent: Percentage of premium to capture
        
        Returns:
            Target premium level (to buy back spread)
        """
        # Exit when spread premium drops to target_percent of entry
        return spread_order.net_premium * (1 - target_percent)
