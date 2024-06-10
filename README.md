# Transactional Algo trading bot with Shoonya API

## Description

This project is a trading bot built with Python. It uses the `shoonya_transaction.py` script to place a short straddle, with OTM stop losses and then monitors the live MTM

It works (and tested) with NIFTY, BANKNIFTY, FINNIFTY, SENSEX, BANKEX, MIDCPNIFTY, CRUDEOIL options.

## The Strategy

1. Place ATM Short Straddle when premimum are almost same (difference: ~15 points)
2. On the breakeven strike (CE = ATM + collected premium, PE = ATM - collected premium), place a stop loss order at 1.75 % (configurable) of the strike price
3. Place a book profit stop loss order at 60 % (configurable) on the individual ATM legs and trail the stop loss for every 5% drop in price set stop loss to 60% of last set price.
4. Monitor the target = 35% of collected premimum (configurable)
5. Cancel pending orders if either target mtm is achieved or both legs profit is booked or time is 15:31 IST or inital ATM straddle is rejected

## The Secret Sauce

Covert a short straddle to iron butter fly to tackle intraday spikes, which tend to hit stop loss frequently. 
Give the strategy time to juggle between volatility and eventually comes to rest on expiry days.

![IronFly](https://github.com/P0W/trading_with_shoonya/assets/5833233/67246ff5-8997-4c16-80a9-0beeb9700b61)


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

## Deploying the trading bot

1. Setup the cron job for daily expiry trading. Refer `expiry_runner.sh`
2. Kick start the `bot_server.py`
   
```bash
  python bot_server.py
```

Browse to `localhost:5000`
1. Signin with shoonya credential to grab the JWT token
2. Call the appropiate methods

![Shoonya_Trading_Bot](https://github.com/P0W/trading_with_shoonya/assets/5833233/9941afdb-aa88-4f68-87d9-a7a2dc3fbefd)

  

**NOTE**: The `rust` contents are not yet complete. I am trying to implement this on rust sooner.
