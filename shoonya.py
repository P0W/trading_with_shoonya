"""
    Sample code place straddle orders for NIFTY, BANKNIFTY and FINNIFTY
"""
import argparse
import datetime
import json
import logging
import os
import pathlib
import sys
import time
import zipfile

import pandas as pd
import pyotp
import redis
import requests
import yaml

from api_helper import Order
from api_helper import ShoonyaApiPy


def configure_logger(log_level, prefix_log_file: str = "shoonya_daily_short"):
    """
    Configure the logger
    """
    # Setup logging
    # create a directory logs if it does not exist
    pathlib.Path.mkdir(pathlib.Path("logs"), exist_ok=True)
    # Create a filename suffixed with current date DDMMYY format with
    # current date inside logs directory
    log_file = pathlib.Path("logs") / (
        f"{prefix_log_file}_{datetime.datetime.now().strftime('%Y%m%d')}.log"
    )
    # pylint: disable=line-too-long
    logging.basicConfig(
        format="%(asctime)s.%(msecs)d %(filename)s:%(lineno)d:%(funcName)s() %(levelname)s %(message)s",
        datefmt="%A,%d/%m/%Y|%H:%M:%S",
        handlers=[
            logging.StreamHandler(sys.stdout),
            logging.FileHandler(log_file),
        ],
        level=log_level,
    )


api = ShoonyaApiPy()

INDICES_TOKEN = {
    "NIFTY": "26000",
    "BANKNIFTY": "26009",
    "FINNIFTY": "26037",
    "INDIAVIX": "26017",
}

INDICES_ROUNDING = {
    "NIFTY": 50,
    "BANKNIFTY": 100,
    "FINNIFTY": 50,
}


def download_scrip_master():
    """
    Download the scrip master from the Shoonya endpoint website
    """
    today = datetime.datetime.now().strftime("%Y%m%d")
    downloads_folder = "./downloads"
    zip_file_name = f"{downloads_folder}/NFO_symbols.txt_{today}.zip"
    todays_nse_fo = f"{downloads_folder}/NFO_symbols.{today}.txt"

    ## unzip and read the file
    ## create a download folder, if not exists
    if not os.path.exists(downloads_folder):
        os.mkdir(downloads_folder)
    if not os.path.exists(todays_nse_fo):
        nse_fo = requests.get("https://api.shoonya.com/NFO_symbols.txt.zip", timeout=15)
        if nse_fo.status_code != 200:
            logging.error("Could not download file")
            return None
        with open(zip_file_name, "wb") as f:
            f.write(nse_fo.content)

        ## extract the file in the download folder
        with zipfile.ZipFile(zip_file_name, "r") as zip_ref:
            zip_ref.extractall(downloads_folder)
        ## remove the zip file
        os.remove(zip_file_name)
        ## rename the file with date suffix
        os.rename(f"{downloads_folder}/NFO_symbols.txt", todays_nse_fo)
    df = pd.read_csv(todays_nse_fo, sep=",")
    return df


def get_staddle_strike(symbol_index):
    """
    Get the nearest strike for the index
    """
    df = download_scrip_master()
    df = df[df["Symbol"] == symbol_index]
    ## the Expiry column is in format 28-DEC-2023
    ## convert to datetime an dfind the closest expiry
    df["Expiry"] = pd.to_datetime(df["Expiry"], format="%d-%b-%Y")
    df["diff"] = df["Expiry"] - datetime.datetime.now()
    df["diff"] = df["diff"].abs()
    df = df.sort_values(by="diff")
    ## get the Expiry date
    expiry_date = df.iloc[0]["Expiry"]
    ## convert to 06DEC23
    expiry_date = expiry_date.strftime("%d%b%y").upper()
    ret = api.get_quotes(exchange="NSE", token=INDICES_TOKEN[symbol_index])
    if ret:
        ltp = float(ret["lp"])
        ## round to nearest INDICES_ROUNDING
        nearest = (
            round(ltp / INDICES_ROUNDING[symbol_index]) * INDICES_ROUNDING[symbol_index]
        )
        ce_strike = f"{symbol_index}{expiry_date}C{nearest}"
        pe_strike = f"{symbol_index}{expiry_date}P{nearest}"
        ## find the token for the strike
        ce_token = df[df["TradingSymbol"] == ce_strike]["Token"].values[0]
        pe_token = df[df["TradingSymbol"] == pe_strike]["Token"].values[0]
        ce_quotes = api.get_quotes(exchange="NFO", token=str(ce_token))
        pe_quotes = api.get_quotes(exchange="NFO", token=str(pe_token))
        return {
            "ce_code": str(ce_token),
            "pe_code": str(pe_token),
            "ce_strike": ce_strike,
            "pe_strike": pe_strike,
            "ce_ltp": float(ce_quotes["lp"]),
            "pe_ltp": float(pe_quotes["lp"]),
        }
    return None


def round_to_point5(x):
    """
    Round to nearest 0.5
    """
    return round(x * 2) / 2


