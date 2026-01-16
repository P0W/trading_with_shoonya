# NIFTY Weekly Options - Bull Put Spread (BPS) & Bull Call Spread (BCS) Strategy Plan

## 🎯 Strategy Overview

| Strategy | Market View | Risk | Reward | When to Use |
|----------|------------|------|--------|-------------|
| **Bull Put Spread (BPS)** | Neutral to Bullish | Limited (width - premium) | Limited (premium received) | High IV, expect support to hold |
| **Bull Call Spread (BCS)** | Moderately Bullish | Limited (premium paid) | Limited (width - premium) | Low IV, expect breakout |

---

## 📊 Simple & Effective Indicators for Strategy Selection

### Primary Decision Framework: **3-Indicator System**

```
┌─────────────────────────────────────────────────────────────────┐
│                    STRATEGY SELECTION LOGIC                      │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│   1. RSI(14) Position    → Oversold/Overbought Detection        │
│   2. INDIA VIX Level     → Volatility & Premium Assessment      │
│   3. NIFTY vs 20 EMA     → Trend Confirmation                   │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### Indicator Details

| Indicator | BPS Signal | BCS Signal | Data Source |
|-----------|-----------|-----------|-------------|
| **RSI(14)** | 30-50 (recovering from oversold) | 50-70 (strong momentum) | NIFTY Spot |
| **INDIA VIX** | > 15 (high premium = better credit) | < 13 (cheap options = better debit) | VIX Index |
| **Price vs 20 EMA** | Above EMA (uptrend intact) | Above EMA + recent breakout | NIFTY Spot |

### Decision Matrix

```
┌──────────────────────────────────────────────────────────────────────┐
│                         DECISION MATRIX                              │
├──────────────────────────────────────────────────────────────────────┤
│                                                                       │
│  VIX > 15 + RSI 30-50 + Price > 20 EMA     →  BULL PUT SPREAD (BPS) │
│  VIX < 13 + RSI 50-70 + Price > 20 EMA     →  BULL CALL SPREAD (BCS)│
│  VIX 13-15 (neutral zone)                  →  NO TRADE / WAIT       │
│  RSI > 70 (overbought)                     →  NO TRADE / WAIT       │
│  RSI < 30 (heavily oversold)               →  WAIT FOR RECOVERY     │
│  Price < 20 EMA                            →  NO BULLISH SPREADS    │
│                                                                       │
└──────────────────────────────────────────────────────────────────────┘
```

---

## 🎲 High Probability Strike Selection Logic

### For Bull Put Spread (BPS) - Credit Strategy

```
SELL PUT: Delta ~ 0.20-0.25 (OTM) OR 1 strike below nearest support
BUY PUT:  2-3 strikes below sold put (50-150 points width for NIFTY)

Example:
  NIFTY Spot: 24,000
  Support: 23,800
  
  SELL 23750 PE (just below support)
  BUY  23600 PE (150 points width)
  
  Max Profit: Premium Received
  Max Loss: 150 - Premium
  Probability of Profit: ~75-80%
```

### For Bull Call Spread (BCS) - Debit Strategy

```
BUY CALL:  ATM or 1 strike OTM
SELL CALL: 2-3 strikes above bought call (100-150 points width)

Example:
  NIFTY Spot: 24,000
  Resistance: 24,200
  
  BUY  24000 CE (ATM)
  SELL 24150 CE (just below resistance)
  
  Max Profit: Width - Premium Paid
  Max Loss: Premium Paid
  Breakeven: 24000 + Premium Paid
```

### Strike Selection Summary

| Parameter | BPS | BCS |
|-----------|-----|-----|
| Short Strike | 1-2% OTM from spot | ATM to 0.5% OTM |
| Spread Width | 100-150 points | 100-150 points |
| Delta (Short) | 0.20-0.25 | 0.45-0.50 |
| Reward:Risk | 1:2 to 1:3 | 1:1 to 2:1 |
| Win Rate Target | 70-80% | 50-60% |

---

## 🛡️ Risk Management Framework

### Position Sizing

```python
# Risk per trade: 1-2% of capital
max_risk_per_trade = capital * 0.02  # 2% max

