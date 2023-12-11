"""
Constants for shoonya trading
"""

INDICES_TOKEN = {
    "NIFTY": "26000",
    "BANKNIFTY": "26009",
    "FINNIFTY": "26037",
    "INDIAVIX": "26017",
    "MIDCPNIFTY": "26074",
    "SENSEX": "26001",
    "USDINR": "1",
    "EURINR": "25",
    "GBPINR": "26",
    "JPYINR": "27",
}

INDICES_ROUNDING = {
    "NIFTY": 50,
    "BANKNIFTY": 100,
    "FINNIFTY": 50,
    "MIDCPNIFTY": 25,
    "SENSEX": 100,
    "USDINR": 0.25,
    "EURINR": 0.25,
    "GBPINR": 0.25,
    "JPYINR": 0.25,
}

LOT_SIZE = {
    "NIFTY": 50,
    "BANKNIFTY": 15,
    "FINNIFTY": 40,
    "SENSEX": 10,
    "MIDCPNIFTY": 75,
    "USDINR": 1000,
    "EURINR": 1000,
    "GBPINR": 1000,
    "JPYINR": 1000,
}

EXCHANGE = {
    "NIFTY": "NFO",
    "BANKNIFTY": "NFO",
    "FINNIFTY": "NFO",
    "SENSEX": "BFO",
    "MIDCPNIFTY": "NFO",
    "INDIAVIX": "NFO",
    "USDINR": "CDS",
    "EURINR": "CDS",
    "GBPINR": "CDS",
    "JPYINR": "CDS",
}
