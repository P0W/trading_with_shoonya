"""
Constants for shoonya trading
"""

import enum


INDICES_TOKEN = {
    "NIFTY": "26000",
    "BANKNIFTY": "26009",
    "FINNIFTY": "26037",
    "INDIAVIX": "26017",
    "MIDCPNIFTY": "26074",
    "SENSEX": "1",
    "USDINR": "1",
    "EURINR": "25",
    "GBPINR": "26",
    "JPYINR": "27",
    "BANKEX": "12",
    "CRUDEOIL": "260604",
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
    "BANKEX": 100,
    "CRUDEOIL": 50,
}

LOT_SIZE = {
    "NIFTY": 50,
    "BANKNIFTY": 15,
    "FINNIFTY": 40,
    "SENSEX": 10,
    "BANKEX": 15,
    "MIDCPNIFTY": 75,
    "USDINR": 1000,
    "EURINR": 1000,
    "GBPINR": 1000,
    "JPYINR": 1000,
    "CRUDEOIL": 100,
}

EXCHANGE = {
    "NIFTY": "NFO",
    "BANKNIFTY": "NFO",
    "FINNIFTY": "NFO",
    "SENSEX": "BFO",
    "BANKEX": "BFO",
    "MIDCPNIFTY": "NFO",
    "INDIAVIX": "NFO",
    "USDINR": "CDS",
    "EURINR": "CDS",
    "GBPINR": "CDS",
    "JPYINR": "CDS",
    "CRUDEOIL": "MCX",
}


SCRIP_SYMBOL_NAME = {
    "NIFTY": "NIFTY",
    "BANKNIFTY": "BANKNIFTY",
    "FINNIFTY": "FINNIFTY",
    "SENSEX": "BSXOPT",
    "BANKEX": "BKXOPT",
    "MIDCPNIFTY": "MIDCPNIFTY",
    "INDIAVIX": "INDIAVIX",
    "USDINR": "USDINR",
    "EURINR": "EURINR",
    "GBPINR": "GBPINR",
    "JPYINR": "JPYINR",
    "CRUDEOIL": "CRUDEOIL",
}


## Enum for order status
class OrderStatus(enum.Enum):
    """
    Enum for order status
    """

    OPEN = "OPEN"
    COMPLETE = "COMPLETE"
    CANCELED = "CANCELED"
    REJECTED = "REJECTED"
    TRIGGER_PENDING = "TRIGGER_PENDING"
    PENDING = "PENDING"
    INVALID_STATUS_TYPE = "INVALID_STATUS_TYPE"

    ## tostring method
    def __str__(self):
        return self.value