# For BPS: Max Loss = (Width - Premium) * Lot Size * Lots
# For BCS: Max Loss = Premium Paid * Lot Size * Lots

# Calculate lots:
lots = max_risk_per_trade / max_loss_per_lot
lots = min(lots, max_allowed_lots)  # Cap at broker/exchange limits
```

### Stop Loss Rules

| Scenario | Action |
|----------|--------|
| Premium doubles (BPS) | Exit immediately |
| Loss > 1.5x premium received (BPS) | Exit |
| Loss > 50% of premium paid (BCS) | Review, consider exit |
| NIFTY breaks key support/resistance | Exit immediately |
| VIX spikes > 20 suddenly | Reduce position or exit |

### Crash Protection Mechanisms

```
┌─────────────────────────────────────────────────────────────────────┐
│                    CRASH PROTECTION LAYERS                          │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  LAYER 1: VIX Monitoring                                            │
│    - VIX > 20 → No new trades, tighten stops                        │
│    - VIX > 25 → Exit all positions                                  │
│                                                                      │
│  LAYER 2: Price-Based Circuit Breaker                               │
│    - NIFTY falls > 1% intraday → Alert + Review                     │
│    - NIFTY falls > 2% intraday → Exit BPS positions                 │
│                                                                      │
│  LAYER 3: Time-Based Protection                                     │
│    - No new trades 30 mins before major news/events                 │
│    - No new trades on expiry day (Thursday)                         │
│    - Reduced position size on Mondays (gap risk)                    │
│                                                                      │
│  LAYER 4: Portfolio Heat                                            │
│    - Max 2 active spread positions at a time                        │
│    - Max 5% of capital at risk across all positions                 │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 🕐 24x7 System Architecture

### Component Design

```
┌──────────────────────────────────────────────────────────────────────┐
│                      SYSTEM ARCHITECTURE                             │
├──────────────────────────────────────────────────────────────────────┤
│                                                                       │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐              │
│  │  Scheduler  │───▶│  Strategy   │───▶│   Order     │              │
│  │  (APScheduler)   │   Engine    │    │  Executor   │              │
│  └─────────────┘    └─────────────┘    └─────────────┘              │
│         │                  │                  │                      │
│         ▼                  ▼                  ▼                      │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐              │
│  │   Market    │    │  Indicator  │    │  Position   │              │
│  │   Hours     │    │  Calculator │    │  Manager    │              │
│  │   Manager   │    │  (RSI/VIX)  │    │  (Redis)    │              │
│  └─────────────┘    └─────────────┘    └─────────────┘              │
│         │                  │                  │                      │
│         └──────────────────┼──────────────────┘                      │
│                            ▼                                         │
│                   ┌─────────────┐                                    │
│                   │   Alerts    │                                    │
│                   │ (Telegram)  │                                    │
│                   └─────────────┘                                    │
│                                                                       │
└──────────────────────────────────────────────────────────────────────┘
```

### Module Structure (Proposed)

```
trading_with_shoonya/
├── strategies/
│   ├── __init__.py
│   ├── base_strategy.py          # Abstract base class
│   ├── bull_put_spread.py        # BPS implementation
│   ├── bull_call_spread.py       # BCS implementation
│   └── spread_selector.py        # Decision logic (RSI/VIX/EMA)
│
├── indicators/
│   ├── __init__.py
│   ├── rsi.py                    # RSI calculation
│   ├── ema.py                    # EMA calculation
│   └── vix_monitor.py            # VIX fetching & monitoring
│
├── risk/
│   ├── __init__.py
│   ├── position_sizer.py         # Lot calculation
│   ├── stop_loss_manager.py      # SL monitoring
│   └── crash_protector.py        # Circuit breakers
│
├── scheduler/
│   ├── __init__.py
│   ├── market_hours.py           # Trading hours logic
│   └── job_scheduler.py          # APScheduler wrapper
│
├── notifications/
│   ├── __init__.py
│   └── telegram_alerts.py        # Alert system
│
└── nifty_spread_runner.py        # Main entry point (24x7)
```

