"""
    Straddle orders for 
    NIFTY, BANKNIFTY, FINNIFTY, 
    MIDCPNIFTY and USDINR
    Monitoring the PNL and exiting at target or stop loss
"""
## Author: Prashant Srivastava
## Date: Dec 11th, 2023

import argparse
import datetime
import json
import logging
import sys
import time

import pandas as pd
import pyotp
import redis
import yaml
from NorenRestApiPy.NorenApi import NorenApi

from utils import configure_logger
from utils import download_scrip_master
from utils import full_stack
from utils import round_to_point5
from utils import validate
from utils import get_exchange

from const import INDICES_ROUNDING
from const import INDICES_TOKEN
from const import EXCHANGE


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


def get_staddle_strike(shoonya_api, symbol_index):
    """
    Get the nearest strike for the index
    """
    df = download_scrip_master(file_id=f"{EXCHANGE[symbol_index]}_symbols")
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
    ret = shoonya_api.get_quotes(
        exchange=get_exchange(symbol_index, is_index=True),
        token=str(INDICES_TOKEN[symbol_index]),
    )
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
        ce_quotes = shoonya_api.get_quotes(
            exchange=EXCHANGE[symbol_index], token=str(ce_token)
        )
        pe_quotes = shoonya_api.get_quotes(
            exchange=EXCHANGE[symbol_index], token=str(pe_token)
        )
        return {
            "ce_code": str(ce_token),
            "pe_code": str(pe_token),
            "ce_strike": ce_strike,
            "pe_strike": pe_strike,
            "ce_ltp": float(ce_quotes["lp"]),
            "pe_ltp": float(pe_quotes["lp"]),
        }
    return None


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
        symbol = strikes_data[f"{item}_code"]
        ltp = strikes_data[f"{item}_ltp"]
        response = shoonya_api.place_order(
            buy_or_sell="S",
            product_type="M",
            exchange=get_exchange(symbol),
            tradingsymbol=symbol,
            quantity=qty,
            discloseqty=0,
            price_type="LMT",
            price=ltp,
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
        exchange=get_exchange(tsym),
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
                    symbol = order["tsym"]
                    exchange_code = get_exchange(symbol)
                    logging.info("Unsubscribing from %s", order["tsym"])
                    shoonya_api.unsubscribe(f"{exchange_code}|{order['tsym']}")
                    logging.info("Exiting Leg")
                    response = shoonya_api.place_order(
                        buy_or_sell="B",
                        product_type="M",
                        exchange=exchange_code,
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
        self.premium_collected = 0.0
        self.premium_left = 0.0
        self.exchange = get_exchange(self.strikes["ce_code"])

    def event_handler_feed_update(self, tick_data):
        """
        Event handler for feed update
        """
        try:
            if not self.strikes or not self.running:
                unsusbscribe_symbols = [
                    f"{self.exchange}|{self.strikes['ce_code']}",
                    f"{self.exchange}|{self.strikes['pe_code']}",
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
                self.api.unsubscribe([f"{self.exchange}|{self.strikes['pe_code']}"])
                self.premium_left += order_data["flqty"] * order_data["flprc"]
            elif order_data["remarks"] == "ce_straddle_stop_loss":
                logging.info("Stop loss hit for CE, unsubscribing")
                self.api.unsubscribe([f"{self.exchange}|{self.strikes['ce_code']}"])
                self.premium_left += order_data["flqty"] * order_data["flprc"]
            elif (
                order_data["remarks"] == "ce_straddle"
                or order_data["remarks"] == "pe_straddle"
            ):
                logging.info("Straddle Placed %s", order_data["remarks"])
                qty = order_data["flqty"]
                lp = order_data["flprc"]
                tsym = order_data["tsym"]
                remarks = order_data["remarks"]
                slipage = lp - order_data["prc"]
                logging.info("Slipage %.2f | %s", slipage, remarks)
                self.premium_collected += qty * lp
                orderno = self.placed_ord_callback(tsym, qty, lp, remarks)
                if orderno:
                    self.existing_orders.append(orderno)
                self.in_position = True
            elif (
                order_data["remarks"] == "ce_straddle_exit"
                or order_data["remarks"] == "pe_straddle_exit"
            ):
                self.premium_left += order_data["flqty"] * order_data["flprc"]
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
        profit = self.premium_collected - self.premium_left
        profit_percent = (profit / self.premium_collected) * 100
        logging.info(
            "Premium Collected %.2f | Premium Left %.2f | Profit %.2f | Profit %% %.2f",
            self.premium_collected,
            self.premium_left,
            profit,
            profit_percent,
        )
        self.api.close_websocket()


args = argparse.ArgumentParser(
    description="Straddle orders for NIFTY, BANKNIFTY, FINNIFTY, MIDCPNIFTY and USDINR"
)
args.add_argument("--force", action="store_true", default=False, help="Force login")
args.add_argument(
    "--index",
    required=True,
    choices=[
        "NIFTY",
        "BANKNIFTY",
        "FINNIFTY",
        "MIDCPNIFTY",
        "USDINR",
        "EURINR",
        "GBPINR",
        "JPYINR",
    ],
)
args.add_argument("--qty", required=True, type=int, help="Quantity to trade")
args.add_argument(
    "--sl_factor", default=1.65, type=float, help="Stop loss factor | default 65%% on individual leg"
)
args.add_argument(
    "--target", default=0.35, type=float, help="Target profit | default 35%% of collected premium"
)
args.add_argument(
    "--log_level", default="INFO", help="Log level", choices=["INFO", "DEBUG"]
)
args.add_argument(
    "--show-strikes",
    action="store_true",
    default=False,
    help="Show strikes only and exit",
)
args = args.parse_args()


if __name__ == "__main__":
    configure_logger(args.log_level)
    # subscribe to multiple tokens
    index = args.index
    quantity = args.qty
    exchange = EXCHANGE[index]
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
    symbols = [f"{exchange}|{strikes['ce_code']}", f"{exchange}|{strikes['pe_code']}"]

    live_feed_manager = LiveFeedManager(
        api,
        strikes,
        lambda tsyb, qty, lp, remark: place_sl_order(
            api, tsyb, qty, lp, remark, args.sl_factor
        ),
    )
    live_feed_manager.start(
        lambda pnl, all_orders: pnl_monitor(
            api, quantity * pnl, all_orders, quantity * target_pnl
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
