"""
INDIA VIX monitoring for volatility-based strategy selection
"""
import logging
from typing import Optional, Tuple
from dataclasses import dataclass
from enum import Enum

logger = logging.getLogger(__name__)


class VIXLevel(Enum):
    """VIX level classifications"""
    LOW = "LOW"           # < 13 - Low volatility, cheap options
    NEUTRAL = "NEUTRAL"   # 13-15 - Neutral zone
    HIGH = "HIGH"         # 15-20 - High volatility, rich premiums
    EXTREME = "EXTREME"   # > 20 - Market fear, avoid trading


@dataclass
class VIXReading:
    """VIX reading with classification"""
    value: float
    level: VIXLevel
    signal: str
    description: str


class VIXMonitor:
    """
    Monitor INDIA VIX for strategy selection and risk management.
    """
    
    # VIX thresholds
    LOW_THRESHOLD = 13.0
    NEUTRAL_THRESHOLD = 15.0
    HIGH_THRESHOLD = 20.0
    EXTREME_THRESHOLD = 25.0
    
    def __init__(self, shoonya_api):
        """
        Initialize VIX monitor.
        
        Args:
            shoonya_api: Shoonya API instance
        """
        self.api = shoonya_api
        self.vix_token = "26017"  # INDIA VIX token
        self.last_vix = None
        
    def get_vix(self) -> Optional[float]:
        """
        Fetch current INDIA VIX value.
        
        Returns:
            Current VIX value or None
        """
        try:
            ret = self.api.get_quotes(exchange="NSE", token=self.vix_token)
            
            if ret and 'lp' in ret:
                vix = float(ret['lp'])
                self.last_vix = vix
                logger.info(f"INDIA VIX: {vix:.2f}")
                return vix
            
            logger.error("Failed to fetch VIX quote")
            return None
            
        except Exception as e:
            logger.error(f"Error fetching VIX: {e}")
            return None
    
    def classify_vix(self, vix_value: float) -> VIXLevel:
        """
        Classify VIX into levels.
        
        Args:
            vix_value: Current VIX value
        
        Returns:
            VIXLevel enum
        """
        if vix_value < self.LOW_THRESHOLD:
            return VIXLevel.LOW
        elif vix_value < self.NEUTRAL_THRESHOLD:
            return VIXLevel.NEUTRAL
        elif vix_value < self.HIGH_THRESHOLD:
            return VIXLevel.HIGH
        else:
            return VIXLevel.EXTREME
    
    def get_vix_signal(self) -> VIXReading:
        """
        Get VIX reading with trading signal.
        
        Returns:
            VIXReading with value, level, signal and description
        """
        vix = self.get_vix()
        
        if vix is None:
            return VIXReading(
                value=0.0,
                level=VIXLevel.NEUTRAL,
                signal="NO_TRADE",
                description="VIX data unavailable"
            )
        
        level = self.classify_vix(vix)
        
        if level == VIXLevel.LOW:
            return VIXReading(
                value=vix,
                level=level,
                signal="BCS",
                description=f"VIX {vix:.2f} - Low IV, options cheap, favor debit spreads (BCS)"
            )
        
        if level == VIXLevel.NEUTRAL:
            return VIXReading(
                value=vix,
                level=level,
                signal="WAIT",
                description=f"VIX {vix:.2f} - Neutral zone, wait for clearer signal"
            )
        
        if level == VIXLevel.HIGH:
            return VIXReading(
                value=vix,
                level=level,
                signal="BPS",
                description=f"VIX {vix:.2f} - High IV, rich premiums, favor credit spreads (BPS)"
            )
        
        # EXTREME
        return VIXReading(
            value=vix,
            level=level,
            signal="NO_TRADE",
            description=f"VIX {vix:.2f} - Extreme fear, avoid all trades"
        )
    
    def is_safe_to_trade(self) -> Tuple[bool, str]:
        """
        Check if VIX levels are safe for trading.
        
        Returns:
            Tuple of (is_safe, reason)
        """
        vix = self.get_vix()
        
        if vix is None:
            return False, "Cannot fetch VIX data"
        
        if vix >= self.EXTREME_THRESHOLD:
            return False, f"VIX {vix:.2f} >= {self.EXTREME_THRESHOLD} - Market in panic mode"
        
        if vix >= self.HIGH_THRESHOLD:
            return True, f"VIX {vix:.2f} - Elevated but tradeable with caution"
        
        return True, f"VIX {vix:.2f} - Safe to trade"
    
    def should_exit_positions(self) -> Tuple[bool, str]:
        """
        Check if VIX spike warrants exiting positions.
        
        Returns:
            Tuple of (should_exit, reason)
        """
        vix = self.get_vix()
        
        if vix is None:
            return False, "Cannot determine - VIX unavailable"
        
        if vix >= self.EXTREME_THRESHOLD:
            return True, f"VIX {vix:.2f} >= {self.EXTREME_THRESHOLD} - EXIT ALL POSITIONS"
        
        return False, f"VIX {vix:.2f} - No exit signal"
