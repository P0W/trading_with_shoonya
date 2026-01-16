"""
Stop loss monitoring and management for spread positions
"""
import logging
from dataclasses import dataclass
from typing import Dict, List, Optional, Callable, Tuple
from enum import Enum
import datetime
import threading
import time

logger = logging.getLogger(__name__)


class ExitReason(Enum):
    """Reason for position exit"""
    STOP_LOSS = "STOP_LOSS"
    TARGET = "TARGET"
    TIME_EXIT = "TIME_EXIT"
    MANUAL = "MANUAL"
    EMERGENCY = "EMERGENCY"


@dataclass
class StopLossConfig:
    """Stop loss configuration"""
    sl_multiplier: float = 2.0  # For credit spreads: exit when premium doubles
    target_percent: float = 0.50  # Capture 50% of max profit
    trailing_enabled: bool = False
    trailing_percent: float = 0.30  # Trail by 30%
    time_exit_enabled: bool = True
    time_exit_hour: int = 15  # Exit at 3 PM
    time_exit_minute: int = 15


@dataclass
class MonitoredPosition:
    """Position being monitored"""
    position_id: str
    entry_premium: float
    stop_loss_premium: float
    target_premium: float
    current_premium: float = 0.0
    long_token: str = ""
    short_token: str = ""
    exchange: str = "NFO"
    quantity: int = 0
    strategy_type: str = "BPS"
    entry_time: datetime.datetime = None
    last_update: datetime.datetime = None
    trailing_sl: Optional[float] = None


