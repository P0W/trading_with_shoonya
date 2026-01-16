"""
Position sizing based on risk parameters
"""
import logging
from dataclasses import dataclass
from typing import Optional, Tuple

logger = logging.getLogger(__name__)


@dataclass
class PositionSize:
    """Position size calculation result"""
    lots: int
    quantity: int
    max_risk: float
    margin_required: float
    risk_percent: float


class PositionSizer:
    """
    Calculate optimal position size based on:
    - Available capital
    - Max risk per trade
    - Strategy max loss
    """
    
    def __init__(
        self,
        capital: float,
        max_risk_percent: float = 2.0,
        max_lots: int = 10
    ):
        """
        Initialize position sizer.
        
        Args:
            capital: Total trading capital
            max_risk_percent: Maximum risk per trade as % of capital
            max_lots: Maximum lots allowed per trade
        """
        self.capital = capital
        self.max_risk_percent = max_risk_percent
        self.max_lots = max_lots
        
        logger.info(
            f"Position Sizer initialized: Capital=₹{capital:,.0f}, "
            f"Max Risk={max_risk_percent}%, Max Lots={max_lots}"
        )
    
    def calculate_lots(
        self,
        max_loss_per_lot: float,
        lot_size: int,
        margin_per_lot: Optional[float] = None
    ) -> PositionSize:
        """
        Calculate number of lots based on risk.
        
        Args:
            max_loss_per_lot: Maximum loss per lot for the strategy
            lot_size: Lot size for the instrument
            margin_per_lot: Margin required per lot (optional)
        
        Returns:
            PositionSize with calculated values
        """
        # Calculate max risk amount
        max_risk_amount = self.capital * (self.max_risk_percent / 100)
        
        # Calculate lots based on risk
        risk_based_lots = int(max_risk_amount / max_loss_per_lot)
        
        # Apply lot limits
        lots = min(risk_based_lots, self.max_lots)
        lots = max(lots, 1)  # Minimum 1 lot
        
        # Check margin if provided
        if margin_per_lot:
            margin_based_lots = int(self.capital * 0.8 / margin_per_lot)  # Use 80% of capital for margin
            lots = min(lots, margin_based_lots)
            lots = max(lots, 1)
        
        quantity = lots * lot_size
        actual_max_risk = lots * max_loss_per_lot
        actual_risk_percent = (actual_max_risk / self.capital) * 100
        margin_required = (margin_per_lot or 0) * lots
        
        result = PositionSize(
            lots=lots,
            quantity=quantity,
            max_risk=actual_max_risk,
            margin_required=margin_required,
            risk_percent=actual_risk_percent
        )
        
        logger.info(
            f"Position Size: {lots} lots ({quantity} qty), "
            f"Max Risk: ₹{actual_max_risk:,.0f} ({actual_risk_percent:.2f}%)"
        )
        
        return result
    
    def calculate_for_spread(
        self,
        spread_width: float,
        premium: float,
        lot_size: int,
        is_credit_spread: bool = True,
        margin_per_lot: Optional[float] = None
    ) -> PositionSize:
        """
        Calculate position size for a spread strategy.
        
        Args:
            spread_width: Width between strikes
            premium: Net premium (positive for credit, negative for debit)
            lot_size: Lot size
            is_credit_spread: True for credit spreads (BPS), False for debit (BCS)
            margin_per_lot: Margin requirement per lot
        
        Returns:
            PositionSize
        """
        if is_credit_spread:
            # Credit spread: Max loss = spread_width - premium
            max_loss_per_lot = (spread_width - premium) * lot_size
        else:
            # Debit spread: Max loss = premium paid
            max_loss_per_lot = abs(premium) * lot_size
        
        return self.calculate_lots(max_loss_per_lot, lot_size, margin_per_lot)
    
    def validate_position(
        self,
        position_size: PositionSize,
        existing_risk: float = 0.0
    ) -> Tuple[bool, str]:
        """
        Validate if a position can be taken given existing risk.
        
        Args:
            position_size: Proposed position size
            existing_risk: Total risk from existing positions
        
        Returns:
            Tuple of (is_valid, reason)
        """
        total_risk = existing_risk + position_size.max_risk
        total_risk_percent = (total_risk / self.capital) * 100
        
        # Max portfolio risk: 5% of capital
        max_portfolio_risk = 5.0
        
        if total_risk_percent > max_portfolio_risk:
            return False, f"Portfolio risk {total_risk_percent:.2f}% exceeds {max_portfolio_risk}% limit"
        
        # Check if margin available
        if position_size.margin_required > self.capital * 0.8:
            return False, f"Margin ₹{position_size.margin_required:,.0f} exceeds 80% of capital"
        
        return True, f"Position valid. Total portfolio risk: {total_risk_percent:.2f}%"
    
    def update_capital(self, new_capital: float):
        """Update capital after P&L"""
        old_capital = self.capital
        self.capital = new_capital
        change = ((new_capital - old_capital) / old_capital) * 100
        logger.info(f"Capital updated: ₹{old_capital:,.0f} -> ₹{new_capital:,.0f} ({change:+.2f}%)")
