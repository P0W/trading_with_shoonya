# Trading with shoonya

## Description

This project is a trading bot built with Python. It uses the `shoonya_transaction.py` script to place a short straddle, with OTM stop losses and then monitors the live MTM

It works (and tested) with NIFTY, BANKNIFTY, FINNIFTY, SENSEX, BANKEX, MIDCPNIFTY, CRUDEOIL options.

---

```
usage: shoonya_transaction.py [-h] [--force] --index {NIFTY,BANKNIFTY,FINNIFTY,MIDCPNIFTY,SENSEX,BANKEX,CRUDEOIL}
                              --qty QTY [--sl_factor SL_FACTOR] [--target TARGET] [--log-level {INFO,DEBUG}]
                              [--show-strikes] [--pnl-display-interval PNL_DISPLAY_INTERVAL]
                              [--target-mtm TARGET_MTM] [--book-profit BOOK_PROFIT]
                              [--cred-file CRED_FILE] [--instance-id INSTANCE_ID]
                              [--same-premium]

Straddle orders for NIFTY, BANKNIFTY, FINNIFTY, MIDCPNIFTY and USDINR

options:
  -h, --help            show this help message and exit
  --force               Force login
  --index {NIFTY,BANKNIFTY,FINNIFTY,MIDCPNIFTY,SENSEX,BANKEX,CRUDEOIL}
  --qty QTY             Quantity to trade
  --sl_factor SL_FACTOR
                        Stop loss factor | default 75 percent on individual leg
  --target TARGET       Target profit | default 35 percent of collected premium
  --log-level {INFO,DEBUG}
                        Log level
  --show-strikes        Show strikes only and exit
  --pnl-display-interval PNL_DISPLAY_INTERVAL
                        PnL display interval in seconds | default 15 seconds
  --target-mtm TARGET_MTM
                        Target MTM profit | default no target MTM profit
  --book-profit BOOK_PROFIT
                        Book profit percent of premium left | default 60 percent of premium left
  --cred-file CRED_FILE
                        Credential file | default cred.yml in the current directory
  --instance-id INSTANCE_ID
                        Instance id for multiple instance of the scripts
  --same-premium        Look for same premium for both legs
```

## Installation

To install the project, follow these steps:

1. Clone the repository
2. Navigate to the project directory and cd into transactional
3. Install the required Python packages, typically in a virtual environment
4. Note: Requires docker runtime to kick off redis and postgresql containers
5. Create a `cred.yml` with following entries grabbed from shoonya API integration page
   https://prism.shoonya.com/api
   
   ```
    user    : 'your-userid'
    pwd     : 'your-password'
    vc      : 'your-userid-suffixed-with _U'
    apikey  : 'your-api-key'
    imei    : 'random-text'
    totp_pin: 'your-totp-seceret-pin-obtained-during-setup'
   ```
```bash
    python -m venv .
    scripts\activate
    cd transactional
    pip install -r requirements.txt
    docker-compose up -d
```
---

Sample runs

```bash
   python .\shoonya_transaction.py --show-strikes --qty 500 --index NIFTY
   python .\shoonya_transaction.py --qty 75 --index BANKNIFTY --target 0.25
   python .\shoonya_transaction.py --cred-file ..\cred.yml --index FINNIFTY --qty 40 --target-mtm 221.00 --show-strikes
```

**NOTE**: The `shoonya.py` is in decrecation mode, however one can look into the event based algo trade bot idea.