class StopLossManager:
    """
    Monitor and manage stop losses for spread positions.
    
    Features:
    - Real-time premium monitoring
    - Automatic SL/Target exits
    - Trailing stop loss
    - Time-based exits
    - Emergency exit capability
    """
    
    def __init__(
        self,
        shoonya_api,
        config: Optional[StopLossConfig] = None,
        on_exit: Optional[Callable[[MonitoredPosition, ExitReason], None]] = None,
        check_interval: int = 5
    ):
        """
        Initialize stop loss manager.
        
        Args:
            shoonya_api: Shoonya API instance
            config: Stop loss configuration
            on_exit: Callback when position is exited
            check_interval: Seconds between checks
        """
        self.api = shoonya_api
        self.config = config or StopLossConfig()
        self.on_exit = on_exit
        self.check_interval = check_interval
        
        self.positions: Dict[str, MonitoredPosition] = {}
        self._running = False
        self._monitor_thread: Optional[threading.Thread] = None
    
    def add_position(
        self,
        position_id: str,
        entry_premium: float,
        long_token: str,
        short_token: str,
        quantity: int,
        exchange: str = "NFO",
        strategy_type: str = "BPS",
        custom_sl: Optional[float] = None,
        custom_target: Optional[float] = None
    ):
        """
        Add a position to monitor.
        
        Args:
            position_id: Unique identifier for the position
            entry_premium: Entry net premium (positive = credit)
            long_token: Token for long leg
            short_token: Token for short leg
            quantity: Position quantity
            exchange: Exchange (NFO, BFO)
            strategy_type: BPS or BCS
            custom_sl: Custom stop loss level
            custom_target: Custom target level
        """
        if strategy_type == "BPS":
            # Credit spread: SL when premium expands
            stop_loss = custom_sl or (entry_premium * self.config.sl_multiplier)
            # Target when premium shrinks
            target = custom_target or (entry_premium * (1 - self.config.target_percent))
        else:
            # Debit spread: SL when premium shrinks
            stop_loss = custom_sl or (entry_premium * (1 - self.config.sl_multiplier / 2))
            # Target when premium expands
            target = custom_target or (entry_premium * (1 + self.config.target_percent))
        
        position = MonitoredPosition(
            position_id=position_id,
            entry_premium=entry_premium,
            stop_loss_premium=stop_loss,
            target_premium=target,
            long_token=long_token,
            short_token=short_token,
            exchange=exchange,
            quantity=quantity,
            strategy_type=strategy_type,
            entry_time=datetime.datetime.now(),
            last_update=datetime.datetime.now()
        )
        
        self.positions[position_id] = position
        
        logger.info(
            f"Position {position_id} added | Entry: {entry_premium:.2f} | "
            f"SL: {stop_loss:.2f} | Target: {target:.2f}"
        )
    
    def remove_position(self, position_id: str):
        """Remove a position from monitoring"""
        if position_id in self.positions:
            del self.positions[position_id]
            logger.info(f"Position {position_id} removed from monitoring")
    
    def get_current_premium(self, position: MonitoredPosition) -> Optional[float]:
        """
        Get current net premium for a position.
        
        Returns:
            Current premium (short_price - long_price) or None
        """
        try:
            long_quote = self.api.get_quotes(
                exchange=position.exchange,
                token=position.long_token
            )
            short_quote = self.api.get_quotes(
                exchange=position.exchange,
                token=position.short_token
            )
            
            if not long_quote or not short_quote:
                return None
            
            long_price = float(long_quote.get('lp', 0))
            short_price = float(short_quote.get('lp', 0))
            
            # Net premium: what we'd receive if we closed now
            # For credit spread: short - long
            return short_price - long_price
            
        except Exception as e:
            logger.error(f"Error fetching premium: {e}")
            return None
    
    def check_position(self, position: MonitoredPosition) -> Tuple[bool, Optional[ExitReason]]:
        """
        Check if a position should be exited.
        
        Args:
            position: Position to check
        
        Returns:
            Tuple of (should_exit, exit_reason)
        """
        current_premium = self.get_current_premium(position)
        
        if current_premium is None:
            return False, None
        
        position.current_premium = current_premium
        position.last_update = datetime.datetime.now()
        
        # Log current status
        pnl = (position.entry_premium - current_premium) * position.quantity
        logger.debug(
            f"{position.position_id} | Entry: {position.entry_premium:.2f} | "
            f"Current: {current_premium:.2f} | P&L: ₹{pnl:.2f}"
        )
        
        if position.strategy_type == "BPS":
            # Credit spread checks
            
            # Stop loss: premium expanded (we'd have to pay more to close)
            if current_premium >= position.stop_loss_premium:
                logger.warning(
                    f"STOP LOSS HIT for {position.position_id} | "
                    f"Current: {current_premium:.2f} >= SL: {position.stop_loss_premium:.2f}"
                )
                return True, ExitReason.STOP_LOSS
            
            # Target: premium shrunk (we pay less to close)
            if current_premium <= position.target_premium:
                logger.info(
                    f"TARGET HIT for {position.position_id} | "
                    f"Current: {current_premium:.2f} <= Target: {position.target_premium:.2f}"
                )
                return True, ExitReason.TARGET
            
            # Update trailing stop if enabled
            if self.config.trailing_enabled and current_premium < position.entry_premium:
                profit_captured = position.entry_premium - current_premium
                new_trailing_sl = current_premium + (profit_captured * self.config.trailing_percent)
                
                if position.trailing_sl is None or new_trailing_sl < position.trailing_sl:
                    position.trailing_sl = new_trailing_sl
                    position.stop_loss_premium = min(position.stop_loss_premium, new_trailing_sl)
                    logger.info(f"Trailing SL updated to {new_trailing_sl:.2f}")
        
        else:
            # Debit spread checks (BCS)
            
            # Stop loss: premium shrunk (our spread lost value)
            if current_premium <= position.stop_loss_premium:
                logger.warning(f"STOP LOSS HIT for {position.position_id}")
                return True, ExitReason.STOP_LOSS
            
            # Target: premium expanded (our spread gained value)
            if current_premium >= position.target_premium:
                logger.info(f"TARGET HIT for {position.position_id}")
                return True, ExitReason.TARGET
        
        # Time-based exit
        if self.config.time_exit_enabled:
            now = datetime.datetime.now()
            exit_time = now.replace(
                hour=self.config.time_exit_hour,
                minute=self.config.time_exit_minute,
                second=0
            )
            if now >= exit_time:
                logger.info(f"TIME EXIT for {position.position_id}")
                return True, ExitReason.TIME_EXIT
        
        return False, None
    
    def check_all_positions(self) -> List[Tuple[MonitoredPosition, ExitReason]]:
        """
        Check all monitored positions.
        
        Returns:
            List of (position, exit_reason) for positions that should exit
        """
        exits = []
        
        for position_id, position in list(self.positions.items()):
            should_exit, reason = self.check_position(position)
            
            if should_exit and reason:
                exits.append((position, reason))
                
                if self.on_exit:
                    self.on_exit(position, reason)
        
        return exits
    
    def emergency_exit_all(self, reason: str = "Emergency"):
        """Trigger emergency exit for all positions"""
        logger.critical(f"EMERGENCY EXIT ALL: {reason}")
        
        for position_id, position in list(self.positions.items()):
            if self.on_exit:
                self.on_exit(position, ExitReason.EMERGENCY)
    
    def start_monitoring(self):
        """Start background monitoring thread"""
        if self._running:
            logger.warning("Monitoring already running")
            return
        
        self._running = True
        self._monitor_thread = threading.Thread(
            target=self._monitor_loop,
            daemon=True
        )
        self._monitor_thread.start()
        logger.info("Stop loss monitoring started")
    
    def stop_monitoring(self):
        """Stop background monitoring"""
        self._running = False
        if self._monitor_thread:
            self._monitor_thread.join(timeout=10)
        logger.info("Stop loss monitoring stopped")
    
    def _monitor_loop(self):
        """Background monitoring loop"""
        while self._running:
            try:
                if self.positions:
                    self.check_all_positions()
                time.sleep(self.check_interval)
            except Exception as e:
                logger.error(f"Error in monitor loop: {e}")
                time.sleep(self.check_interval)
    
    def get_position_status(self, position_id: str) -> Optional[Dict]:
        """Get current status of a position"""
        position = self.positions.get(position_id)
        if not position:
            return None
        
        current_premium = self.get_current_premium(position)
        pnl = 0
        pnl_percent = 0
        
        if current_premium:
            pnl = (position.entry_premium - current_premium) * position.quantity
            if position.entry_premium != 0:
                pnl_percent = ((position.entry_premium - current_premium) / position.entry_premium) * 100
        
        return {
            "position_id": position_id,
            "strategy": position.strategy_type,
            "entry_premium": position.entry_premium,
            "current_premium": current_premium,
            "stop_loss": position.stop_loss_premium,
            "target": position.target_premium,
            "pnl": pnl,
            "pnl_percent": pnl_percent,
            "entry_time": position.entry_time,
            "last_update": position.last_update
        }
