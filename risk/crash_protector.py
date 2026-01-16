"""
Crash protection and circuit breakers for risk management
"""
import logging
from dataclasses import dataclass
from typing import Optional, Tuple, Callable, List
from enum import Enum
import datetime

logger = logging.getLogger(__name__)


class AlertLevel(Enum):
    """Alert severity levels"""
    INFO = "INFO"
    WARNING = "WARNING"
    CRITICAL = "CRITICAL"
    EMERGENCY = "EMERGENCY"


@dataclass
class ProtectionTrigger:
    """Protection trigger event"""
    level: AlertLevel
    message: str
    action: str
    timestamp: datetime.datetime


class CrashProtector:
    """
    Multi-layer crash protection system.
    
    Layers:
    1. VIX Monitoring - Exit on extreme VIX
    2. Price Circuit Breakers - Exit on large intraday moves
    3. Time-based Protection - No trades before events
    4. Portfolio Heat - Max positions and risk limits
    """
    
    # VIX thresholds
    VIX_CAUTION = 18.0
    VIX_WARNING = 20.0
    VIX_CRITICAL = 25.0
    
    # Price move thresholds (intraday %)
    PRICE_ALERT = 1.0
    PRICE_WARNING = 1.5
    PRICE_CRITICAL = 2.0
    
    # Portfolio limits
    MAX_OPEN_POSITIONS = 2
    MAX_PORTFOLIO_RISK_PERCENT = 5.0
    
    def __init__(
        self,
        shoonya_api,
        symbol: str = "NIFTY",
        on_alert: Optional[Callable[[ProtectionTrigger], None]] = None
    ):
        """
        Initialize crash protector.
        
        Args:
            shoonya_api: Shoonya API instance
            symbol: Symbol to monitor
            on_alert: Callback function for alerts
        """
        self.api = shoonya_api
        self.symbol = symbol
        self.on_alert = on_alert
        self.triggers: List[ProtectionTrigger] = []
        self.day_open_price: Optional[float] = None
        
        from indicators.vix_monitor import VIXMonitor
        self.vix_monitor = VIXMonitor(shoonya_api)
    
    def _trigger_alert(self, level: AlertLevel, message: str, action: str):
        """Create and process an alert trigger"""
        trigger = ProtectionTrigger(
            level=level,
            message=message,
            action=action,
            timestamp=datetime.datetime.now()
        )
        self.triggers.append(trigger)
        
        log_method = {
            AlertLevel.INFO: logger.info,
            AlertLevel.WARNING: logger.warning,
            AlertLevel.CRITICAL: logger.critical,
            AlertLevel.EMERGENCY: logger.critical
        }.get(level, logger.info)
        
        log_method(f"[{level.value}] {message} | Action: {action}")
        
        if self.on_alert:
            self.on_alert(trigger)
        
        return trigger
    
    def check_vix(self) -> Tuple[bool, Optional[ProtectionTrigger]]:
        """
        Check VIX levels and trigger alerts if needed.
        
        Returns:
            Tuple of (is_safe_to_trade, trigger_if_any)
        """
        vix = self.vix_monitor.get_vix()
        
        if vix is None:
            return False, self._trigger_alert(
                AlertLevel.WARNING,
                "Cannot fetch VIX data",
                "HALT_NEW_TRADES"
            )
        
        if vix >= self.VIX_CRITICAL:
            return False, self._trigger_alert(
                AlertLevel.EMERGENCY,
                f"VIX {vix:.2f} >= {self.VIX_CRITICAL} - MARKET PANIC",
                "EXIT_ALL_POSITIONS"
            )
        
        if vix >= self.VIX_WARNING:
            return False, self._trigger_alert(
                AlertLevel.CRITICAL,
                f"VIX {vix:.2f} >= {self.VIX_WARNING} - High volatility",
                "HALT_NEW_TRADES"
            )
        
        if vix >= self.VIX_CAUTION:
            return True, self._trigger_alert(
                AlertLevel.WARNING,
                f"VIX {vix:.2f} >= {self.VIX_CAUTION} - Elevated",
                "TIGHTEN_STOPS"
            )
        
        return True, None
    
    def check_price_move(self) -> Tuple[bool, Optional[ProtectionTrigger]]:
        """
        Check intraday price move and trigger alerts.
        
        Returns:
            Tuple of (is_safe, trigger_if_any)
        """
        from utils import get_exchange
        from const import INDICES_TOKEN
        
        try:
            exchange = get_exchange(self.symbol, is_index=True)
            token = INDICES_TOKEN.get(self.symbol)
            
            quote = self.api.get_quotes(exchange=exchange, token=str(token))
            
            if not quote:
                return True, None
            
            current_price = float(quote.get('lp', 0))
            open_price = float(quote.get('o', current_price))
            
            if open_price == 0:
                return True, None
            
            self.day_open_price = open_price
            change_percent = ((current_price - open_price) / open_price) * 100
            
            # Check for drops (negative moves)
            if change_percent <= -self.PRICE_CRITICAL:
                return False, self._trigger_alert(
                    AlertLevel.EMERGENCY,
                    f"{self.symbol} down {abs(change_percent):.2f}% - Potential crash",
                    "EXIT_BPS_POSITIONS"
                )
            
            if change_percent <= -self.PRICE_WARNING:
                return False, self._trigger_alert(
                    AlertLevel.CRITICAL,
                    f"{self.symbol} down {abs(change_percent):.2f}% - Significant drop",
                    "HALT_NEW_TRADES"
                )
            
            if change_percent <= -self.PRICE_ALERT:
                return True, self._trigger_alert(
                    AlertLevel.WARNING,
                    f"{self.symbol} down {abs(change_percent):.2f}%",
                    "MONITOR_CLOSELY"
                )
            
            return True, None
            
        except Exception as e:
            logger.error(f"Error checking price move: {e}")
            return True, None
    
    def check_time_restrictions(self) -> Tuple[bool, Optional[ProtectionTrigger]]:
        """
        Check time-based trading restrictions.
        
        Returns:
            Tuple of (can_trade, trigger_if_any)
        """
        now = datetime.datetime.now()
        
        # No trading on expiry day (Thursday)
        if now.weekday() == 3:  # Thursday
            return False, self._trigger_alert(
                AlertLevel.INFO,
                "Expiry day - No new trades",
                "SKIP_ENTRY"
            )
        
        # No trading 30 mins before market close
        market_close = now.replace(hour=15, minute=30, second=0)
        if now >= (market_close - datetime.timedelta(minutes=30)):
            return False, self._trigger_alert(
                AlertLevel.INFO,
                "Too close to market close",
                "SKIP_ENTRY"
            )
        
        # Reduced position on Mondays (gap risk)
        if now.weekday() == 0:  # Monday
            return True, self._trigger_alert(
                AlertLevel.INFO,
                "Monday - Gap risk elevated",
                "REDUCE_POSITION_SIZE"
            )
        
        return True, None
    
    def check_portfolio_heat(
        self,
        open_positions: int,
        current_risk: float,
        capital: float
    ) -> Tuple[bool, Optional[ProtectionTrigger]]:
        """
        Check portfolio-level risk limits.
        
        Args:
            open_positions: Number of open positions
            current_risk: Total risk from open positions
            capital: Trading capital
        
        Returns:
            Tuple of (can_add_position, trigger_if_any)
        """
        risk_percent = (current_risk / capital) * 100
        
        if open_positions >= self.MAX_OPEN_POSITIONS:
            return False, self._trigger_alert(
                AlertLevel.WARNING,
                f"Max positions ({self.MAX_OPEN_POSITIONS}) reached",
                "SKIP_ENTRY"
            )
        
        if risk_percent >= self.MAX_PORTFOLIO_RISK_PERCENT:
            return False, self._trigger_alert(
                AlertLevel.WARNING,
                f"Portfolio risk {risk_percent:.2f}% >= {self.MAX_PORTFOLIO_RISK_PERCENT}%",
                "SKIP_ENTRY"
            )
        
        return True, None
    
    def run_all_checks(
        self,
        open_positions: int = 0,
        current_risk: float = 0.0,
        capital: float = 100000.0
    ) -> Tuple[bool, List[ProtectionTrigger]]:
        """
        Run all protection checks.
        
        Returns:
            Tuple of (can_trade, list_of_triggers)
        """
        triggers = []
        can_trade = True
        
        # VIX check
        vix_safe, vix_trigger = self.check_vix()
        if vix_trigger:
            triggers.append(vix_trigger)
        if not vix_safe:
            can_trade = False
        
        # Price check
        price_safe, price_trigger = self.check_price_move()
        if price_trigger:
            triggers.append(price_trigger)
        if not price_safe:
            can_trade = False
        
        # Time check
        time_ok, time_trigger = self.check_time_restrictions()
        if time_trigger:
            triggers.append(time_trigger)
        if not time_ok:
            can_trade = False
        
        # Portfolio check
        portfolio_ok, portfolio_trigger = self.check_portfolio_heat(
            open_positions, current_risk, capital
        )
        if portfolio_trigger:
            triggers.append(portfolio_trigger)
        if not portfolio_ok:
            can_trade = False
        
        return can_trade, triggers
    
    def should_exit_all(self) -> Tuple[bool, str]:
        """
        Determine if all positions should be exited immediately.
        
        Returns:
            Tuple of (should_exit, reason)
        """
        # Check VIX
        vix = self.vix_monitor.get_vix()
        if vix and vix >= self.VIX_CRITICAL:
            return True, f"VIX {vix:.2f} >= {self.VIX_CRITICAL} - Emergency exit"
        
        # Check price crash
        _, price_trigger = self.check_price_move()
        if price_trigger and price_trigger.level == AlertLevel.EMERGENCY:
            return True, price_trigger.message
        
        return False, "No emergency exit condition"
