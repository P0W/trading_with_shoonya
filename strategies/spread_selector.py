"""
Strategy selector using RSI, VIX, and EMA indicators
"""
import logging
from dataclasses import dataclass
from typing import Optional, Tuple
from enum import Enum

logger = logging.getLogger(__name__)


class StrategyType(Enum):
    """Available strategy types"""
    BPS = "BULL_PUT_SPREAD"
    BCS = "BULL_CALL_SPREAD"
    NO_TRADE = "NO_TRADE"
    WAIT = "WAIT"


@dataclass
class StrategySignal:
    """Combined signal from all indicators"""
    strategy: StrategyType
    confidence: float  # 0-100
    rsi_signal: str
    vix_signal: str
    ema_signal: str
    rsi_value: float
    vix_value: float
    spot_price: float
    ema_value: float
    reason: str


class SpreadSelector:
    """
    Select between BPS and BCS based on market indicators.
    
    Decision Matrix:
    - VIX > 15 + RSI 30-50 + Price > 20 EMA → BPS (credit spread)
    - VIX < 13 + RSI 50-70 + Price > 20 EMA → BCS (debit spread)
    - VIX 13-15 (neutral) → WAIT
    - RSI > 70 (overbought) → NO_TRADE
    - RSI < 30 (oversold) → WAIT for recovery
    - Price < 20 EMA → NO bullish spreads
    """
    
    def __init__(self, shoonya_api, symbol: str = "NIFTY"):
        """
        Initialize selector with API and symbol.
        
        Args:
            shoonya_api: Shoonya API instance
            symbol: Index symbol (default NIFTY)
        """
        self.api = shoonya_api
        self.symbol = symbol
        
        # Initialize indicators
        from indicators.vix_monitor import VIXMonitor
        self.vix_monitor = VIXMonitor(shoonya_api)
    
    def get_all_indicators(self) -> Tuple[Optional[float], Optional[float], Optional[float], Optional[float]]:
        """
        Fetch all indicator values.
        
        Returns:
            Tuple of (rsi, vix, spot_price, ema)
        """
        from indicators.rsi import get_current_rsi
        from indicators.ema import get_current_ema
        
        # Get RSI
        rsi = get_current_rsi(self.api, self.symbol)
        
        # Get VIX
        vix = self.vix_monitor.get_vix()
        
        # Get spot price and EMA
        spot_price, ema = get_current_ema(self.api, self.symbol)
        
        return rsi, vix, spot_price, ema
    
    def select_strategy(self) -> StrategySignal:
        """
        Analyze indicators and select appropriate strategy.
        
        Returns:
            StrategySignal with recommendation
        """
        rsi, vix, spot_price, ema = self.get_all_indicators()
        
        # Default values for missing data
        rsi = rsi if rsi is not None else 50.0
        vix = vix if vix is not None else 14.0
        spot_price = spot_price if spot_price is not None else 0.0
        ema = ema if ema is not None else spot_price
        
        # Determine individual signals
        rsi_signal = self._get_rsi_signal(rsi)
        vix_signal = self._get_vix_signal(vix)
        ema_signal = self._get_ema_signal(spot_price, ema)
        
        # Combined decision logic
        strategy, confidence, reason = self._make_decision(
            rsi, vix, spot_price, ema,
            rsi_signal, vix_signal, ema_signal
        )
        
        return StrategySignal(
            strategy=strategy,
            confidence=confidence,
            rsi_signal=rsi_signal,
            vix_signal=vix_signal,
            ema_signal=ema_signal,
            rsi_value=rsi,
            vix_value=vix,
            spot_price=spot_price,
            ema_value=ema,
            reason=reason
        )
    
    def _get_rsi_signal(self, rsi: float) -> str:
        """Get RSI signal string"""
        if rsi < 30:
            return "OVERSOLD"
        elif 30 <= rsi < 50:
            return "RECOVERING"
        elif 50 <= rsi < 70:
            return "BULLISH"
        else:
            return "OVERBOUGHT"
    
    def _get_vix_signal(self, vix: float) -> str:
        """Get VIX signal string"""
        if vix < 13:
            return "LOW_IV"
        elif 13 <= vix < 15:
            return "NEUTRAL_IV"
        elif 15 <= vix < 20:
            return "HIGH_IV"
        else:
            return "EXTREME_IV"
    
    def _get_ema_signal(self, spot: float, ema: float) -> str:
        """Get EMA signal string"""
        if spot == 0 or ema == 0:
            return "UNKNOWN"
        
        diff_percent = ((spot - ema) / ema) * 100
        
        if diff_percent > 0.5:
            return "ABOVE_EMA"
        elif diff_percent < -0.5:
            return "BELOW_EMA"
        else:
            return "AT_EMA"
    
    def _make_decision(
        self, 
        rsi: float, 
        vix: float, 
        spot: float, 
        ema: float,
        rsi_signal: str,
        vix_signal: str,
        ema_signal: str
    ) -> Tuple[StrategyType, float, str]:
        """
        Make final strategy decision based on all indicators.
        
        Returns:
            Tuple of (StrategyType, confidence, reason)
        """
        reasons = []
        
        # Check EMA first - no bullish spreads if below EMA
        if ema_signal == "BELOW_EMA":
            return (
                StrategyType.NO_TRADE,
                0.0,
                f"Price below 20 EMA - No bullish spreads | RSI:{rsi:.1f} VIX:{vix:.1f}"
            )
        
        # Check for overbought - no bullish spreads
        if rsi_signal == "OVERBOUGHT":
            return (
                StrategyType.NO_TRADE,
                0.0,
                f"RSI {rsi:.1f} overbought - Avoid bullish spreads | VIX:{vix:.1f}"
            )
        
        # Check for extreme VIX
        if vix_signal == "EXTREME_IV":
            return (
                StrategyType.NO_TRADE,
                0.0,
                f"VIX {vix:.1f} extreme - Market too volatile | RSI:{rsi:.1f}"
            )
        
        # Check for oversold - wait for recovery
        if rsi_signal == "OVERSOLD":
            return (
                StrategyType.WAIT,
                20.0,
                f"RSI {rsi:.1f} oversold - Wait for recovery before BPS | VIX:{vix:.1f}"
            )
        
        # BPS Conditions: VIX > 15 + RSI 30-50 + Above EMA
        if vix_signal == "HIGH_IV" and rsi_signal == "RECOVERING" and ema_signal in ["ABOVE_EMA", "AT_EMA"]:
            confidence = 80.0
            if ema_signal == "ABOVE_EMA":
                confidence += 10.0
            return (
                StrategyType.BPS,
                confidence,
                f"BPS Signal: High IV ({vix:.1f}) + Recovering RSI ({rsi:.1f}) + Uptrend"
            )
        
        # BCS Conditions: VIX < 13 + RSI 50-70 + Above EMA
        if vix_signal == "LOW_IV" and rsi_signal == "BULLISH" and ema_signal in ["ABOVE_EMA", "AT_EMA"]:
            confidence = 75.0
            if ema_signal == "ABOVE_EMA":
                confidence += 10.0
            return (
                StrategyType.BCS,
                confidence,
                f"BCS Signal: Low IV ({vix:.1f}) + Bullish RSI ({rsi:.1f}) + Uptrend"
            )
        
        # Neutral VIX - wait
        if vix_signal == "NEUTRAL_IV":
            return (
                StrategyType.WAIT,
                30.0,
                f"VIX {vix:.1f} neutral - Wait for clearer signal | RSI:{rsi:.1f}"
            )
        
        # Mixed signals - try to pick best option
        if vix > 15 and ema_signal in ["ABOVE_EMA", "AT_EMA"]:
            return (
                StrategyType.BPS,
                60.0,
                f"BPS (moderate): High IV ({vix:.1f}) but mixed RSI ({rsi:.1f})"
            )
        
        if vix < 13 and ema_signal in ["ABOVE_EMA", "AT_EMA"]:
            return (
                StrategyType.BCS,
                55.0,
                f"BCS (moderate): Low IV ({vix:.1f}) but mixed RSI ({rsi:.1f})"
            )
        
        # Default - wait
        return (
            StrategyType.WAIT,
            25.0,
            f"Mixed signals - RSI:{rsi:.1f} VIX:{vix:.1f} EMA:{ema_signal}"
        )
    
    def print_analysis(self) -> StrategySignal:
        """Print detailed analysis and return signal"""
        signal = self.select_strategy()
        
        print("\n" + "="*60)
        print("NIFTY SPREAD STRATEGY ANALYSIS")
        print("="*60)
        print(f"Spot Price: {signal.spot_price:.2f}")
        print(f"20 EMA:     {signal.ema_value:.2f}")
        print(f"RSI(14):    {signal.rsi_value:.2f} [{signal.rsi_signal}]")
        print(f"INDIA VIX:  {signal.vix_value:.2f} [{signal.vix_signal}]")
        print("-"*60)
        print(f"RECOMMENDATION: {signal.strategy.value}")
        print(f"CONFIDENCE:     {signal.confidence:.0f}%")
        print(f"REASON:         {signal.reason}")
        print("="*60 + "\n")
        
        return signal
