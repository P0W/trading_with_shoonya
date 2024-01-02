# Trading with shoonya

## Description

This project is a trading bot built with Python. It uses the `shoonya_transaction.py` script to place a short straddle, with OTM stop losses and then monitors the live MTM

It works (and tested) with NIFTY, BANKNIFTY, FINNIFTY, SENSEX, BANKEX, MIDCPNIFTY, CRUDEOIL options.

```
Straddle orders for NIFTY, BANKNIFTY, FINNIFTY, MIDCPNIFTY and USDINR

options:
  -h, --help            show this help message and exit
  --force               Force login
  --index {NIFTY,BANKNIFTY,FINNIFTY,MIDCPNIFTY,SENSEX,BANKEX}
  --qty QTY             Quantity to trade
  --sl_factor SL_FACTOR
                        Stop loss factor | default 30 percent on individual leg
  --target TARGET       Target profit | default 35 percent of collected premium
  --log-level {INFO,DEBUG}
                        Log level
  --show-strikes        Show strikes only and exit
  --pnl-display-interval PNL_DISPLAY_INTERVAL
                        PnL display interval in seconds
  --target-mtm TARGET_MTM
                        Target MTM profit
  --book-profit BOOK_PROFIT
                        Book profit percent of premium left
```

## Installation

To install the project, follow these steps:

1. Clone the repository
2. Navigate to the project directory and cd into transactional
3. Install the required Python packages, typically in a virtual environment
4. Note: Requires docker runtime to kick off redis and postgresql containers

```bash
    python -m venv .
    scripts\activate
    cd transactional
    pip install -r requirements.txt
    docker-compose up -d
```
4. Sample runs

```bash
   python .\shoonya_transaction.py --show-strikes --qty 500 --index NIFTY
   python .\shoonya_transaction.py --qty 75 --index BANKNIFTY --target 0.25 
```

**NOTE**: The `shoonya.py` is in decrecation mode, however one can look into the event based algo trade bot idea.