def login(force=False):
    """
    Login to the Shoonya API
    """
    ACCESS_TOKEN_KEY = "access_token"  ## pylint: disable=invalid-name
    try:
        redis_client = redis.Redis()
        access_token = redis_client.get(ACCESS_TOKEN_KEY)
        if access_token and not force:
            access_token = access_token.decode("utf-8")
            with open("cred.yml", encoding="utf-8") as f:
                cred = yaml.load(f, Loader=yaml.FullLoader)
                api.set_session(cred["user"], cred["pwd"], access_token)
            logging.info("Access token found in cache, logging in")
        else:
            raise ValueError("No access token found")
    except Exception as ex:  ## pylint: disable=broad-except
        logging.warning("No access token found in cache, logging in: %s", ex)
        with open("cred.yml", encoding="utf-8") as f:
            cred = yaml.load(f, Loader=yaml.FullLoader)

            ret = api.login(
                userid=cred["user"],
                password=cred["pwd"],
                twoFA=pyotp.TOTP(cred["totp_pin"]).now(),
                vendor_code=cred["vc"],
                api_secret=cred["apikey"],
                imei=cred["imei"],
            )
            susertoken = ret["susertoken"]
            try:
                redis_client.set(
                    ACCESS_TOKEN_KEY, susertoken, ex=2 * 60 * 60
                )  # 2 hours expiry
            except Exception:  ## pylint: disable=broad-except
                pass


def place_straddle(strikes_data, qty=15):
    """
    Place a straddle order for the index
    """
    if not strikes_data:
        return
    logging.info("Strikes: %s", json.dumps(strikes_data, indent=2))
    straddle_orders = [
        {
            "buy_or_sell": "S",
            "product_type": "M",
            "exchange": "NFO",
            "tradingsymbol": strikes_data["ce_strike"],
            "quantity": qty,
            "discloseqty": 0,
            "price_type": "LMT",
            "price": strikes_data["ce_ltp"],
            "trigger_price": None,
            "retention": "DAY",
            "remarks": "ce_straddle",
        },
        {
            "buy_or_sell": "S",
            "product_type": "M",
            "exchange": "NFO",
            "tradingsymbol": strikes_data["pe_strike"],
            "quantity": qty,
            "discloseqty": 0,
            "price_type": "LMT",
            "price": strikes_data["pe_ltp"],
            "trigger_price": None,
            "retention": "DAY",
            "remarks": "pe_straddle",
        },
    ]
    ## all_orders is of type Order
    straddle_orders_obj = [Order(**order) for order in straddle_orders]
    logging.info("Placing straddle: %s", json.dumps(straddle_orders, indent=2))
    response = api.place_basket(straddle_orders_obj)
    logging.info("Response: %s", json.dumps(response, indent=2))


def place_sl_order(tsym, qty, lp, sl_factor, remarks):
    """
    Place a stop loss order
    """
    lp = float(lp)
    sl = round_to_point5(lp * sl_factor)
    trigger = sl - 0.5
    ret = api.place_order(
        buy_or_sell="B",
        product_type="M",
        exchange="NFO",
        tradingsymbol=tsym,
        quantity=qty,
        discloseqty=0,
        price_type="SL-LMT",
        price=sl,
        trigger_price=trigger,
        retention="DAY",
        remarks=f"{remarks}_stop_loss",
    )
    logging.info("Placed stop loss order: %s", json.dumps(ret, indent=2))


def pnl_monitor(pnl, target):
    """
    Monitor pnl
    """
    continue_running = True
    if pnl > target:
        logging.info("PNL > %.2f, exiting", target)
        ret = api.get_order_book()
        for order in ret:
            if order["status"] != "COMPLETE":
                continue
            if order["remarks"] == "pe_straddle" or order["remarks"] == "ce_straddle":
                logging.info("Unsubscribing from %s", order["tsym"])
                api.unsubscribe(f"NFO|{order['tsym']}")
                logging.info("Exiting Leg")
                response = api.place_order(
                    buy_or_sell="B",
                    product_type="M",
                    exchange="NFO",
                    tradingsymbol=order["tsym"],
                    quantity=order["qty"],
                    discloseqty=0,
                    price_type="MKT",
                    price=0,  ## market order
                    trigger_price=None,
                    retention="DAY",
                    remarks=f"{order['remarks']}_exit",
                )
                logging.info(
                    "Response exit position %s", json.dumps(response, indent=2)
                )
        ret = api.get_order_book()
        for order in ret:
            if order["status"] != "TRIGGER_PENDING":
                continue
            if (
                order["remarks"] == "pe_strangle_stop_loss"
                or order["remarks"] == "ce_strangle_stop_loss"
            ):
                norenordno = order["norenordno"]
                logging.info("Cancelling pending stop loss orders")
                response = api.cancel_order(norenordno)
                logging.info("Response cancel order %s", json.dumps(response, indent=2))
        continue_running = False
    return continue_running


