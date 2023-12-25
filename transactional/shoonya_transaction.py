"""
This module is used to place orders for straddle strategy using Shoonya API.
Uses relational database to store orders and their status.
"""
import json
import logging

from client_shoonya import ShoonyaApiPy
from const import OrderStatus
from utils import configure_logger
from utils import get_exchange
from utils import get_staddle_strike
from utils import parse_args
from utils import round_to_point5
from utils import validate
from utils import delay_decorator

## pylint: disable=import-error
import transaction_manager_postgres


class ShoonyaTransaction:
    """
    Shoonya Transaction class
    """

    def __init__(self, api_object: ShoonyaApiPy):
        self.api = api_object
        self.transaction_manager = transaction_manager_postgres.TransactionManager(
            self.api,
            config={
                "dbname": "shoonya",
                "user": "admin",
                "password": "admin",
                "host": "localhost",
                "port": "6000",
            },
        )
        self.logger = logging.getLogger(__name__)
        self.order_queue = set()
        for item in ["ce", "pe"]:
            message = f"{item}_straddle"
            self.order_queue.add(message)
            self.order_queue.add(f"{message}_stop_loss")
            self.order_queue.add(f"{message}_subscribe")
            self.order_queue.add(f"{message}_book_profit")
            self.order_queue.add(f"{message}_exit")
            self.order_queue.add(f"{message}_stop_loss_subscribe")
            self.order_queue.add(f"{message}_cancel")
            self.order_queue.add(f"{message}_unsubscribe")

        self.logger.debug("Order queue: %s", self.order_queue)

        self.transaction_manager.start()

    def place_order(
        self,
        order_data,
        parent_remarks=None,
        parent_status: OrderStatus = OrderStatus.COMPLETE,
    ):
        """
        Place order using Shoonya API, using transaction database
        """
        remarks = order_data["remarks"]
        if remarks not in self.order_queue:  ## Already placed
            return
        parent_present = not parent_remarks or (
            parent_remarks not in self.order_queue
            and self.transaction_manager.get_for_remarks(parent_remarks, parent_status)
        )
        result = self.transaction_manager.get_for_remarks(remarks)
        if not result and parent_present:
            response = self.api.place_order(**order_data)
            self.logger.info("Order placed: %s", response)
            ## remove remarks from order queue
            self.order_queue.remove(remarks)

    def subscribe(
        self,
        symbol_data,
        remarks,
        parent_remarks,
        parent_status: OrderStatus = OrderStatus.COMPLETE,
    ):
        """
        Subscribe to a symbol
        """
        ## check if remarks is not present in order_queue,
        ## meaning subscription is already done
        ## check if parent_remarks is present in order_queue,
        ## meaning parent order is not placed yet
        if remarks not in self.order_queue or parent_remarks in self.order_queue:
            return
        result = self.transaction_manager.get_for_remarks(parent_remarks, parent_status)
        if result:
            self.transaction_manager.subscribe_symbols(symbol_data)
            self.order_queue.remove(remarks)

    def unsubscribe(self, symbol_data, remarks, parent_remarks):
        """
        Unsubscribe from a symbol
        """
        if remarks not in self.order_queue:
            return
        norenordno_stop_loss = self.transaction_manager.get_for_remarks(
            f"{parent_remarks}_exit", OrderStatus.COMPLETE
        )
        norenordno_book_profit = self.transaction_manager.get_for_remarks(
            f"{parent_remarks}_book_profit", OrderStatus.COMPLETE
        )
        norenordno_cancelled = self.transaction_manager.get_for_remarks(
            f"{parent_remarks}", OrderStatus.CANCELED
        )
        if norenordno_stop_loss or norenordno_book_profit or norenordno_cancelled:
            self.transaction_manager.unsubscribe_symbols(symbol_data)
            self.order_queue.remove(remarks)

    def over(self):
        """
        Check if the day is over or order queue is empty
        """
        return not self.order_queue or self.transaction_manager.day_over()

    def cancel_on_book_profit(
        self, remarks, parent_remarks, parent_status, cancel_remarks
    ):
        """
        Cancel order using Shoonya API
        """
        ## check if remarks is present in order_queue,
        ## meaning order is not placed yet
        if remarks not in self.order_queue or parent_remarks in self.order_queue:
            return
        norenordno = self.transaction_manager.get_for_remarks(
            parent_remarks, parent_status
        )
        if norenordno:
            norenordno = self.transaction_manager.get_for_remarks(
                cancel_remarks, OrderStatus.TRIGGER_PENDING
            )
            if norenordno:
                ## Cancel the _stop_loss order
                response = self.api.cancel_order(norenordno)
                self.logger.info("Order cancelled: %s", response)
                ## remove remarks from order queue
                self.order_queue.remove(remarks)

    @delay_decorator(15)
    def cancel_on_profit(self, target_profit):
        """
        Cancel order using Shoonya API
        """
        total_pnl = self.transaction_manager.get_pnl()
        if total_pnl > target_profit:
            self.logger.info("Target profit reached, cancelling all pending orders")
            for item in ["ce", "pe"]:
                remarks = f"{item}_straddle"
                norenordno = self.transaction_manager.get_for_remarks(
                    f"{remarks}_stop_loss", OrderStatus.TRIGGER_PENDING
                )
                if norenordno:
                    response = self.api.cancel_order(norenordno)
                    self.logger.info("Order cancelled for stop_loss: %s", response)
                norenordno = self.transaction_manager.get_for_remarks(
                    f"{remarks}_book_profit", OrderStatus.OPEN
                )
                if norenordno:
                    response = self.api.cancel_order(norenordno)
                    self.logger.info("Order cancelled for book_profit: %s", response)

    def test(self, status: str, interval: int = 15):
        """
        Test function
        """
        return self.transaction_manager.test(status, interval)

    @delay_decorator(10)
    def display_order_queue(self):
        """
        Display order queue
        """
        self.logger.info("Order queue: %s", self.order_queue)


