"""
Market hours and trading session management
"""
import logging
from dataclasses import dataclass
from typing import Optional, Tuple, List
from enum import Enum
import datetime

logger = logging.getLogger(__name__)


class SessionType(Enum):
    """Trading session types"""
    PRE_MARKET = "PRE_MARKET"
    MARKET_OPEN = "MARKET_OPEN"
    TRADING = "TRADING"
    CLOSING = "CLOSING"
    POST_MARKET = "POST_MARKET"
    CLOSED = "CLOSED"


@dataclass
class TradingSession:
    """Trading session details"""
    session_type: SessionType
    start_time: datetime.time
    end_time: datetime.time
    can_enter: bool
    can_exit: bool
    description: str


class MarketHours:
    """
    Manage market hours, holidays, and trading sessions.
    
    NSE/NFO Market Hours:
    - Pre-open: 9:00 - 9:15
    - Regular trading: 9:15 - 15:30
    
    For options trading:
    - Best entry: 9:30 - 10:30
    - Monitoring: 10:30 - 15:00
    - Exit window: 15:00 - 15:20
    """
    
    # Market timing
    PRE_MARKET_START = datetime.time(9, 0)
    MARKET_OPEN = datetime.time(9, 15)
    ENTRY_WINDOW_START = datetime.time(9, 30)
    ENTRY_WINDOW_END = datetime.time(10, 30)
    CLOSING_START = datetime.time(15, 0)
    MARKET_CLOSE = datetime.time(15, 30)
    POST_MARKET_END = datetime.time(16, 0)
    
    # NSE Holidays 2026 (approximate - should be updated)
    HOLIDAYS_2026 = [
        datetime.date(2026, 1, 26),   # Republic Day
        datetime.date(2026, 3, 17),   # Holi
        datetime.date(2026, 4, 3),    # Good Friday
        datetime.date(2026, 4, 14),   # Dr. Ambedkar Jayanti
        datetime.date(2026, 4, 21),   # Ram Navami
        datetime.date(2026, 5, 1),    # May Day
        datetime.date(2026, 7, 17),   # Muharram
        datetime.date(2026, 8, 15),   # Independence Day
        datetime.date(2026, 10, 2),   # Gandhi Jayanti
        datetime.date(2026, 10, 21),  # Diwali (Lakshmi Puja)
        datetime.date(2026, 10, 22),  # Diwali Balipratipada
        datetime.date(2026, 11, 4),   # Guru Nanak Jayanti
        datetime.date(2026, 12, 25),  # Christmas
    ]
    
    def __init__(self, holidays: Optional[List[datetime.date]] = None):
        """
        Initialize market hours manager.
        
        Args:
            holidays: List of holiday dates (uses default if not provided)
        """
        self.holidays = holidays or self.HOLIDAYS_2026
        
        self.sessions = [
            TradingSession(
                SessionType.PRE_MARKET,
                self.PRE_MARKET_START,
                self.MARKET_OPEN,
                can_enter=False,
                can_exit=False,
                description="Pre-market - Prepare indicators"
            ),
            TradingSession(
                SessionType.MARKET_OPEN,
                self.MARKET_OPEN,
                self.ENTRY_WINDOW_START,
                can_enter=False,
                can_exit=True,
                description="Market just opened - Wait for stability"
            ),
            TradingSession(
                SessionType.TRADING,
                self.ENTRY_WINDOW_START,
                self.ENTRY_WINDOW_END,
                can_enter=True,
                can_exit=True,
                description="Optimal entry window"
            ),
            TradingSession(
                SessionType.TRADING,
                self.ENTRY_WINDOW_END,
                self.CLOSING_START,
                can_enter=False,
                can_exit=True,
                description="Position monitoring - No new entries"
            ),
            TradingSession(
                SessionType.CLOSING,
                self.CLOSING_START,
                self.MARKET_CLOSE,
                can_enter=False,
                can_exit=True,
                description="Closing session - Exit if needed"
            ),
            TradingSession(
                SessionType.POST_MARKET,
                self.MARKET_CLOSE,
                self.POST_MARKET_END,
                can_enter=False,
                can_exit=False,
                description="Post-market - Review and analysis"
            ),
        ]
    
    def is_holiday(self, date: Optional[datetime.date] = None) -> bool:
        """Check if a date is a market holiday"""
        date = date or datetime.date.today()
        return date in self.holidays
    
    def is_weekend(self, date: Optional[datetime.date] = None) -> bool:
        """Check if a date is a weekend"""
        date = date or datetime.date.today()
        return date.weekday() >= 5  # Saturday = 5, Sunday = 6
    
    def is_trading_day(self, date: Optional[datetime.date] = None) -> bool:
        """Check if a date is a trading day"""
        date = date or datetime.date.today()
        return not self.is_weekend(date) and not self.is_holiday(date)
    
    def is_expiry_day(self, date: Optional[datetime.date] = None) -> bool:
        """Check if a date is weekly expiry (Thursday for NIFTY)"""
        date = date or datetime.date.today()
        return date.weekday() == 3  # Thursday
    
    def get_current_session(self) -> TradingSession:
        """Get the current trading session"""
        now = datetime.datetime.now()
        current_time = now.time()
        
        if not self.is_trading_day(now.date()):
            return TradingSession(
                SessionType.CLOSED,
                datetime.time(0, 0),
                datetime.time(23, 59),
                can_enter=False,
                can_exit=False,
                description="Market closed (weekend/holiday)"
            )
        
        for session in self.sessions:
            if session.start_time <= current_time < session.end_time:
                return session
        
        # Outside market hours
        return TradingSession(
            SessionType.CLOSED,
            datetime.time(0, 0),
            datetime.time(23, 59),
            can_enter=False,
            can_exit=False,
            description="Market closed"
        )
    
    def can_enter_trade(self) -> Tuple[bool, str]:
        """
        Check if new trades can be entered now.
        
        Returns:
            Tuple of (can_enter, reason)
        """
        session = self.get_current_session()
        
        if not session.can_enter:
            return False, session.description
        
        # Additional check: no entry on expiry day
        if self.is_expiry_day():
            return False, "No new entries on expiry day"
        
        return True, "Entry window open"
    
    def can_exit_trade(self) -> Tuple[bool, str]:
        """
        Check if trades can be exited now.
        
        Returns:
            Tuple of (can_exit, reason)
        """
        session = self.get_current_session()
        
        if not session.can_exit:
            return False, session.description
        
        return True, "Exit allowed"
    
    def time_until_market_open(self) -> Optional[datetime.timedelta]:
        """Get time until market opens"""
        now = datetime.datetime.now()
        
        if not self.is_trading_day():
            # Find next trading day
            next_day = now.date() + datetime.timedelta(days=1)
            while not self.is_trading_day(next_day):
                next_day += datetime.timedelta(days=1)
            
            market_open = datetime.datetime.combine(next_day, self.MARKET_OPEN)
            return market_open - now
        
        current_time = now.time()
        
        if current_time < self.MARKET_OPEN:
            market_open = datetime.datetime.combine(now.date(), self.MARKET_OPEN)
            return market_open - now
        
        if current_time >= self.MARKET_CLOSE:
            # Find next trading day
            next_day = now.date() + datetime.timedelta(days=1)
            while not self.is_trading_day(next_day):
                next_day += datetime.timedelta(days=1)
            
            market_open = datetime.datetime.combine(next_day, self.MARKET_OPEN)
            return market_open - now
        
        return None  # Market is open
    
    def time_until_entry_window(self) -> Optional[datetime.timedelta]:
        """Get time until entry window starts"""
        now = datetime.datetime.now()
        current_time = now.time()
        
        if not self.is_trading_day():
            return None
        
        if current_time < self.ENTRY_WINDOW_START:
            entry_start = datetime.datetime.combine(now.date(), self.ENTRY_WINDOW_START)
            return entry_start - now
        
        if current_time >= self.ENTRY_WINDOW_END:
            return None  # Entry window passed
        
        return datetime.timedelta(0)  # In entry window
    
    def get_next_trading_day(self) -> datetime.date:
        """Get the next trading day"""
        next_day = datetime.date.today() + datetime.timedelta(days=1)
        while not self.is_trading_day(next_day):
            next_day += datetime.timedelta(days=1)
        return next_day
    
    def get_market_status(self) -> dict:
        """Get comprehensive market status"""
        now = datetime.datetime.now()
        session = self.get_current_session()
        can_enter, enter_reason = self.can_enter_trade()
        can_exit, exit_reason = self.can_exit_trade()
        
        return {
            "current_time": now.strftime("%H:%M:%S"),
            "date": now.strftime("%Y-%m-%d"),
            "day": now.strftime("%A"),
            "is_trading_day": self.is_trading_day(),
            "is_expiry_day": self.is_expiry_day(),
            "is_holiday": self.is_holiday(),
            "session": session.session_type.value,
            "session_description": session.description,
            "can_enter": can_enter,
            "enter_reason": enter_reason,
            "can_exit": can_exit,
            "exit_reason": exit_reason,
            "time_until_open": str(self.time_until_market_open()),
        }
    
    def print_status(self):
        """Print current market status"""
        status = self.get_market_status()
        
        print("\n" + "="*50)
        print("MARKET STATUS")
        print("="*50)
        print(f"Time: {status['current_time']} | {status['day']}, {status['date']}")
        print(f"Session: {status['session']} - {status['session_description']}")
        print(f"Trading Day: {'Yes' if status['is_trading_day'] else 'No'}")
        print(f"Expiry Day: {'Yes' if status['is_expiry_day'] else 'No'}")
        print(f"Can Enter: {'✓' if status['can_enter'] else '✗'} ({status['enter_reason']})")
        print(f"Can Exit: {'✓' if status['can_exit'] else '✗'} ({status['exit_reason']})")
        if status['time_until_open']:
            print(f"Time until market open: {status['time_until_open']}")
        print("="*50 + "\n")
