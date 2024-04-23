"""
Order manager
"""

import datetime
import logging
import time
from typing import Any
from typing import Dict

from utils import wait_with_progress
from utils import full_stack


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
        start_time = time.time()
        timeout = 60  # Timeout after 60 seconds

        while True:
            if not self.opened:
                self.api.start_websocket(
                    order_update_callback=self._event_handler_order_update,
                    subscribe_callback=self._event_handler_feed_update,
                    socket_open_callback=self._open_callback,
                    socket_error_callback=lambda e: self.logger.error(
                        "Websocket Error: %s\n%s", e, full_stack()
                    ),
                    socket_close_callback=lambda: self.logger.info("Websocket Closed"),
                )

            open_start_time = time.time()
            while self.opened is False:
                elapsed_time = time.time() - open_start_time
                if elapsed_time > 30:  # If WebSocket is not open after 30 seconds
                    self.logger.warning(
                        "WebSocket not open after 30 seconds. Retrying..."
                    )
                    break
                self.logger.warning("Waiting for websocket to open")
                time.sleep(0.5)

            if self.opened:
                self.running = True
                break

            elapsed_time = time.time() - start_time
            if elapsed_time > timeout:
                self.logger.error("Failed to start WebSocket after 1 minute. Exiting.")
                break

            self.logger.info("Retrying in 30 seconds...")
            wait_with_progress(30)  # Use wait_with_progress instead of time.sleep
            self.api.close_websocket()


if __name__ == "__main__":
    import client_shoonya
    import utils

    utils.configure_logger("DEBUG", "test_order_manager")
    api = client_shoonya.ShoonyaApiPy(cred_file="../cred.yml", force_login=False)
    om = OrderManager(api, {})
    om.start()
    om.subscribe("MCX|426261")
    time.sleep(20)