## pylint: disable=too-many-locals
def main(args):
    """
    Main function
    """
    index = args.index
    sl_factor = args.sl_factor
    book_profit = args.book_profit
    qty = args.qty
    target = args.target
    cred_file = args.cred_file
    logger = configure_logger(args.log_level, f"shoonya_evt_driven_{index}")
    # disable_module_logger("sqlalchemy.engine.Engine", logging.ERROR)
    logger.debug("Input Arguments: %s", json.dumps(vars(args), indent=2))

    api = ShoonyaApiPy(cred_file)
    ## pnl_display_interval = args.pnl_display_interval

    ## validate the quantity
    validate(qty, index)

    strikes_data = get_staddle_strike(api, index)

    if args.show_strikes:
        logger.info("Strikes data: %s", json.dumps(strikes_data, indent=2))

    shoonya_transaction = ShoonyaTransaction(api)
    test_flag = False
    test_flag_2 = False
    test_flag_3 = False

    while not shoonya_transaction.over():
        for item in ["ce", "pe"]:
            subscribe_msg = f"{item}_straddle"

            symbol = strikes_data[f"{item}_strike"]
            ltp = float(strikes_data[f"{item}_ltp"])
            code = f"{strikes_data[f'{item}_code']}"

            sl_symbol = strikes_data[f"{item}_sl_strike"]
            sl_ltp = float(strikes_data[f"{item}_sl_ltp"])
            sl_ltp = round_to_point5(sl_ltp * sl_factor)
            trigger = sl_ltp - 0.5
            book_profit_ltp = round_to_point5(ltp * book_profit)  ## 20% of premium left
            code_sl = f"{strikes_data[f'{item}_sl_code']}"

            shoonya_transaction.place_order(  ## Place straddle order
                {
                    "buy_or_sell": "S",
                    "product_type": "M",  ## NRML
                    "exchange": get_exchange(symbol),
                    "tradingsymbol": symbol,
                    "quantity": qty,
                    "discloseqty": 0,
                    "price_type": "LMT",
                    "price": ltp,
                    "trigger_price": None,
                    "retention": "DAY",
                    "remarks": subscribe_msg,
                }
            )
            shoonya_transaction.subscribe(  ## Subscribe to straddle symbol, if executed
                symbol_data={
                    "symbolcode": code,
                    "exchange": get_exchange(symbol),
                    "tradingsymbol": symbol,
                },
                remarks=f"{subscribe_msg}_subscribe",
                parent_remarks=subscribe_msg,
                parent_status=OrderStatus.COMPLETE,
            )
            shoonya_transaction.place_order(  ## Place stop loss order
                order_data={
                    "buy_or_sell": "B",
                    "product_type": "M",  ## NRML
                    "exchange": get_exchange(sl_symbol),
                    "tradingsymbol": sl_symbol,
                    "quantity": qty,
                    "discloseqty": 0,
                    "price_type": "SL-LMT",
                    "price": sl_ltp,
                    "trigger_price": trigger,
                    "retention": "DAY",
                    "remarks": f"{subscribe_msg}_stop_loss",
                },
                parent_remarks=subscribe_msg,
            )
            shoonya_transaction.subscribe(  ## Subscribe to stop loss symbol, if executed
                symbol_data={
                    "symbolcode": code_sl,
                    "exchange": get_exchange(sl_symbol),
                    "tradingsymbol": sl_symbol,
                },
                remarks=f"{subscribe_msg}_stop_loss_subscribe",
                parent_remarks=f"{subscribe_msg}_stop_loss",
                parent_status=OrderStatus.COMPLETE,
            )
            shoonya_transaction.place_order(  ## Place book profit order,
                ## if stop loss is placed (TRIGGER_PENDING)
                order_data={
                    "buy_or_sell": "B",
                    "product_type": "M",  ## NRML
                    "exchange": get_exchange(symbol),
                    "tradingsymbol": symbol,
                    "quantity": qty,
                    "discloseqty": 0,
                    "price_type": "LMT",
                    "price": book_profit_ltp,
                    "trigger_price": None,
                    "retention": "DAY",
                    "remarks": f"{subscribe_msg}_book_profit",
                },
                parent_remarks=f"{subscribe_msg}_stop_loss",
                parent_status=OrderStatus.TRIGGER_PENDING,
            )
            shoonya_transaction.cancel_on_book_profit(
                remarks=f"{subscribe_msg}_cancel",
                parent_remarks=f"{subscribe_msg}_book_profit",
                parent_status=OrderStatus.COMPLETE,
                cancel_remarks=f"{subscribe_msg}_stop_loss",
            )
            shoonya_transaction.cancel_on_profit(target_profit=target)
            shoonya_transaction.place_order(  ## Place exit order, if stop loss is CANCELLED
                order_data={
                    "buy_or_sell": "B",
                    "product_type": "M",  ## NRML
                    "exchange": get_exchange(symbol),
                    "tradingsymbol": symbol,
                    "quantity": qty,
                    "discloseqty": 0,
                    "price_type": "MKT",  ## Market order
                    "price": 0,
                    "trigger_price": None,
                    "retention": "DAY",
                    "remarks": f"{subscribe_msg}_exit",
                },
                parent_remarks=f"{subscribe_msg}_stop_loss",
                parent_status=OrderStatus.CANCELED,
            )
            shoonya_transaction.unsubscribe(
                symbol_data={  ## Unsubscribe from straddle symbol,
                    ## if exit order is placed or order is cancelled
                    ## or book profit order is executed
                    "symbolcode": code,
                    "exchange": get_exchange(symbol),
                    "tradingsymbol": symbol,
                },
                remarks=f"{subscribe_msg}_unsubscribe",
                parent_remarks=subscribe_msg,
            )
            if not test_flag:
                test_flag = shoonya_transaction.test(OrderStatus.COMPLETE, 5)
            if not test_flag_2 and test_flag:
                test_flag_2 = shoonya_transaction.test(OrderStatus.TRIGGER_PENDING, 15)
            if not test_flag_3 and test_flag_2:
                test_flag_3 = shoonya_transaction.test(OrderStatus.COMPLETE, 20)
            shoonya_transaction.display_order_queue()


if __name__ == "__main__":
    main(parse_args())
