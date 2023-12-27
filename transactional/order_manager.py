"""
Order manager
"""
import datetime
import logging
import time
from typing import Any
from typing import Dict


class OrderManager:
    """
    Order manager class
    """

    def __init__(self, api_object, config):
        """
        Initialize the order manager
        """
        self.logger = logging.getLogger(__name__)
        self.api = api_object
        self.opened = False
        self.subscribed_symbols = set()
        self.running = False
        self.config = config

    def _event_handler_feed_update(self, tick_data: Dict):
        """
        Event handler for feed update
        """
        self.logger.debug(tick_data)

    def _open_callback(self):
        """
        Callback for websocket open
        """
        if self.opened:
            self.logger.info("Websocket Re-Opened")
            ## check if self.subscribed_symbols is non empty, if yes resubscribe
            if self.subscribed_symbols:
                self.logger.info("Resubscribing to %s", self.subscribed_symbols)
                ## convert to list
                self.api.subscribe(list(self.subscribed_symbols))
        else:
            self.logger.info("Websocket Opened")
        self.opened = True

    def _event_handler_order_update(self, order_data):
        """
        Event handler for order update
        """
        self.logger.debug(order_data)

    def subscribe(self, symbols_list: Any):
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

    def unsubscribe(self, symbols_list: Any):
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
        ## Add 5 hours 30 minutes to UTC time for IST
        now = datetime.datetime.utcnow() + datetime.timedelta(hours=5, minutes=30)
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
