"""
RSI (Relative Strength Index) calculation and signal generation
"""
import logging
from typing import Optional, Tuple
import pandas as pd
import numpy as np

logger = logging.getLogger(__name__)


def calculate_rsi(prices: pd.Series, period: int = 14) -> pd.Series:
    """
    Calculate RSI using Wilder's smoothing method.
    
    Args:
        prices: Series of closing prices
        period: RSI period (default 14)
    
    Returns:
        Series of RSI values
    """
    if len(prices) < period + 1:
        logger.warning("Insufficient data for RSI calculation")
        return pd.Series([np.nan] * len(prices))
    
    delta = prices.diff()
    gain = delta.where(delta > 0, 0.0)
    loss = (-delta).where(delta < 0, 0.0)
    
    # Wilder's smoothing (exponential moving average)
    avg_gain = gain.ewm(alpha=1/period, min_periods=period, adjust=False).mean()
    avg_loss = loss.ewm(alpha=1/period, min_periods=period, adjust=False).mean()
    
    rs = avg_gain / avg_loss
    rsi = 100 - (100 / (1 + rs))
    
    return rsi


def get_rsi_signal(rsi_value: float) -> Tuple[str, str]:
    """
    Get trading signal based on RSI value.
    
    Args:
        rsi_value: Current RSI value
    
    Returns:
        Tuple of (signal, description)
        signal: 'BPS', 'BCS', 'WAIT', 'NO_TRADE'
    """
    if pd.isna(rsi_value):
        return "NO_TRADE", "RSI not available"
    
    if rsi_value < 30:
        return "WAIT", f"RSI {rsi_value:.1f} - Heavily oversold, wait for recovery"
    
    if 30 <= rsi_value < 50:
        return "BPS", f"RSI {rsi_value:.1f} - Recovering from oversold, favor BPS"
    
    if 50 <= rsi_value < 70:
        return "BCS", f"RSI {rsi_value:.1f} - Strong momentum, favor BCS"
    
    if rsi_value >= 70:
        return "NO_TRADE", f"RSI {rsi_value:.1f} - Overbought, avoid bullish spreads"
    
    return "WAIT", f"RSI {rsi_value:.1f} - Neutral zone"


def get_current_rsi(shoonya_api, symbol: str = "NIFTY", period: int = 14) -> Optional[float]:
    """
    Fetch historical data and calculate current RSI.
    
    Args:
        shoonya_api: Shoonya API instance
        symbol: Index symbol
        period: RSI period
    
    Returns:
        Current RSI value or None
    """
    from const import INDICES_TOKEN
    from utils import get_exchange
    import datetime
    
    try:
        exchange = get_exchange(symbol, is_index=True)
        token = INDICES_TOKEN.get(symbol)
        
        if not token:
            logger.error(f"Token not found for {symbol}")
            return None
        
        # Get last 30 days of data for RSI calculation
        end_date = datetime.datetime.now()
        start_date = end_date - datetime.timedelta(days=30)
        
        ret = shoonya_api.get_time_price_series(
            exchange=exchange,
            token=str(token),
            starttime=start_date.timestamp(),
            endtime=end_date.timestamp(),
            interval=240  # 4-hour candles for smoother RSI
        )
        
        if not ret:
            logger.error("Failed to fetch historical data for RSI")
            return None
        
        df = pd.DataFrame(ret)
        df['close'] = pd.to_numeric(df['intc'], errors='coerce')
        df = df.sort_values('time')
        
        rsi_series = calculate_rsi(df['close'], period)
        current_rsi = rsi_series.iloc[-1]
        
        logger.info(f"{symbol} Current RSI({period}): {current_rsi:.2f}")
        return current_rsi
        
    except Exception as e:
        logger.error(f"Error calculating RSI: {e}")
        return None
