"""
Base strategy class for option spreads
"""
import logging
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Tuple
from enum import Enum
import datetime

logger = logging.getLogger(__name__)


class OrderSide(Enum):
    BUY = "B"
    SELL = "S"


class OptionType(Enum):
    CALL = "CE"
    PUT = "PE"


@dataclass
class SpreadLeg:
    """Single leg of an option spread"""
    trading_symbol: str
    token: str
    strike: float
    option_type: OptionType
    side: OrderSide
    quantity: int
    ltp: float = 0.0
    entry_price: float = 0.0
    order_id: Optional[str] = None


@dataclass
class SpreadOrder:
    """Complete spread order with both legs"""
    strategy_name: str
    symbol: str
    expiry_date: str
    long_leg: SpreadLeg
    short_leg: SpreadLeg
    spread_width: float = 0.0
    net_premium: float = 0.0  # Positive = credit, Negative = debit
    max_profit: float = 0.0
    max_loss: float = 0.0
    breakeven: float = 0.0
    created_at: datetime.datetime = field(default_factory=datetime.datetime.now)


@dataclass
class SpreadPosition:
    """Active spread position for tracking"""
    spread_order: SpreadOrder
    entry_premium: float
    current_premium: float = 0.0
    unrealized_pnl: float = 0.0
    status: str = "OPEN"
    stop_loss_price: float = 0.0
    target_price: float = 0.0


class BaseStrategy(ABC):
    """
    Abstract base class for option spread strategies.
    """
    
    def __init__(self, shoonya_api, symbol: str = "NIFTY"):
        """
        Initialize strategy.
        
        Args:
            shoonya_api: Shoonya API instance
            symbol: Trading symbol (NIFTY, BANKNIFTY, etc.)
        """
        self.api = shoonya_api
        self.symbol = symbol
        self.positions: List[SpreadPosition] = []
        
        # Import required utilities
        from const import INDICES_ROUNDING, LOT_SIZE, EXCHANGE, INDICES_TOKEN
        from utils import get_closest_expiry, get_strike_tsym, download_scrip_master
        
        self.rounding = INDICES_ROUNDING.get(symbol, 50)
        self.lot_size = LOT_SIZE.get(symbol, 50)
        self.exchange = EXCHANGE.get(symbol, "NFO")
        self.token = INDICES_TOKEN.get(symbol)
    
    @property
    @abstractmethod
    def strategy_name(self) -> str:
        """Return strategy name"""
        pass
    
    @abstractmethod
    def calculate_strikes(self, spot_price: float, **kwargs) -> Tuple[float, float]:
        """
        Calculate short and long strike prices.
        
        Args:
            spot_price: Current spot price
            **kwargs: Additional parameters
        
        Returns:
            Tuple of (short_strike, long_strike)
        """
        pass
    
    @abstractmethod
    def create_spread_order(self, quantity: int, **kwargs) -> Optional[SpreadOrder]:
        """
        Create a spread order with calculated strikes.
        
        Args:
            quantity: Number of lots
            **kwargs: Additional parameters
        
        Returns:
            SpreadOrder or None if cannot create
        """
        pass
    
    def get_spot_price(self) -> Optional[float]:
        """Get current spot price"""
        from utils import get_exchange
        
        try:
            exchange = get_exchange(self.symbol, is_index=True)
            ret = self.api.get_quotes(exchange=exchange, token=str(self.token))
            
            if ret and 'lp' in ret:
                return float(ret['lp'])
            
            logger.error(f"Failed to get spot price for {self.symbol}")
            return None
            
        except Exception as e:
            logger.error(f"Error getting spot price: {e}")
            return None
    
    def round_to_strike(self, price: float) -> float:
        """Round price to nearest valid strike"""
        return round(price / self.rounding) * self.rounding
    
    def get_option_quote(self, strike: float, option_type: OptionType, expiry_date: str) -> Optional[Dict]:
        """
        Get quote for an option.
        
        Args:
            strike: Strike price
            option_type: CE or PE
            expiry_date: Expiry date string
        
        Returns:
            Quote dict or None
        """
        from utils import download_scrip_master, get_strike_tsym
        
        try:
            df = download_scrip_master(file_id=f"{self.exchange}_symbols")
            trading_symbol = get_strike_tsym(df, expiry_date, strike, option_type.value)
            token = df[df['TradingSymbol'] == trading_symbol]['Token'].values[0]
            
            ret = self.api.get_quotes(exchange=self.exchange, token=str(token))
            
            if ret:
                ret['trading_symbol'] = trading_symbol
                ret['token'] = str(token)
                ret['strike'] = strike
                return ret
            
            return None
            
        except Exception as e:
            logger.error(f"Error getting option quote: {e}")
            return None
    
    def place_spread_order(self, spread_order: SpreadOrder, product_type: str = "H") -> bool:
        """
        Place both legs of the spread order.
        
        Args:
            spread_order: SpreadOrder object
            product_type: Product type (H for hedge/NRML)
        
        Returns:
            True if both orders placed successfully
        """
        try:
            # Place long leg first (for credit spreads this is the hedge)
            long_ret = self.api.place_order(
                buy_or_sell=spread_order.long_leg.side.value,
                product_type=product_type,
                exchange=self.exchange,
                tradingsymbol=spread_order.long_leg.trading_symbol,
                quantity=spread_order.long_leg.quantity,
                discloseqty=0,
                price_type="LMT",
                price=spread_order.long_leg.ltp,
                trigger_price=None,
                retention="DAY",
                remarks=f"{self.strategy_name}|LONG"
            )
            
            if long_ret and long_ret.get('stat') == 'Ok':
                spread_order.long_leg.order_id = long_ret.get('norenordno')
                logger.info(f"Long leg order placed: {spread_order.long_leg.order_id}")
            else:
                logger.error(f"Failed to place long leg: {long_ret}")
                return False
            
            # Place short leg
            short_ret = self.api.place_order(
                buy_or_sell=spread_order.short_leg.side.value,
                product_type=product_type,
                exchange=self.exchange,
                tradingsymbol=spread_order.short_leg.trading_symbol,
                quantity=spread_order.short_leg.quantity,
                discloseqty=0,
                price_type="LMT",
                price=spread_order.short_leg.ltp,
                trigger_price=None,
                retention="DAY",
                remarks=f"{self.strategy_name}|SHORT"
            )
            
            if short_ret and short_ret.get('stat') == 'Ok':
                spread_order.short_leg.order_id = short_ret.get('norenordno')
                logger.info(f"Short leg order placed: {spread_order.short_leg.order_id}")
                return True
            else:
                logger.error(f"Failed to place short leg: {short_ret}")
                # TODO: Cancel long leg order if short fails
                return False
                
        except Exception as e:
            logger.error(f"Error placing spread order: {e}")
            return False
    
    def calculate_pnl(self, position: SpreadPosition) -> float:
        """Calculate current P&L for a position"""
        # Get current prices
        long_quote = self.api.get_quotes(
            exchange=self.exchange,
            token=position.spread_order.long_leg.token
        )
        short_quote = self.api.get_quotes(
            exchange=self.exchange,
            token=position.spread_order.short_leg.token
        )
        
        if not long_quote or not short_quote:
            return 0.0
        
        long_price = float(long_quote.get('lp', 0))
        short_price = float(short_quote.get('lp', 0))
        
        # For credit spread: profit when premium decreases
        # For debit spread: profit when premium increases
        position.current_premium = short_price - long_price
        position.unrealized_pnl = (position.entry_premium - position.current_premium) * position.spread_order.long_leg.quantity
        
        return position.unrealized_pnl
