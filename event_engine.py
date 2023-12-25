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
from utils import round_to_point5


## pylint: disable=too-many-instance-attributes
class EventEngine:
    """
    Live feed event manager
    """

    ## pylint: disable=too-many-arguments
    def __init__(self, api_object, target, pnl_display_interval):
        self.opened = False
        self.running = False
        self.api = api_object
        self.target = target
        self.pnl_display_interval = pnl_display_interval

        self.subscribed_symbols = set()
        self.tick_data = {}
        self.on_complete_methods = {}
        self.user_methods = []
        self.pnl_monitor = None
        self.symbols_init_data = {}
        self.existing_orders = []
        self.in_position = False
        self.logger = logging.getLogger("event_engine")
        self._last_displayed_time = None

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
                self.tick_data[tk] = {"pnl": self._get_pnl(tk, lp), "lp": lp}
                self.running = self._monitor_function()
        except Exception as ex:  ## pylint: disable=broad-except
            self.logger.error("Exception in feed update: %s", ex)
            ## stacktrace
            self.logger.error(full_stack())
            sys.exit(-1)

    def _get_displayed_pnl(self):
        """
        Get displayed pnl
        """
        pnl_data = {}
        total_pnl = 0
        for symbol, data in self.tick_data.items():
            tradingsymbol = self.symbols_init_data[symbol]["tradingsymbol"]
            buy_or_sell = self.symbols_init_data[symbol]["buy_or_sell"]
            qty = self.symbols_init_data[symbol]["qty"]
            sign = "+" if buy_or_sell == "B" else "-"
            key = f"{sign}{qty} x {tradingsymbol}"
            pnl_data[key] = {"mtm": f"{data['pnl']:.2f}", "ltp": f"{data['lp']:.2f}"}
            total_pnl += data["pnl"]
        pnl_data["Total"] = f"{total_pnl:.2f}"
        pnl_data["Target"] = f"{self.target:.2f}"
        return pnl_data

    def _cancel_all_orders(self):
        for norenordno in self.existing_orders:
            res = self.api.single_order_history(norenordno)
            for order in res:
                if "exchordid" in order:
                    if order["status"] in ["TRIGGER_PENDING", "OPEN"]:
                        self.logger.info("Cancelling pending order %s", norenordno)
                        remarks = order["remarks"]
                        response = self.api.cancel_order(norenordno)
                        self.logger.info(
                            "Response cancel order %s",
                            json.dumps(response, indent=2),
                        )
                        if remarks in self.on_complete_methods:
                            self.logger.debug(
                                "Removing %s from on_complete_methods", remarks
                            )
                            del self.on_complete_methods[remarks]

    ## pylint: disable=too-many-locals
    def _monitor_function(self):
        """
        Monitor pnl
        """

        def exit_place_order(args):
            """
            Place order
            """
            self.logger.debug("Placing order %s", json.dumps(args, indent=2))
            response = self.api.place_order(**args)
            self.add_existing_orders(response["norenordno"])
            return response

        continue_running = True
        pnl_data = self._get_displayed_pnl()
        pnl = float(pnl_data["Total"])
        if pnl > self.target:
            self.logger.info("Target Achieved | PNL > %.2f | exiting", self.target)
            order_book_response = self.api.get_order_book()
            for order in order_book_response:
                if order["status"] != "COMPLETE":
                    continue
                norenordno = order["norenordno"]
                ## Need to make sure we close only the orders we
                ## opened with current running instance
                ## This will enable us to run multiple instances of the same script
                if norenordno in self.existing_orders and ("remarks" in order):
                    symbol = order["tsym"]
                    exchange_code = get_exchange(symbol)
                    qty = order["fillshares"]
                    buy_or_sell = order["trantype"]
                    remarks = order["remarks"]
                    opposite_buy_or_sell = "B" if buy_or_sell == "S" else "S"
                    ## get code from self.symbols_init_data
                    symbol_code = None
                    for code, data in self.symbols_init_data.items():
                        if data["norenordno"] == norenordno:
                            symbol_code = code
                            break
                    if not symbol_code:
                        self.logger.warning(
                            "Symbol code not found for self.symbols_init_data: %s for %s",
                            json.dumps(self.symbols_init_data, indent=2),
                            norenordno,
                        )
                        continue
                    self.logger.info("Unsubscribing from %s", symbol_code)
                    self.unsubscribe(f"{exchange_code}|{symbol_code}")
                    self.logger.info("Exiting Leg %s | %s", remarks, symbol)

                    ## square_off_price slight above ltp for buy and below for sell
                    self.logger.debug("tick_data: %s", self.tick_data)
                    if symbol_code not in self.tick_data:
                        self.logger.warning(
                            "Symbol %s not found in tick_data %s, placing market order",
                            symbol_code,
                            json.dumps(self.tick_data, indent=2),
                        )
                        square_off_price = 0.0
                    else:
                        ltp = self.tick_data[symbol_code]["lp"]
                        square_off_price = ltp
                        if ltp > 0.5:
                            square_off_price = round_to_point5(ltp)
                            square_off_price = (
                                ltp + 0.5 if buy_or_sell == "B" else ltp - 0.5
                            )
                    self.register(
                        exit_place_order,
                        {
                            "buy_or_sell": opposite_buy_or_sell,
                            "product_type": "M",  ## NRML
                            "exchange": exchange_code,
                            "tradingsymbol": symbol,
                            "quantity": qty,
                            "discloseqty": 0,
                            "price_type": "MKT",  ## CHANGE TO LMT
                            "price": 0.0,  ## FIXME: square_off_price,
                            "trigger_price": None,
                            "retention": "DAY",
                            "remarks": f"{remarks}_exit",
                        },
                        f"{remarks}_exit",
                        self._exit_complete,
                        None,
                    )
                    self._cancel_all_orders()
            continue_running = False
        else:
            now = datetime.datetime.now()
            if (
                self._last_displayed_time is None
                or (not continue_running)
                or (now - self._last_displayed_time).seconds
                >= self.pnl_display_interval
            ):
                self.logger.info("PNL: %s", json.dumps(pnl_data, indent=1))
                self._last_displayed_time = now
        return continue_running

    def _exit_complete(self, order_remark=None):
        """
        Cancel pending orders
        """
        logging.debug("Cancelling pending orders")
        order_book_response = self.api.get_order_book()
        for order in order_book_response:
            if (order["status"] == "TRIGGER_PENDING") or (order["status"] == "OPEN"):
                norenordno = order["norenordno"]
                if norenordno in self.existing_orders and ("remarks" in order):
                    if not order_remark or order["remarks"] == order_remark:
                        if order_remark:
                            self.logger.info(
                                "Cancelling pending order %s", order_remark
                            )
                        else:
                            self.logger.info("Cancelling pending stop loss orders")
                        response = self.api.cancel_order(norenordno)
                        self.logger.info(
                            "Response cancel order %s",
                            json.dumps(response, indent=2),
                        )
                else:
                    self.logger.debug(
                        "Not cancelling order: %s Current existing_orders: %s | order_remark: %s",
                        json.dumps(order, indent=2),
                        self.existing_orders,
                        order_remark,
                    )
            else:
                self.logger.debug(
                    "Not matched. Not cancelling order: %s", json.dumps(order, indent=2)
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
                ## convert to list
                self.api.subscribe(list(self.subscribed_symbols))
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
            ) or (
                order_data["status"] == "CANCELLED"
                and order_data["reporttype"] == "Canceled"
            ):
                message = order_data["remarks"]
                norenordno = order_data["norenordno"]
                logging.debug(
                    "Is %s Present in on_complete_methods: %s",
                    message,
                    message in self.on_complete_methods,
                )
                logging.debug(
                    "Is %s Present in existing_orders: %s",
                    message,
                    norenordno in self.existing_orders,
                )
                if (
                    message in self.on_complete_methods
                    and norenordno in self.existing_orders
                ):
                    self.logger.debug("Found %s in on_complete_methods", message)
                    if order_data["status"] == "CANCELLED":
                        self.logger.info("Order %s Cancelled", message)
                        del self.on_complete_methods[message]
                        return
                    (
                        on_complete_method,
                        on_complete_method_args,
                    ) = self.on_complete_methods[message]
                    self.logger.debug(
                        "Current on_complete_methods_args: %s | %s",
                        on_complete_method,
                        on_complete_method_args,
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
                    self.logger.debug(
                        "Still to complete: %s", self.on_complete_methods.keys()
                    )
                else:
                    self.logger.debug(
                        "Ignored Matching Order update %s",
                        json.dumps(order_data, indent=2),
                    )
            else:
                self.logger.debug(
                    "Ignored Order update %s", json.dumps(order_data, indent=2)
                )
        except Exception as ex:  ## pylint: disable=broad-except
            self.logger.error("Exception in order update: %s", ex)
            ## stacktrace
            self.logger.error(full_stack())
            sys.exit(-1)

    def _all_unsubscribed(self):
        """
        All unsubscribed
        """
        return len(self.subscribed_symbols) == 0

    def _all_registration_completed(self):
        """
        All registered methods completed
        """
        ## check for self.on_complete_methods and self.user_methods empty
        return not self.on_complete_methods and len(self.user_methods) == 0

    def subscribe(self, symbols_list):
        """
        Subscribe to symbols
        """
        if isinstance(symbols_list, str):
            symbols_list = [symbols_list]
        self.logger.info("Subscribing to %s", symbols_list)
        self.api.subscribe(symbols_list)
        ## add to the list of subscribed symbols
        self.subscribed_symbols.update(symbols_list)
        self.logger.debug(
            "Current subscribed_symbols: %s",
            self.subscribed_symbols,
        )

    def unsubscribe(self, symbols_list):
        """
        Unsubscribe from symbols
        """
        if isinstance(symbols_list, str):
            symbols_list = [symbols_list]
        for symbol in symbols_list:
            ## remove from the list of subscribed symbols
            if symbol in self.subscribed_symbols:
                self.logger.info("Unsubscribed from %s", symbol)
                self.subscribed_symbols.remove(symbol)
        self.api.unsubscribe(symbols_list)
        self.logger.debug(
            "Current subscribed_symbols: %s",
            self.subscribed_symbols,
        )

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
        Is running when either of the following is true
        1. self.running is True
        2. self.subscribed_symbols is non empty
        3. self.on_complete_methods is non empty
        """
        return (
            self.running
            or (not self._all_unsubscribed())
            or (not self._all_registration_completed())
        )

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
            "Registering event subscribe_msg=%s for callback=%s with args=%s",
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
        self.logger.debug(
            "Current symbols_init_data: %s",
            json.dumps(self.symbols_init_data, indent=2),
        )

    def add_existing_orders(self, norenordno):
        """
        Add existing orders
        """
        self.existing_orders.append(norenordno)
        self.logger.debug(
            "Current existing_orders: %s",
            json.dumps(self.existing_orders, indent=2),
        )

    def cancel_all_orders(self, remark):
        """
        Cancel all orders with given remark
        """
        self._exit_complete(remark)
