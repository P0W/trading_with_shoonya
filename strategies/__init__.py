"""
Trading strategies for NIFTY options spreads
"""
from strategies.base_strategy import BaseStrategy, SpreadOrder, SpreadPosition
from strategies.spread_selector import SpreadSelector, StrategySignal
from strategies.bull_put_spread import BullPutSpread
from strategies.bull_call_spread import BullCallSpread

__all__ = [
    "BaseStrategy",
    "SpreadOrder",
    "SpreadPosition",
    "SpreadSelector",
    "StrategySignal",
    "BullPutSpread",
    "BullCallSpread",
]
