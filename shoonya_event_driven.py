"""
    Event driven pub-sub trading strategy for iron fly short straddle
"""
import argparse
import datetime
import json
import logging
import sys
import time

from client_shoonya import ShoonyaApiPy
from shoonya import get_staddle_strike
from utils import configure_logger
from utils import full_stack
from utils import get_exchange
from utils import round_to_point5


## pylint: disable=too-many-instance-attributes
class LiveFeedManager:
    """
    Live feed manager
    """

    ## pylint: disable=too-many-arguments
    def __init__(self, api_object, target):
        self.opened = False
        self.running = False
        self.api = api_object
        self.target = target

        self.subscribed_symbols = set()
        self.tick_data = {}
        self.on_complete_methods = {}
        self.user_methods = []
        self.pnl_monitor = None
        self.symbols_init_data = {}
        self.existing_orders = []

    def add_symbol_init_data(
        self, symbol_code, qty, avg_price, buy_or_sell, norenordno
    ):
        """
        Add symbol init data
        """
        self.symbols_init_data[symbol_code] = {
            "qty": qty,
            "avg_price": avg_price,
            "buy_or_sell": buy_or_sell,
        }
        self.existing_orders.append(norenordno)

    def _get_pnl(self, symbol, ltp):
        """
        Get pnl
        """
        if symbol in self.symbols_init_data:
            qty = self.symbols_init_data[symbol]["qty"]
            avg_price = self.symbols_init_data[symbol]["avg_price"]
            buy_or_sell = self.symbols_init_data[symbol]["buy_or_sell"]
            if buy_or_sell == "B":
                return (ltp - avg_price) * qty
            return (avg_price - ltp) * qty
        return 0

    def event_handler_feed_update(self, tick_data):
        """
        Event handler for feed update
        """
        try:
            if "lp" in tick_data:
                lp = float(tick_data["lp"])
                tk = tick_data["tk"]
                self.tick_data[tk] = self._get_pnl(tk, lp)
                total_pnl = sum(self.tick_data.values())
                logging.info("PnL: %s", total_pnl)
                self.running = self._monitor_function(total_pnl)
        except Exception as ex:  ## pylint: disable=broad-except
            logging.error("Exception in feed update: %s", ex)
            ## stacktrace
            logging.error(full_stack())
            sys.exit(-1)

    def _monitor_function(self, pnl):
        """
        Monitor pnl
        """
        continue_running = True
        if pnl > self.target:
            logging.info("Target Achieved | PNL > %.2f | exiting", self.target)
            ret = self.api.get_order_book()
            for order in ret:
                if order["status"] != "COMPLETE":
                    continue
                if ("remarks" in order) and (
                    order["remarks"] == "pe_straddle"
                    or order["remarks"] == "ce_straddle"
                ):
                    norenordno = order["norenordno"]
                    if norenordno in self.existing_orders:
                        symbol = order["tsym"]
                        exchange_code = get_exchange(symbol)
                        logging.info("Unsubscribing from %s", order["tsym"])
                        self.unsubscribe(f"{exchange_code}|{order['tsym']}")
                        logging.info("Exiting Leg")
                        self.register(
                            self.api.place_order,
                            {
                                "buy_or_sell": "B",
                                "product_type": "M",
                                "exchange": exchange_code,
                                "tradingsymbol": order["tsym"],
                                "quantity": order["qty"],
                                "discloseqty": 0,
                                "price_type": "MKT",
                                "price": 0,  ## market order
                                "trigger_price": None,
                                "retention": "DAY",
                                "remarks": f"{order['remarks']}_exit",
                            },
                            f"{order['remarks']}_exit",
                            self._exit_complete,
                            {},
                        )
            continue_running = False
        else:
            logging.info("PNL %.2f | Target %.2f", pnl, self.target)
        return continue_running

    def _exit_complete(self):
        ret = self.api.get_order_book()
        for order in ret:
            if order["status"] == "TRIGGER_PENDING" and ("remarks" in order):
                if (
                    order["remarks"] == "pe_straddle_stop_loss"
                    or order["remarks"] == "ce_straddle_stop_loss"
                ):
                    norenordno = order["norenordno"]
                    if norenordno in self.existing_orders:
                        logging.info("Cancelling pending stop loss orders")
                        response = self.api.cancel_order(norenordno)
                        logging.info(
                            "Response cancel order %s",
                            json.dumps(response, indent=2),
                        )

    def open_callback(self):
        """
        Callback for websocket open
        """
        self.opened = True

    def event_handler_order_update(self, order_data):
        """
        Event handler for order update
        """
        if order_data["status"] == "COMPLETE" and order_data["reporttype"] == "Fill":
            message = order_data["remarks"]
            if message in self.on_complete_methods:
                on_complete_method, on_complete_method_args = self.on_complete_methods[
                    message
                ]
                on_complete_method_args["order_data"] = order_data
                response = on_complete_method(**on_complete_method_args)
                logging.info("Response method %s | %s", message, response)
                ## remove this from on_complete_methods
                del self.on_complete_methods[message]

        logging.info("order update %s", json.dumps(order_data, indent=2))

    def subscribe(self, symbols_list):
        """
        Subscribe to symbols
        """
        logging.info("Subscribing to %s", symbols_list)
        self.api.subscribe(symbols_list)
        ## add to the list of subscribed symbols
        self.subscribed_symbols.update(symbols_list)

    def unsubscribe(self, symbols_list):
        """
        Unsubscribe from symbols
        """
        for symbol in symbols_list:
            self.api.unsubscribe(symbol)
            ## remove from the list of subscribed symbols
            if symbol in self.subscribed_symbols:
                logging.info("Unsubscribed from %s", symbol)
                self.subscribed_symbols.remove(symbol)

    def is_empty(self):
        """
        Is empty
        """
        return len(self.subscribed_symbols) == 0

    def day_over(self):
        """
        Day over
        """
        ## check for 15:31, beware of timezone
        now = datetime.datetime.now()
        if now.hour == 15 and now.minute >= 31:
            return True
        return False

    def start(self):
        """
        Start the websocket
        """
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

    ## create a method register that accepts a two callbacks and their argument lists
    def register(
        self,
        user_method,
        user_method_args,
        subscribe_msg,
        on_complete_method,
        on_complete_method_args,
    ):
        """
        Register a method
        """
        self.user_methods.append((subscribe_msg, user_method, user_method_args))
        self.on_complete_methods[subscribe_msg] = (
            on_complete_method,
            on_complete_method_args,
        )

    def evt_register(self, subscribe_msg, user_method, user_method_args):
        """
        Register a method
        """
        self.on_complete_methods[subscribe_msg] = (user_method, user_method_args)

    def run(self):
        """
        Run the registered methods
        """
        for subscribe_msg, user_method, user_method_args in self.user_methods:
            response = user_method(**user_method_args)
            logging.info("Response method %s | %s", subscribe_msg, response)
            time.sleep(1)
        ## empty the list
        self.user_methods = []


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
    "--sl_factor",
    default=1.65,
    type=float,
    help="Stop loss factor | default 65%% on individual leg",
)
args.add_argument(
    "--target",
    default=0.35,
    type=float,
    help="Target profit | default 35%% of collected premium",
)
args.add_argument(
    "--log-level", default="DEBUG", help="Log level", choices=["INFO", "DEBUG"]
)
args.add_argument(
    "--show-strikes",
    action="store_true",
    default=False,
    help="Show strikes only and exit",
)
args = args.parse_args()

