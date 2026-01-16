"""
EMA (Exponential Moving Average) calculation and signal generation
"""
import logging
from typing import Optional, Tuple
import pandas as pd
import numpy as np

logger = logging.getLogger(__name__)


def calculate_ema(prices: pd.Series, period: int = 20) -> pd.Series:
    """
    Calculate Exponential Moving Average.
    
    Args:
        prices: Series of closing prices
        period: EMA period (default 20)
    
    Returns:
        Series of EMA values
    """
    if len(prices) < period:
        logger.warning("Insufficient data for EMA calculation")
        return pd.Series([np.nan] * len(prices))
    
    return prices.ewm(span=period, adjust=False).mean()


def get_ema_signal(current_price: float, ema_value: float) -> Tuple[str, str]:
    """
    Get trading signal based on price position relative to EMA.
    
    Args:
        current_price: Current spot price
        ema_value: Current EMA value
    
    Returns:
        Tuple of (signal, description)
        signal: 'BULLISH', 'BEARISH', 'NEUTRAL'
    """
    if pd.isna(ema_value) or pd.isna(current_price):
        return "NEUTRAL", "EMA data not available"
    
    diff_percent = ((current_price - ema_value) / ema_value) * 100
    
    if current_price > ema_value:
        if diff_percent > 1.0:
            return "BULLISH", f"Price {diff_percent:.2f}% above 20 EMA - Strong uptrend"
        return "BULLISH", f"Price {diff_percent:.2f}% above 20 EMA - Uptrend intact"
    
    if diff_percent < -1.0:
        return "BEARISH", f"Price {diff_percent:.2f}% below 20 EMA - Downtrend"
    
    return "NEUTRAL", f"Price near 20 EMA ({diff_percent:.2f}%) - Consolidation"


def get_current_ema(shoonya_api, symbol: str = "NIFTY", period: int = 20) -> Tuple[Optional[float], Optional[float]]:
    """
    Fetch historical data and calculate current EMA along with spot price.
    
    Args:
        shoonya_api: Shoonya API instance
        symbol: Index symbol
        period: EMA period
    
    Returns:
        Tuple of (current_price, ema_value) or (None, None)
    """
    from const import INDICES_TOKEN
    from utils import get_exchange
    import datetime
    
    try:
        exchange = get_exchange(symbol, is_index=True)
        token = INDICES_TOKEN.get(symbol)
        
        if not token:
            logger.error(f"Token not found for {symbol}")
            return None, None
        
        # Get current price
        quote = shoonya_api.get_quotes(exchange=exchange, token=str(token))
        if not quote or 'lp' not in quote:
            logger.error(f"Failed to fetch quote for {symbol}")
            return None, None
        
        current_price = float(quote['lp'])
        
        # Get historical data for EMA
        end_date = datetime.datetime.now()
        start_date = end_date - datetime.timedelta(days=45)
        
        ret = shoonya_api.get_time_price_series(
            exchange=exchange,
            token=str(token),
            starttime=start_date.timestamp(),
            endtime=end_date.timestamp(),
            interval=240  # 4-hour candles
        )
        
        if not ret:
            logger.error("Failed to fetch historical data for EMA")
            return current_price, None
        
        df = pd.DataFrame(ret)
        df['close'] = pd.to_numeric(df['intc'], errors='coerce')
        df = df.sort_values('time')
        
        ema_series = calculate_ema(df['close'], period)
        ema_value = ema_series.iloc[-1]
        
        logger.info(f"{symbol} Price: {current_price:.2f}, EMA({period}): {ema_value:.2f}")
        return current_price, ema_value
        
    except Exception as e:
        logger.error(f"Error calculating EMA: {e}")
        return None, None
