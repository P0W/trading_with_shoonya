"""
This module is used to place orders for straddle strategy using Shoonya API.
Uses relational database to store orders and their status.
"""
import json
import logging
import sys
import time
from typing import Dict

from client_shoonya import ShoonyaApiPy
from const import OrderStatus
from utils import configure_logger
from utils import delay_decorator
from utils import get_exchange
from utils import get_instance_id
from utils import get_staddle_strike
from utils import parse_args
from utils import round_to_point5
from utils import validate
from utils import get_remarks

import transaction_manager_postgres  ## pylint: disable=import-error


class ShoonyaTransaction:
    """
    Shoonya Transaction class
    """

    def __init__(self, api_object: ShoonyaApiPy, instance_id: str):
        """
        Initialize the Shoonya Transaction
        """
        self.api = api_object
        self.instance_id = instance_id
        self.transaction_manager = transaction_manager_postgres.TransactionManager(
            self.api,
            config={
                "dbname": "shoonya",
                "user": "admin",
                "password": "admin",
                "host": "localhost",
                "port": "6000",
                "instance_id": self.instance_id,
            },
        )
        self.logger = logging.getLogger(__name__)
        self.order_queue = set()
        for item in ["ce", "pe"]:
            message = get_remarks(instance_id=self.instance_id, msg=f"{item}_straddle")
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
        order_data: Dict,
        parent_remarks: str = None,
        parent_status: OrderStatus = OrderStatus.COMPLETE,
        exit_order: str = None,
    ):
        """
        Place order using Shoonya API, using transaction database
        """
        remarks = order_data["remarks"]
        if remarks not in self.order_queue:  ## Already placed
            return
        parent_present = not parent_remarks or (
            parent_remarks not in self.order_queue
            and self.transaction_manager.get_for_remarks(parent_remarks, parent_status)[
                0
            ]
        )
        result, _ = self.transaction_manager.get_for_remarks(remarks)
        if not result and parent_present or exit_order in self.order_queue:
            response = self.api.place_order(**order_data)
            self.logger.info("Order placed: %s", response)
            ## remove remarks from order queue
            self.order_queue.remove(remarks)
            if exit_order in self.order_queue:
                self.order_queue.remove(exit_order)

    def subscribe(
        self,
        symbol_data: Dict,
        remarks: str,
        parent_remarks: str,
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
        result, _ = self.transaction_manager.get_for_remarks(
            parent_remarks, parent_status
        )
        if result:
            self.transaction_manager.subscribe_symbols(symbol_data)
            self.order_queue.remove(remarks)

    def unsubscribe(self, symbol_data: Dict, remarks: str, parent_remarks: str):
        """
        Unsubscribe from a symbol
        """
        if remarks not in self.order_queue:
            return
        norenordno_stop_loss, _ = self.transaction_manager.get_for_remarks(
            f"{parent_remarks}_exit", OrderStatus.COMPLETE
        )
        norenordno_book_profit, _ = self.transaction_manager.get_for_remarks(
            f"{parent_remarks}_book_profit", OrderStatus.COMPLETE
        )
        norenordno_cancelled, _ = self.transaction_manager.get_for_remarks(
            f"{parent_remarks}", OrderStatus.CANCELED
        )
        if norenordno_stop_loss or norenordno_book_profit or norenordno_cancelled:
            self.logger.debug(
                "stop_loss %s or book_profit %s or cancelled %s",
                norenordno_stop_loss,
                norenordno_book_profit,
                norenordno_cancelled,
            )
            self.transaction_manager.unsubscribe_symbols(symbol_data)
            self.order_queue.remove(remarks)

    def over(self):
        """
        Check if the day is over or order queue is empty
        """
        return not self.order_queue or self.transaction_manager.day_over()

    def cancel_on_book_profit(
        self,
        remarks: str,
        parent_remarks: str,
        parent_status: OrderStatus,
        cancel_remarks: str,
    ):
        """
        Cancel order using Shoonya API
        """
        ## check if remarks is present in order_queue,
        ## meaning order is not placed yet
        if remarks not in self.order_queue or parent_remarks in self.order_queue:
            return
        norenordno, _ = self.transaction_manager.get_for_remarks(
            parent_remarks, parent_status
        )
        if norenordno:
            norenordno, status = self.transaction_manager.get_for_remarks(
                cancel_remarks
            )
            if norenordno:
                if status == OrderStatus.TRIGGER_PENDING:
                    ## Cancel the _stop_loss order
                    response = self.api.cancel_order(norenordno)
                    self.logger.info("Order cancelled: %s", response)
                    ## remove remarks from order queue
                    self.order_queue.remove(remarks)
                elif status == OrderStatus.COMPLETE:
                    ## if cancel_remarks is completed, simply remove remarks from order queue
                    self.order_queue.remove(remarks)

    @delay_decorator(delay=10)
    def cancel_on_profit(self, target_profit: float):
        """Cancel order using Shoonya API"""
        total_pnl = self.transaction_manager.get_pnl()
        if total_pnl > target_profit:
            self.logger.info("Target profit reached, cancelling all pending orders")
            self._square_off()

    def test(self, status: str, interval: int = 15):
        """Test function"""
        return self.transaction_manager.test(status, interval)

    @delay_decorator(delay=30)
    def display_order_queue(self):
        """Display order queue"""
        self.logger.debug("Order queue: %s", self.order_queue)

    def _square_off(self):
        """Square off all positions"""
        order_book = self.transaction_manager.get_orders()
        for order in order_book:
            order_status = order["status"]
            remarks = order["remarks"]
            if order_status in [
                OrderStatus.OPEN,
                OrderStatus.TRIGGER_PENDING,
                OrderStatus.PENDING,
            ]:
                self.api.cancel_order(order["norenordno"])
                self.logger.info("Order cancelled: %s", remarks)
            elif order_status == OrderStatus.COMPLETE:
                self.logger.info("Placing square off orders: %s", remarks)
                tradingsymbol = order["tradingsymbol"]
                qty = order["qty"]
                exchange = get_exchange(tradingsymbol)
                opposite_buysell = "B" if order["buysell"] == "S" else "S"
                ## Place exit order at Market price
                response = self.api.place_order(
                    buy_or_sell=opposite_buysell,
                    product_type="M",
                    exchange=exchange,
                    tradingsymbol=tradingsymbol,
                    quantity=qty,
                    discloseqty=0,
                    price_type="MKT",
                    price=0,
                    trigger_price=None,
                    retention="DAY",
                    remarks=f"{remarks}_square_off",
                )
                self.logger.debug("Square off Order placed: %s", response)
            else:
                self.logger.debug("Ignoring Order status: %s", order["status"])
        ## Empty the order queue
        self.order_queue.clear()
        ## Wait for 5 seconds
        time.sleep(5)


## pylint: disable=too-many-locals, too-many-statements
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
    target_mtm = args.target_mtm
    instance_id = args.instance_id
    logger = configure_logger(args.log_level, f"shoonya_evt_driven_{index}")

    logger.debug("Input Arguments: %s", json.dumps(vars(args), indent=2))
    if not instance_id:
        instance_id = f"shoonya_{get_instance_id()}"
    else:
        logger.warning("Instance id provided, this is running for previous instance")

    logger.info("Running Instance: %s", instance_id)

    api = ShoonyaApiPy(cred_file)

    ## validate the quantity
    validate(qty, index)

    strikes_data = get_staddle_strike(api, index)

    premium = qty * (float(strikes_data["ce_ltp"]) + float(strikes_data["pe_ltp"]))
    premium_lost = (
        qty
        * sl_factor
        * (float(strikes_data["ce_sl_ltp"]) + float(strikes_data["pe_sl_ltp"]))
    )
    max_loss = strikes_data["max_strike_diff"] * qty + (premium - premium_lost)
    if target_mtm == -1:
        logging.info("Target MTM not provided, calculating from premium")
        target_mtm = premium * target
    else:
        logging.info(
            "Target MTM provided, ignoring target %.2f %% of premium", target * 100.0
        )

    logging.info(
        "Strikes data: %s | Max profit :%.2f | Max Loss : %.2f | Target : %.2f",
        json.dumps(strikes_data, indent=2),
        premium,
        max_loss,
        target_mtm,
    )

    if args.show_strikes:
        sys.exit(0)

    shoonya_transaction = ShoonyaTransaction(api_object=api, instance_id=instance_id)

    while not shoonya_transaction.over():
        for item in ["ce", "pe"]:
            subscribe_msg = get_remarks(instance_id=instance_id, msg=f"{item}_straddle")

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
            shoonya_transaction.cancel_on_book_profit(  ## Cancel stop loss order,
                ## if book profit is COMPLETE
                remarks=f"{subscribe_msg}_cancel",
                parent_remarks=f"{subscribe_msg}_book_profit",
                parent_status=OrderStatus.COMPLETE,
                cancel_remarks=f"{subscribe_msg}_stop_loss",
            )
            shoonya_transaction.cancel_on_profit(target_profit=target_mtm)
            shoonya_transaction.place_order(  ## Place exit order, if stop loss is CANCELED
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
                exit_order=f"{subscribe_msg}_target_hit",
            )
            shoonya_transaction.unsubscribe(  ## Unsubscribe from straddle symbol,
                ## if exit order is placed or order is cancelled
                ## or book profit order is executed
                symbol_data={
                    "symbolcode": code,
                    "exchange": get_exchange(symbol),
                    "tradingsymbol": symbol,
                },
                remarks=f"{subscribe_msg}_unsubscribe",
                parent_remarks=subscribe_msg,
            )
            shoonya_transaction.display_order_queue()


def quick_test():
    """
    Quick test function
    """
    logger = configure_logger(logging.DEBUG, "quick_test")

    ## Setup
    api = ShoonyaApiPy("../cred.yml")
    instance_id = "shoonya_55992_1703609778"
    msg = "ce_straddle_stop_loss"
    shoonya_transaction = ShoonyaTransaction(api_object=api, instance_id=instance_id)
    remark = get_remarks(instance_id=instance_id, msg=msg)

    ## Act
    n, s = shoonya_transaction.transaction_manager.get_for_remarks(
        remark, OrderStatus.TRIGGER_PENDING
    )

    ## Verify
    logger.info("norenordno: %s | status: %s", n, s == OrderStatus.TRIGGER_PENDING)


if __name__ == "__main__":
    main(parse_args())
    # quick_test()
    sys.exit(0)