logging = configure_logger("DEBUG")


## pylint: disable=too-many-locals
def main():
    """
    Main
    """
    api = ShoonyaApiPy()

    qty = args.qty
    sl_factor = args.sl_factor
    target = args.target
    index = args.index

    live_feed_manager = LiveFeedManager(api, target)

    def on_complete(response):
        """
        On complete
        """
        api.place_order(**response)
        fillshares = int(response["fillshares"])
        flprc = float(response["flprc"])
        # symbol = response["tsym"]
        buy_or_sell = response["trantype"]
        code = response["code"]
        norenordno = response["norenordno"]
        live_feed_manager.add_symbol_init_data(
            symbol_code=code,
            qty=fillshares,
            avg_price=flprc,
            buy_or_sell=buy_or_sell,
            norenordno=norenordno,
        )

    def stop_loss_executed(response):
        """
        On stop_loss executed complete
        """
        live_feed_manager.subscribe([response["tsym"]])
        fillshares = int(response["fillshares"])
        flprc = float(response["flprc"])
        # symbol = response["tsym"]
        instrument = response["instrument"]
        buy_or_sell = response["trantype"]
        code = response["code"]
        norenordno = response["norenordno"]
        live_feed_manager.subscribe([instrument])
        live_feed_manager.add_symbol_init_data(
            symbol_code=code,
            qty=fillshares,
            avg_price=flprc,
            buy_or_sell=buy_or_sell,
            norenordno=norenordno,
        )

    strikes_data = get_staddle_strike(api, index)

    for item in ["ce", "pe"]:
        subscribe_msg = f"{item}_straddle"

        symbol = strikes_data[f"{item}_strike"]
        ltp = float(strikes_data[f"{item}_ltp"])
        code = f"{strikes_data[f'{item}_code']}"

        sl_symbol = strikes_data[f"{item}_sl_strike"]
        sl_ltp = float(strikes_data[f"{item}_sl_ltp"])
        sl_ltp = round_to_point5(sl_ltp * sl_factor)
        trigger = sl_ltp - 0.5
        code_sl = f"{strikes_data[f'{item}_sl_code']}"
        live_feed_manager.register(
            api.place_order,
            {
                "buy_or_sell": "S",
                "product_type": "M",
                "exchange": get_exchange(symbol),
                "tradingsymbol": symbol,
                "quantity": qty,
                "discloseqty": 0,
                "price_type": "LMT",
                "price": ltp,
                "trigger_price": None,
                "retention": "DAY",
                "remarks": subscribe_msg,
            },
            subscribe_msg,
            on_complete,
            {
                "buy_or_sell": "B",
                "product_type": "M",
                "exchange": get_exchange(sl_symbol),
                "tradingsymbol": sl_symbol,
                "quantity": qty,
                "discloseqty": 0,
                "price_type": "SL-LMT",
                "price": sl_ltp,
                "trigger_price": trigger,
                "retention": "DAY",
                "remarks": f"{subscribe_msg}_stop_loss",
                "code": code,
            },
        )

        live_feed_manager.evt_register(
            f"{subscribe_msg}_stop_loss",
            stop_loss_executed,
            {"instrument": f"{get_exchange(sl_symbol)}|{code_sl}", "code": code_sl},
        )
