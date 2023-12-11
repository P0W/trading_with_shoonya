# Trading with shoonya

## Description

This project is a trading bot built with Python. It uses the `shoonya.py` script to place a short straddle and then monitors the live MTM

## Installation

To install the project, follow these steps:

1. Clone the repository
2. Navigate to the project directory
3. Install the required Python packages: 

```bash
    python -m venv .
    pip install -r requirements.txt
```
4. Sample runs

```bash
   python .\shoonya.py --show-strikes --qty 500 --index NIFTY
   python .\shoonya.py --qty 75 --index BANKNIFTY --target 0.25 
```