## pylint: disable=too-many-instance-attributes
class LiveFeedManager:
    """
    Live feed manager
    """

    ## pylint: disable=too-many-arguments
    def __init__(
        self,
        api_object,
        qty,
        option_strikes,
        sl_factor,
        placed_ord_callback,
    ):
        self.opened = False
        self.pnl = {}
        self.qty = qty
        self.strikes = option_strikes
        self.api = api_object
        self.monitor_function = None
        self.running = False
        self.sl_factor = sl_factor
        self.placed_ord_callback = placed_ord_callback

    def event_handler_feed_update(self, tick_data):
        """
        Event handler for feed update
        """
        if not self.strikes or not self.running:
            unsusbscribe_symbols = [
                f"NFO|{self.strikes['ce_code']}",
                f"NFO|{self.strikes['pe_code']}",
            ]
            self.api.unsubscribe(unsusbscribe_symbols)
            self.api.close_websocket()
            return
        msg = []
        if "lp" in tick_data:
            if tick_data["tk"] == self.strikes["ce_code"]:
                self.pnl[tick_data["tk"]] = self.strikes["ce_ltp"] - float(
                    tick_data["lp"]
                )
                msg.append(f"CE lp: {float(tick_data['lp'])}")
            elif tick_data["tk"] == self.strikes["pe_code"]:
                self.pnl[tick_data["tk"]] = self.strikes["pe_ltp"] - float(
                    tick_data["lp"]
                )
                msg.append(f"PE lp: {float(tick_data['lp'])}")
        if len(self.pnl) == 2:
            total_pnl = self.qty * (
                self.pnl[self.strikes["ce_code"]] + self.pnl[self.strikes["pe_code"]]
            )
            msg.append(f"Total PNL: {total_pnl}")
            logging.info("Feed Data: %s", "| ".join(msg))
            self.running = self.monitor_function(total_pnl)

    def open_callback(self):
        """
        Callback for websocket open
        """
        self.opened = True

    def event_handler_order_update(self, order_data):
        """
        Event handler for order update
        """
        if order_data["status"] == "COMPLETE":
            if order_data["remarks"] == "pe_straddle_stop_loss":
                logging.info("Stop loss hit for PE, unsubscribing")
                self.api.unsubscribe([f"NFO|{self.strikes['pe_code']}"])
            elif order_data["remarks"] == "ce_straddle_stop_loss":
                logging.info("Stop loss hit for CE, unsubscribing")
                self.api.unsubscribe([f"NFO|{self.strikes['ce_code']}"])
            elif (
                order_data["remarks"] == "ce_straddle"
                or order_data["remarks"] == "pe_straddle"
            ):
                logging.info("Straddle Placed %s", order_data["remarks"])
                qty = order_data["qty"]
                lp = order_data["lp"]
                tsym = order_data["tsym"]
                remarks = order_data["remarks"]
                self.placed_ord_callback(tsym, qty, lp, self.sl_factor, remarks)
        logging.info("order update %s", json.dumps(order_data, indent=2))

    def subscribe(self, symbols_list):
        """
        Subscribe to symbols
        """
        self.api.subscribe(symbols_list)

    def start(self, callback):
        """
        Start the websocket
        """
        self.monitor_function = callback
        self.api.start_websocket(
            order_update_callback=self.event_handler_order_update,
            subscribe_callback=self.event_handler_feed_update,
            socket_open_callback=self.open_callback,
            socket_error_callback=lambda e: logging.error("Websocket Error: %s", e),
            socket_close_callback=lambda: logging.info("Websocket Closed"),
        )
        while self.opened is False:
            logging.info("Waiting for websocket to open")
            time.sleep(0.5)
        self.running = True


args = argparse.ArgumentParser()
args.add_argument("--force", action="store_true", default=False)
args.add_argument("--index", required=True, choices=["NIFTY", "BANKNIFTY", "FINNIFTY"])
args.add_argument("--qty", required=True, type=int)
args.add_argument("--sl_factor", default=1.65, type=float)
args.add_argument("--target", default=0.35, type=float)
args.add_argument("--log_level", default="INFO")
args.add_argument("--show-strikes", action="store_true", default=False)
args = args.parse_args()


if __name__ == "__main__":
    configure_logger(args.log_level)
    login(args.force)
    # subscribe to multiple tokens
    index = args.index
    strikes = get_staddle_strike(index)
    logging.info("Strikes: %s", json.dumps(strikes, indent=2))
    target_pnl = (strikes["ce_ltp"] + strikes["pe_ltp"]) * args.qty * (args.target)
    logging.info("Target PNL: %.2f", target_pnl)
    if args.show_strikes:
        sys.exit(0)
    symbols = [f"NFO|{strikes['ce_code']}", f"NFO|{strikes['pe_code']}"]

    live_feed_manager = LiveFeedManager(
        api, args.qty, strikes, args.sl_factor, place_sl_order
    )
    live_feed_manager.start(lambda pnl: pnl_monitor(pnl, target_pnl))
    logging.info("Subscribing to %s", symbols)
    live_feed_manager.subscribe(symbols)
    logging.info("Waiting for 2 seconds")
    time.sleep(2)
    place_straddle(strikes, args.qty)
    while True:
        pass
