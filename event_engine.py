"""
Event driven trading bot for broker Shoonya
"""
import datetime
import json
import logging
import sys
import time

from utils import full_stack
from utils import get_exchange


## pylint: disable=too-many-instance-attributes
class EventEngine:
    """
    Live feed event manager
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
        self.in_position = False
        self.logger = logging.getLogger("event_engine")

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
        self.logger.error(
            "Symbol %s not found in symbols_init_data %s",
            symbol,
            json.dumps(self.symbols_init_data, indent=2),
        )
        return -999.99

    def _event_handler_feed_update(self, tick_data):
        """
        Event handler for feed update
        """
        try:
            if not self.running:
                self.in_position = False
                return
            if "lp" in tick_data and self.in_position:
                lp = float(tick_data["lp"])
                tk = tick_data["tk"]
                self.tick_data[tk] = self._get_pnl(tk, lp)
                msg = {}
                for symbol, pnl in self.tick_data.items():
                    tradingsymbol = self.symbols_init_data[symbol]["tradingsymbol"]
                    msg[tradingsymbol] = f"{pnl:.2f}"
                total_pnl = sum(self.tick_data.values())
                msg["total"] = f"{total_pnl:.2f}"
                self.logger.info(
                    "PNL: %s | Target %.2f",
                    json.dumps(msg, indent=2),
                    self.target,
                )
                self.running = self._monitor_function(total_pnl)
        except Exception as ex:  ## pylint: disable=broad-except
            self.logger.error("Exception in feed update: %s", ex)
            ## stacktrace
            self.logger.error(full_stack())
            sys.exit(-1)

    def _monitor_function(self, pnl):
        """
        Monitor pnl
        """
        continue_running = True
        if pnl > self.target:
            self.logger.info("Target Achieved | PNL > %.2f | exiting", self.target)
            ret = self.api.get_order_book()
            for order in ret:
                if order["status"] != "COMPLETE":
                    continue
                norenordno = order["norenordno"]
                if norenordno in self.existing_orders and ("remarks" in order):
                    symbol = order["tsym"]
                    exchange_code = get_exchange(symbol)
                    qty = order["fillshares"]
                    buy_or_sell = order["trantype"]
                    ## get code from self.symbols_init_data
                    for code, data in self.symbols_init_data.items():
                        if data["norenordno"] == norenordno:
                            symbol_code = code
                            break
                    opposite_buy_or_sell = "B" if buy_or_sell == "S" else "S"
                    self.logger.info("Unsubscribing from %s", symbol_code)
                    self.unsubscribe(f"{exchange_code}|{symbol_code}")
                    self.logger.info("Exiting Leg %s", order["remarks"])
                    self.logger.info("Placing exit order for %s", order["tsym"])
                    self.register(
                        lambda args: self.api.place_order(**args),
                        {
                            "buy_or_sell": opposite_buy_or_sell,
                            "product_type": "M",
                            "exchange": exchange_code,
                            "tradingsymbol": order["tsym"],
                            "quantity": qty,
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
        return continue_running

    def _exit_complete(self, _args):
        """
        Cancel pending orders
        """
        ret = self.api.get_order_book()
        for order in ret:
            if order["status"] == "TRIGGER_PENDING":
                norenordno = order["norenordno"]
                if norenordno in self.existing_orders and ("remarks" in order):
                    self.logger.info("Cancelling pending stop loss orders")
                    response = self.api.cancel_order(norenordno)
                    self.logger.info(
                        "Response cancel order %s",
                        json.dumps(response, indent=2),
                    )

    def _open_callback(self):
        """
        Callback for websocket open
        """
        if self.opened:
            self.logger.info("Websocket Re-Opened")
            ## check if self.subscribed_symbols is non empty, if yes resubscribe
            if self.subscribed_symbols:
                self.logger.info(
                    "Resubscribing to %s", json.dumps(self.subscribed_symbols, indent=2)
                )
                self.api.subscribe(self.subscribed_symbols)
        else:
            self.logger.info("Websocket Opened")
        self.opened = True

    def _event_handler_order_update(self, order_data):
        """
        Event handler for order update
        """
        try:
            if (
                order_data["status"] == "COMPLETE"
                and order_data["reporttype"] == "Fill"
            ):
                message = order_data["remarks"]
                if message in self.on_complete_methods:
                    self.logger.debug("Found %s in on_complete_methods", message)
                    (
                        on_complete_method,
                        on_complete_method_args,
                    ) = self.on_complete_methods[message]
                    self.logger.debug(
                        "Current on_complete_methods_args: %s",
                        json.dumps(on_complete_method_args, indent=2),
                    )
                    ## put the order data in the new args as keys
                    order_data["user_data"] = on_complete_method_args
                    self.logger.debug(
                        "Calling %s with %s",
                        on_complete_method,
                        json.dumps(order_data, indent=2),
                    )
                    response = on_complete_method(order_data)
                    if response:
                        self.logger.info("Response method %s | %s", message, response)
                    ## remove this from on_complete_methods
                    self.logger.debug("Removing %s from on_complete_methods", message)
                    del self.on_complete_methods[message]
            else:
                self.logger.debug(
                    "Ignored Order update %s", json.dumps(order_data, indent=2)
                )
        except Exception as ex:  ## pylint: disable=broad-except
            self.logger.error("Exception in order update: %s", ex)
            ## stacktrace
            self.logger.error(full_stack())
            sys.exit(-1)

    def subscribe(self, symbols_list):
        """
        Subscribe to symbols
        """
        self.logger.info("Subscribing to %s", symbols_list)
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
                self.logger.info("Unsubscribed from %s", symbol)
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
            order_update_callback=self._event_handler_order_update,
            subscribe_callback=self._event_handler_feed_update,
            socket_open_callback=self._open_callback,
            socket_error_callback=lambda e: self.logger.error("Websocket Error: %s", e),
            socket_close_callback=lambda: self.logger.info("Websocket Closed"),
        )
        while self.opened is False:
            self.logger.info("Waiting for websocket to open")
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
        ## pylint: disable=line-too-long
        self.logger.debug(
            "Registering subscribe_msg=%s for callback=%s with args=%s on_complete_method=%s with args=%s",
            subscribe_msg,
            user_method,
            json.dumps(user_method_args, indent=2),
            on_complete_method,
            json.dumps(on_complete_method_args, indent=2),
        )
        self.user_methods.append((subscribe_msg, user_method, user_method_args))
        self.on_complete_methods[subscribe_msg] = (
            on_complete_method,
            on_complete_method_args,
        )

    def evt_register(self, subscribe_msg, user_method, user_method_args):
        """
        Register a method
        """
        self.logger.debug(
            "Registering subscribe_msg=%s for callback=%s with args=%s",
            subscribe_msg,
            user_method,
            json.dumps(user_method_args, indent=2),
        )
        self.on_complete_methods[subscribe_msg] = (user_method, user_method_args)

    def run(self):
        """
        Run the registered methods
        """
        for subscribe_msg, user_method, user_method_args in self.user_methods:
            self.logger.info("Running %s", subscribe_msg)
            response = user_method(user_method_args)
            if response:
                self.logger.info(
                    "Response method %s | %s",
                    subscribe_msg,
                    json.dumps(response, indent=2),
                )
            time.sleep(1)
        ## empty the list
        self.user_methods = []

    def add_symbol_init_data(
        self, symbol_code, qty, avg_price, buy_or_sell, norenordno, tradingsymbol
    ):
        """
        Add symbol init data
        """
        ## pylint: disable=line-too-long
        self.logger.debug(
            "Adding symbol=%s, qty=%s, avg_price=%s, buy_or_sell=%s, norenordno=%s, tradingsymbol=%s",
            symbol_code,
            qty,
            avg_price,
            buy_or_sell,
            norenordno,
            tradingsymbol,
        )
        self.symbols_init_data[symbol_code] = {
            "qty": qty,
            "avg_price": avg_price,
            "buy_or_sell": buy_or_sell,
            "norenordno": norenordno,
            "tradingsymbol": tradingsymbol,
        }
        self.in_position = True

    def add_existing_orders(self, norenordno):
        """
        Add existing orders
        """
        self.existing_orders.append(norenordno)
