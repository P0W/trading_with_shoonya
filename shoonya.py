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
import traceback
import zipfile

import pandas as pd
import pyotp
import redis
import requests
import yaml


from NorenRestApiPy.NorenApi import NorenApi


class ShoonyaApiPy(NorenApi):
    """
    Shoonya API Initializer
    """

    def __init__(self):
        NorenApi.__init__(
            self,
            host="https://api.shoonya.com/NorenWClientTP/",
            websocket="wss://api.shoonya.com/NorenWSTP/",
        )


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

LOT_SIZE = {
    "NIFTY": 50,
    "BANKNIFTY": 15,
    "FINNIFTY": 40,
    "SENSEX": 10,
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


def get_staddle_strike(shoonya_api, symbol_index):
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
    ret = shoonya_api.get_quotes(exchange="NSE", token=INDICES_TOKEN[symbol_index])
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
        ce_quotes = shoonya_api.get_quotes(exchange="NFO", token=str(ce_token))
        pe_quotes = shoonya_api.get_quotes(exchange="NFO", token=str(pe_token))
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


def login(shoonya_api, force=False):
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
                shoonya_api.set_session(cred["user"], cred["pwd"], access_token)
            logging.info("Access token found in cache, logging in")
        else:
            raise ValueError("No access token found")
    except Exception as ex:  ## pylint: disable=broad-except
        logging.warning("No access token found in cache, logging in: %s", ex)
        with open("cred.yml", encoding="utf-8") as f:
            cred = yaml.load(f, Loader=yaml.FullLoader)

            ret = shoonya_api.login(
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


def place_straddle(shoonya_api, strikes_data, qty):
    """
    Place a straddle order for the index
    """
    if not strikes_data:
        return None
    ## all_orders is of type Order
    placed_orders = []
    for item in ["ce", "pe"]:
        logging.info("Placing order for %s", item)
        response = shoonya_api.place_order(
            buy_or_sell="S",
            product_type="M",
            exchange="NFO",
            tradingsymbol=strikes_data[f"{item}_strike"],
            quantity=qty,
            discloseqty=0,
            price_type="LMT",
            price=strikes_data[f"{item}_ltp"],
            trigger_price=None,
            retention="DAY",
            remarks=f"{item}_straddle",
        )
        if response["stat"] == "Ok":
            logging.info("Order placed for %s", item)
            logging.info("Response: %s", json.dumps(response, indent=2))
            placed_orders.append(response["norenordno"])
        else:
            logging.error("Could not place order for %s", item)
            logging.error("Response: %s", json.dumps(response, indent=2))
            sys.exit(-1)
    return placed_orders

## pylint: disable=too-many-arguments
def place_sl_order(shoonya_api, tsym, qty, lp, remarks, sl_factor):
    """
    Place a stop loss order
    """
    lp = float(lp)
    sl = round_to_point5(lp * sl_factor)
    trigger = sl - 0.5
    response = shoonya_api.place_order(
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
    logging.info("Placed stop loss order: %s", json.dumps(response, indent=2))
    if response["stat"] == "Ok":
        return response["norenordno"]
    return None


def pnl_monitor(shoonya_api, pnl, existing_orders, target):
    """
    Monitor pnl
    """
    continue_running = True
    if pnl > target:
        logging.info("Target Achieved | PNL > %.2f | exiting", target)
        logging.info("Existing orders %s", json.dumps(existing_orders, indent=2))
        ret = shoonya_api.get_order_book()
        for order in ret:
            if order["status"] != "COMPLETE":
                continue
            if ("remarks" in order) and (
                order["remarks"] == "pe_straddle" or order["remarks"] == "ce_straddle"
            ):
                norenordno = order["norenordno"]
                if norenordno in existing_orders:
                    logging.info("Unsubscribing from %s", order["tsym"])
                    shoonya_api.unsubscribe(f"NFO|{order['tsym']}")
                    logging.info("Exiting Leg")
                    response = shoonya_api.place_order(
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
        ret = shoonya_api.get_order_book()
        for order in ret:
            if order["status"] == "TRIGGER_PENDING" and ("remarks" in order):
                if (
                    order["remarks"] == "pe_straddle_stop_loss"
                    or order["remarks"] == "ce_straddle_stop_loss"
                ):
                    norenordno = order["norenordno"]
                    if norenordno in existing_orders:
                        logging.info("Cancelling pending stop loss orders")
                        response = shoonya_api.cancel_order(norenordno)
                        logging.info(
                            "Response cancel order %s", json.dumps(response, indent=2)
                        )
        continue_running = False
    else:
        logging.info("PNL %.2f | Target %.2f", pnl, target)
    return continue_running


def full_stack():
    """
    Get the full stack trace
    """
    exc = sys.exc_info()[0]
    stack = traceback.extract_stack()[:-1]  # last one would be full_stack()
    if exc is not None:  # i.e. an exception is present
        del stack[-1]  # remove call of full_stack, the printed exception
        # will contain the caught exception caller instead
    trc = "Traceback (most recent call last):\n"
    stackstr = trc + "".join(traceback.format_list(stack))
    if exc is not None:
        stackstr += "  " + traceback.format_exc()
    return stackstr


def validate(index_qty, index_value):
    """
    Validate the quantity
    """
    if index_value not in INDICES_TOKEN:
        logging.error("Invalid index %s", index_value)
        sys.exit(-1)
    if index_qty % LOT_SIZE[index_value] != 0:
        logging.error("Quantity must be multiple of %s", LOT_SIZE[index_value])
        sys.exit(-1)


## pylint: disable=too-many-instance-attributes
class LiveFeedManager:
    """
    Live feed manager
    """

    ## pylint: disable=too-many-arguments
    def __init__(
        self,
        api_object,
        option_strikes,
        placed_ord_callback,
    ):
        self.opened = False
        self.pnl = {}
        self.strikes = option_strikes
        self.api = api_object
        self.monitor_function = None
        self.running = False
        self.placed_ord_callback = placed_ord_callback
        self.in_position = False
        self.existing_orders = []

    def event_handler_feed_update(self, tick_data):
        """
        Event handler for feed update
        """
        try:
            if not self.strikes or not self.running:
                unsusbscribe_symbols = [
                    f"NFO|{self.strikes['ce_code']}",
                    f"NFO|{self.strikes['pe_code']}",
                ]
                self.api.unsubscribe(unsusbscribe_symbols)
                self.in_position = False
                return
            msg = []
            if "lp" in tick_data and self.in_position:
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
                total_pnl = (
                    self.pnl[self.strikes["ce_code"]]
                    + self.pnl[self.strikes["pe_code"]]
                )
                logging.info("Feed Data: %s", "| ".join(msg))
                self.running = self.monitor_function(total_pnl, self.existing_orders)
        except Exception as ex:  ## pylint: disable=broad-except
            logging.error("Exception in feed update: %s", ex)
            ## stacktrace
            logging.error(full_stack())
            sys.exit(-1)

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
                lp = order_data["prc"]
                tsym = order_data["tsym"]
                remarks = order_data["remarks"]
                orderno = self.placed_ord_callback(tsym, qty, lp, remarks)
                if orderno:
                    self.existing_orders.append(orderno)
                self.in_position = True
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

    def update_orders(self, all_orders):
        """
        Update the orders
        """
        for order in all_orders:
            self.existing_orders.append(order)

    def is_running(self):
        """
        Is running
        """
        return self.running

    def stop(self):
        """
        Stop the websocket
        """
        self.api.close_websocket()


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
    # subscribe to multiple tokens
    index = args.index
    quantity = args.qty
    ## validate the quantity
    validate(quantity, index)
    api = ShoonyaApiPy()
    login(api, args.force)
    strikes = get_staddle_strike(api, index)
    premium_collected = strikes["ce_ltp"] + strikes["pe_ltp"]
    target_pnl = premium_collected * (args.target)
    stop_loss = premium_collected * (args.sl_factor - 1)
    logging.info(
        "STARTING ALGO TRADING WITH SHOONYA on %s | Straddle Strikes %s"
        "| Total Premium %.2f | Target %.2f | Stop Loss %.2f",
        index,
        json.dumps(strikes, indent=2),
        premium_collected * quantity,
        target_pnl * quantity,
        stop_loss * quantity,
    )
    if args.show_strikes:
        sys.exit(0)
    symbols = [f"NFO|{strikes['ce_code']}", f"NFO|{strikes['pe_code']}"]

    live_feed_manager = LiveFeedManager(
        api,
        strikes,
        lambda tsyb, qty, lp, remark: place_sl_order(
            api, tsyb, qty, lp, remark, args.sl_factor
        ),
    )
    live_feed_manager.start(
        lambda pnl, stop_loss_orders: pnl_monitor(
            api, quantity * pnl, stop_loss_orders, quantity * target_pnl
        )
    )
    logging.info("Subscribing to %s", symbols)
    live_feed_manager.subscribe(symbols)
    logging.info("Waiting for 2 seconds")
    time.sleep(2)
    orders = place_straddle(api, strikes, args.qty)
    live_feed_manager.update_orders(orders)
    while live_feed_manager.is_running():
        pass
    live_feed_manager.stop()
    logging.info("Exiting")
    time.sleep(2)
    logging.info("Good Bye!")
