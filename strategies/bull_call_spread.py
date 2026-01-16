"""
Bull Call Spread (BCS) Strategy Implementation

A debit spread where we:
- BUY a lower strike CALL (ATM or slightly OTM)
- SELL a higher strike CALL (further OTM)

Profit when: Market moves up above bought strike
Max Profit: Spread width - Premium paid
Max Loss: Net premium paid
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


class BullCallSpread(BaseStrategy):
    """
    Bull Call Spread (Debit Spread) Strategy.
    
    Best used when:
    - VIX < 13 (cheap options)
    - RSI 50-70 (bullish momentum)
    - Price above 20 EMA (uptrend)
    """
    
    @property
    def strategy_name(self) -> str:
        return "BULL_CALL_SPREAD"
    
    def calculate_strikes(
        self, 
        spot_price: float,
        buy_strike_offset: float = 0.0,  # ATM by default
        spread_width_points: int = 100
    ) -> Tuple[float, float]:
        """
        Calculate strike prices for Bull Call Spread.
        
        Args:
            spot_price: Current spot price
            buy_strike_offset: Offset from ATM for long call (0 = ATM)
            spread_width_points: Width between strikes in points
        
        Returns:
            Tuple of (long_strike, short_strike)
        """
        # Long strike: ATM or slightly OTM
        long_strike_raw = spot_price + buy_strike_offset
        long_strike = self.round_to_strike(long_strike_raw)
        
        # Short strike: spread_width points above long strike
        short_strike = long_strike + spread_width_points
        
        logger.info(
            f"BCS Strikes - Spot: {spot_price:.2f}, "
            f"Long CALL: {long_strike}, Short CALL: {short_strike}"
        )
        
        return long_strike, short_strike
    
    def create_spread_order(
        self,
        quantity: int = 1,
        buy_strike_offset: float = 0.0,
        spread_width_points: int = 100
    ) -> Optional[SpreadOrder]:
        """
        Create a Bull Call Spread order.
        
        Args:
            quantity: Number of lots
            buy_strike_offset: Offset from ATM for long strike
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
            long_strike, short_strike = self.calculate_strikes(
                spot_price, buy_strike_offset, spread_width_points
            )
            
            # Get expiry
            expiry_date, df = get_closest_expiry(self.symbol)
            expiry_str = expiry_date.strftime("%d-%b-%Y").upper()
            
            # Convert expiry for lookup
            df['Expiry'] = pd.to_datetime(df['Expiry'], format="%d-%b-%Y")
            
            # Get long call details (BUY)
            long_tsym = get_strike_tsym(df, expiry_date, long_strike, "CE")
            long_token = df[df['TradingSymbol'] == long_tsym]['Token'].values[0]
            long_quote = self.api.get_quotes(exchange=self.exchange, token=str(long_token))
            
            if not long_quote or 'lp' not in long_quote:
                logger.error(f"Cannot get quote for long call {long_tsym}")
                return None
            
            # Get short call details (SELL)
            short_tsym = get_strike_tsym(df, expiry_date, short_strike, "CE")
            short_token = df[df['TradingSymbol'] == short_tsym]['Token'].values[0]
            short_quote = self.api.get_quotes(exchange=self.exchange, token=str(short_token))
            
            if not short_quote or 'lp' not in short_quote:
                logger.error(f"Cannot get quote for short call {short_tsym}")
                return None
            
            long_ltp = float(long_quote['lp'])
            short_ltp = float(short_quote['lp'])
            
            # Calculate spread metrics
            total_qty = quantity * self.lot_size
            net_premium = long_ltp - short_ltp  # Debit paid (negative = paid out)
            spread_width = short_strike - long_strike
            max_loss = net_premium * total_qty  # Premium paid
            max_profit = (spread_width - net_premium) * total_qty
            breakeven = long_strike + net_premium
            
            # Create leg objects
            long_leg = SpreadLeg(
                trading_symbol=long_tsym,
                token=str(long_token),
                strike=long_strike,
                option_type=OptionType.CALL,
                side=OrderSide.BUY,
                quantity=total_qty,
                ltp=long_ltp
            )
            
            short_leg = SpreadLeg(
                trading_symbol=short_tsym,
                token=str(short_token),
                strike=short_strike,
                option_type=OptionType.CALL,
                side=OrderSide.SELL,
                quantity=total_qty,
                ltp=short_ltp
            )
            
            spread_order = SpreadOrder(
                strategy_name=self.strategy_name,
                symbol=self.symbol,
                expiry_date=expiry_str,
                long_leg=long_leg,
                short_leg=short_leg,
                spread_width=spread_width,
                net_premium=-net_premium,  # Negative because we pay
                max_profit=max_profit,
                max_loss=max_loss,
                breakeven=breakeven
            )
            
            logger.info(
                f"\n{'='*50}\n"
                f"BULL CALL SPREAD ORDER\n"
                f"{'='*50}\n"
                f"Spot: {spot_price:.2f} | Expiry: {expiry_str}\n"
                f"BUY  {long_tsym} @ {long_ltp:.2f}\n"
                f"SELL {short_tsym} @ {short_ltp:.2f}\n"
                f"Net Debit: {net_premium:.2f} x {total_qty} = ₹{max_loss:.2f}\n"
                f"Max Profit: ₹{max_profit:.2f}\n"
                f"Breakeven: {breakeven:.2f}\n"
                f"Risk:Reward = 1:{max_profit/max_loss:.2f}\n"
                f"{'='*50}"
            )
            
            return spread_order
            
        except Exception as e:
            logger.error(f"Error creating BCS order: {e}")
            return None
    
    def calculate_stop_loss(self, spread_order: SpreadOrder, sl_percent: float = 0.50) -> float:
        """
        Calculate stop loss for the spread.
        
        Args:
            spread_order: The spread order
            sl_percent: Percentage of premium to lose before exit
        
        Returns:
            Stop loss premium level
        """
        # For debit spreads, SL when premium drops by sl_percent
        entry_debit = abs(spread_order.net_premium)
        return entry_debit * (1 - sl_percent)
    
    def calculate_target(self, spread_order: SpreadOrder, target_percent: float = 0.70) -> float:
        """
        Calculate target exit price.
        
        Args:
            spread_order: The spread order
            target_percent: Percentage of max profit to capture
        
        Returns:
            Target premium level (to sell spread)
        """
        # Target is when spread value increases towards max profit
        entry_debit = abs(spread_order.net_premium)
        potential_gain = spread_order.spread_width - entry_debit
        target_gain = potential_gain * target_percent
        return entry_debit + target_gain