---

## 📅 Trading Schedule

```python
TRADING_SCHEDULE = {
    "market_scan": "09:20",      # Scan for opportunities
    "entry_window": "09:30-10:30",  # Best entry time
    "monitoring": "10:30-15:00",    # Position monitoring
    "exit_window": "15:00-15:20",   # Exit if needed
    "eod_review": "15:30",          # Daily P&L summary
    
    # Pre-market (before 9:15)
    "indicator_calc": "09:00",      # Calculate RSI, EMA
    "vix_check": "09:10",           # Check VIX levels
    
    # Post-market
    "data_download": "16:00",       # Download EOD data
    "backtest_update": "17:00",     # Update backtest results
}
```

---

## 📋 Implementation Phases

### Phase 1: Foundation (Week 1)
- [ ] Create `indicators/` module with RSI, EMA, VIX fetcher
- [ ] Implement `spread_selector.py` with decision matrix
- [ ] Unit tests for indicator calculations

### Phase 2: Strategy Engine (Week 2)
- [ ] Create `base_strategy.py` abstract class
- [ ] Implement `bull_put_spread.py` with strike selection
- [ ] Implement `bull_call_spread.py` with strike selection
- [ ] Integration with existing `utils.py` helpers

### Phase 3: Risk Management (Week 3)
- [ ] Implement `position_sizer.py`
- [ ] Implement `crash_protector.py` with VIX monitoring
- [ ] Implement `stop_loss_manager.py` with auto-exit

### Phase 4: Scheduler & Alerts (Week 4)
- [ ] Setup APScheduler for 24x7 operation
- [ ] Implement Telegram notifications
- [ ] Create `market_hours.py` for holiday handling

### Phase 5: Integration & Testing (Week 5)
- [ ] Create main runner `nifty_spread_runner.py`
- [ ] Paper trading for 1 week
- [ ] Docker containerization for reliability

---

## 🔧 Technical Requirements

### Python Dependencies (Add to requirements.txt)

```
# Existing
pandas
redis
pyotp
requests
colorlog
pyyaml
tqdm

# New for this strategy
ta                    # Technical Analysis library (RSI, EMA)
apscheduler           # Job scheduling
python-telegram-bot   # Alerts
numpy                 # Calculations
```

### Data Requirements

| Data | Source | Frequency |
|------|--------|-----------|
| NIFTY Spot | Shoonya API | Real-time |
| INDIA VIX | Shoonya API (Token: 26017) | Real-time |
| Option Chain | Shoonya API | On-demand |
| Historical OHLC | Shoonya API / Yahoo | Daily (for RSI/EMA) |

---

## 📊 Key Metrics to Track

```python
METRICS = {
    "win_rate": "Target > 65%",
    "avg_profit": "Per winning trade",
    "avg_loss": "Per losing trade", 
    "profit_factor": "Gross Profit / Gross Loss > 1.5",
    "max_drawdown": "Should be < 10% of capital",
    "sharpe_ratio": "Target > 1.5",
    "trades_per_week": "1-3 trades ideal",
}
```

---

## ⚠️ Important Considerations

1. **Expiry Selection**: Always use current week expiry for maximum theta decay
2. **Liquidity**: Trade only strikes with good OI and tight bid-ask spread
3. **Margin**: BPS requires higher margin; ensure adequate funds
4. **Slippage**: Use limit orders, avoid market orders
5. **Re-entry**: If stopped out, wait for fresh signal before re-entering

---

## 🚀 Quick Start Checklist

- [ ] Verify Shoonya API access and credentials
- [ ] Ensure Redis is running for session management
- [ ] Set up Telegram bot for alerts
- [ ] Create paper trading account for testing
- [ ] Define capital allocation for this strategy
- [ ] Review and customize risk parameters

---

*Plan Created: January 2026*
*Strategy Type: Weekly Options Credit/Debit Spreads*
*Instrument: NIFTY Index Options*
