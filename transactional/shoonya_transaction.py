"""
This module is used to place orders for straddle strategy using Shoonya API.
Uses relational database to store orders and their status.
"""

import json
import logging
import sys
import time
from typing import Dict

import transaction_manager_postgres  ## pylint: disable=import-error

from client_shoonya import ShoonyaApiPy
from const import OrderStatus
from data_store import DataStore  ## pylint: disable=import-error
from utils import configure_logger
from utils import delay_decorator
from utils import get_exchange
from utils import get_instance_id
from utils import get_remarks
from utils import get_staddle_strike
from utils import parse_args
from utils import round_to_point5
from utils import validate
from utils import wait_with_progress


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
        self.product_type = "M"  ## For MIS, "M" ## for NRML, hardcode for now
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
        norenordno_book_profit, _ = self.transaction_manager.get_for_remarks(
            f"{parent_remarks}_book_profit", OrderStatus.COMPLETE
        )
        norenordno_cancelled, _ = self.transaction_manager.get_for_remarks(
            f"{parent_remarks}", OrderStatus.CANCELED
        )
        if norenordno_book_profit or norenordno_cancelled:
            self.logger.debug(
                "book_profit %s or cancelled %s",
                norenordno_book_profit,
                norenordno_cancelled,
            )
            self.transaction_manager.unsubscribe_symbols(symbol_data)
            self.order_queue.remove(remarks)

    def over(self):
        """
        Check if the day is over or order queue is empty
        """
        return (
            not self.order_queue
            or self.transaction_manager.day_over()
            or self._both_legs_rejected()
        )

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
                elif status == OrderStatus.COMPLETE:  ## _stop_loss order is completed
                    ## if cancel_remarks is completed, simply remove remarks from order queue
                    self.order_queue.remove(remarks)

    @delay_decorator(delay=10)
    def cancel_on_profit(self, target_profit: float, target_loss: float):
        """Cancel order using Shoonya API"""
        total_pnl, display_msg = self.transaction_manager.get_pnl()
        if (total_pnl > target_profit) or (total_pnl <= target_loss):
            self.logger.info(
                "Target reached Current Pnl: %.2f | Target: %.2f | Target Loss: %.2f | Cancelling all pending orders",
                total_pnl,
                target_profit,
                traget_loss,
            )
            self._square_off()
        display_msg["Target"] = round(target_profit, 2)
        self.logger.info(json.dumps(display_msg, indent=2))

    @delay_decorator(delay=10)
    def exit_on_book_profit(self):
        """Exit if book profit is reached on each leg"""
        result = True
        for item in ["ce", "pe"]:
            message = get_remarks(instance_id=self.instance_id, msg=f"{item}_straddle")
            book_profit_remarks = f"{message}_book_profit"
            if book_profit_remarks in self.order_queue:
                ## Not yet placed, still in order queue
                result = False
                break
            norenordno, _ = self.transaction_manager.get_for_remarks(
                book_profit_remarks, OrderStatus.COMPLETE
            )
            if not norenordno:
                result = False
                break
        if result:
            self.logger.warning("Book profit reached, sqauring off all pending orders")
            self._square_off()

    def test(self, status: str, interval: int = 15):
        """Test function"""
        return self.transaction_manager.test(status, interval)

    @delay_decorator(delay=60)
    def display_stats(self):
        """Display order queue"""
        self.logger.debug("Order queue: %s", self.order_queue)
        self.logger.debug(
            "Active Connections %d", self.transaction_manager.get_active_connections()
        )

    @delay_decorator(delay=5)
    def _both_legs_rejected(self):
        """Close the transaction if any leg is rejected"""
        result = True
        for item in ["ce", "pe"]:
            message = get_remarks(instance_id=self.instance_id, msg=f"{item}_straddle")
            norenordno, _ = self.transaction_manager.get_for_remarks(
                message, OrderStatus.REJECTED
            )
            if not norenordno:
                result = False
                break
        if result:
            self.logger.warning("Both legs rejected, closing transaction")
        return result

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
                    product_type=self.product_type,
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

    @delay_decorator(delay=15)
    def modify_book_profit_sl(
        self, book_profit_factor: float, diff_threshold: float = 5.0
    ):
        """Modify stop loss order"""
        ## pylint: disable=too-many-locals
        all_orders = self.transaction_manager.get_orders()
        for order in all_orders:
            norenordno = order["norenordno"]
            tradingsymbol = order["tradingsymbol"]
            ## filter order with remarks and OPEN status
            remarks = order["remarks"]
            if (
                not remarks.endswith("_book_profit")
                or order["status"] != OrderStatus.TRIGGER_PENDING
            ):
                continue
            (last_modified_price, qty) = self.transaction_manager.get_order_prices(
                tradingsymbol=tradingsymbol, remarks=remarks
            )
            if not last_modified_price or not qty:
                self.logger.error("Avgprice or qty not available for %s", tradingsymbol)
                continue
            ltp = self.transaction_manager.get_ltp(tradingsymbol)
            if not ltp:
                self.logger.error("LTP not available for %s", tradingsymbol)
                continue
            ## if difference between ltp and rounded_ltp is more than 5%
            factored_price = last_modified_price * book_profit_factor
            diff_percent = (1 - (ltp / factored_price)) * 100
            msg = "Not modifying order"
            if diff_percent > diff_threshold:  ## 5% difference, then modify the order
                sl_price = round_to_point5(factored_price)
                sl_trigger = sl_price - 0.5
                ## modify the stop loss order
                order_data = {
                    "orderno": norenordno,
                    "exchange": get_exchange(tradingsymbol),
                    "tradingsymbol": tradingsymbol,
                    "newquantity": qty,
                    "newprice_type": "SL-LMT",
                    "newprice": sl_price,
                    "newtrigger_price": sl_trigger,
                }
                response = self.api.modify_order(**order_data)
                self.logger.info("Book Profit Order modified: %s", response)
                self.logger.debug(
                    "Book Profit Order modified: %s", json.dumps(order_data, indent=2)
                )
                msg = "Order modified"
            self.logger.debug(
                "%s | LTP: %.2f | Price: %.2f | Diff Percent: %.2f %% | %s",
                tradingsymbol,
                ltp,
                factored_price,
                diff_percent,
                msg,
            )

    @delay_decorator(delay=30)
    def re_enqueue_rejected_order(self):
        """Re-enqueue rejected order"""
        has_rejected = False
        all_orders = self.transaction_manager.get_orders()
        for order in all_orders:
            remarks = order["remarks"]
            status = order["status"]
            if "_book_profit" in remarks and status == OrderStatus.REJECTED:
                has_rejected = True
                self.order_queue.add(remarks)
                self.logger.info("Order re-enqueued: %s", remarks)
        self.logger.debug("Rejected book_profit order re-enqueued: %s", has_rejected)

    @delay_decorator(delay=15)
    def place_book_profit_sl(
        self, book_profit_price: float, qty: int, diff_threshold: float = 5.0
    ):
        """Place book sl v2 for each leg"""
        ## pylint: disable=too-many-locals
        ## fast lookup from order queue
        ## if both of ce_straddle_book_profit and pe_straddle_book_profit is executed return
        ce_remarks = get_remarks(
            instance_id=self.instance_id, msg="ce_straddle_book_profit"
        )
        pe_remarks = get_remarks(
            instance_id=self.instance_id, msg="pe_straddle_book_profit"
        )
        if ce_remarks not in self.order_queue and pe_remarks not in self.order_queue:
            return
        ## get all orders
        all_orders = self.transaction_manager.get_orders()
        for order in all_orders:
            status = order["status"]
            remarks = order["remarks"]
            ## parent order not placed yet
            if not remarks.endswith("_straddle") or status != OrderStatus.COMPLETE:
                continue
            book_profit_remark = f"{remarks}_book_profit"
            if book_profit_remark not in self.order_queue:
                continue  ## Already placed
            ## get the ltp
            tradingsymbol = order["tradingsymbol"]
            ltp = self.transaction_manager.get_ltp(tradingsymbol)
            if not ltp:
                self.logger.error("LTP not available for %s", tradingsymbol)
                continue
            diff_percent = (1 - (ltp / book_profit_price)) * 100
            msg = "Not placing order"
            if diff_percent > diff_threshold:  ## 5% difference, then place the order
                ## place order
                order_data = {
                    "buy_or_sell": "B",
                    "product_type": self.product_type,
                    "exchange": get_exchange(tradingsymbol),
                    "tradingsymbol": tradingsymbol,
                    "quantity": qty,
                    "discloseqty": 0,
                    "price_type": "SL-LMT",
                    "price": book_profit_price,
                    "trigger_price": (book_profit_price - 0.5),
                    "retention": "DAY",
                    "remarks": f"{remarks}_book_profit",
                }
                response = self.api.place_order(**order_data)
                self.order_queue.remove(f"{remarks}_book_profit")
                self.logger.info("Book Profit Order placed: %s", response)
                self.logger.debug(
                    "Book Profit Order placed: %s", json.dumps(order_data, indent=2)
                )
                msg = "Order placed"
            self.logger.debug(
                "%s | LTP: %.2f | Price: %.2f | Diff Percent: %.2f %% | %s",
                tradingsymbol,
                ltp,
                book_profit_price,
                diff_percent,
                msg,
            )


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
    same_premium = args.same_premium
    show_strikes = args.show_strikes
    logger = configure_logger(args.log_level, f"shoonya_transaction_{index}")

    logger.debug("Input Arguments: %s", json.dumps(vars(args), indent=2))
    if not instance_id:
        instance_id = f"shoonya_{get_instance_id()}"
    else:
        logger.warning("Instance id provided, this is running for previous instance")

    logger.info("Running Instance: %s", instance_id)

    api = ShoonyaApiPy(cred_file)

    ## validate the quantity
    validate(qty, index)

    strikes_data = get_staddle_strike(api, symbol_index=index, qty=qty)
    min_ltp = min(float(strikes_data["ce_ltp"]), float(strikes_data["pe_ltp"]))
    if same_premium and not show_strikes:
        ## keep checking for same premium, if not same, keep updating the strikes,
        ## after every 5 minutes
        diff = abs(float(strikes_data["ce_ltp"]) - float(strikes_data["pe_ltp"]))
        per_change = (diff / min_ltp) * 100
        while per_change > 25:  ## if difference is more than 25%, re-check the strikes
            ## Display ltp values too
            logger.info(
                "Current LTP: CE: %.2f | PE: %.2f",
                float(strikes_data["ce_ltp"]),
                float(strikes_data["pe_ltp"]),
            )
            logger.info(
                "Difference in premium: %.2f (change: %.2f %%), re-checking after 5 minutes",
                diff,
                per_change,
            )
            ## use a visual indicator to show that the script is running
            wait_with_progress(300)
            strikes_data = get_staddle_strike(api, symbol_index=index, qty=qty)
            diff = abs(float(strikes_data["ce_ltp"]) - float(strikes_data["pe_ltp"]))
            min_ltp = min(float(strikes_data["ce_ltp"]), float(strikes_data["pe_ltp"]))
            per_change = (diff / min_ltp) * 100

    premium = qty * (float(strikes_data["ce_ltp"]) + float(strikes_data["pe_ltp"]))
    premium_lost = (
        qty
        * sl_factor
        * (float(strikes_data["ce_sl_ltp"]) + float(strikes_data["pe_sl_ltp"]))
    )
    max_loss = (premium - premium_lost) - strikes_data["max_strike_diff"] * qty
    if target_mtm == -1:
        logging.info("Target MTM not provided, calculating from premium")
        target_mtm = premium * target
    elif target_mtm != -1 and target != 0.35:  ## not equal to default
        logging.info("Target MTM provided and target provided, using minimum")
        target_mtm = min(premium * target, target_mtm)
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

    redis_store = DataStore(instance_id)
    ## Add target_mtm to data store
    redis_store.set_param("target_mtm", target_mtm)

    if show_strikes:
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
            book_profit_ltp = round_to_point5(
                min_ltp * book_profit
            )  ## pylint: disable=unused-variable
            code_sl = f"{strikes_data[f'{item}_sl_code']}"

            shoonya_transaction.place_order(  ## Place straddle order
                {
                    "buy_or_sell": "S",
                    "product_type": "M",  ## M for NRML, I for MIS
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
                    "product_type": "M",  ## M for NRML, I for MIS
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
            shoonya_transaction.cancel_on_book_profit(  ## Cancel stop loss order,
                ## if book profit is COMPLETE
                remarks=f"{subscribe_msg}_cancel",
                parent_remarks=f"{subscribe_msg}_book_profit",
                parent_status=OrderStatus.COMPLETE,
                cancel_remarks=f"{subscribe_msg}_stop_loss",
            )
            shoonya_transaction.cancel_on_profit(
                target_profit=redis_store.retrieve_param("target_mtm"),
                target_loss=-1.0 * target_mtm * 1.33,  ## Hardcoded
            )  ## Cancel all orders if target is reached
            ## Exit if book profit is reached on each leg
            # shoonya_transaction.exit_on_book_profit()
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
            # shoonya_transaction.place_book_profit_sl(  ## Place the book profit order
            #     book_profit_price=book_profit_ltp, qty=qty
            # )
            ## Modify book profit order
            # shoonya_transaction.modify_book_profit_sl(book_profit_factor=book_profit)
            ## Re-enqueue rejected order
            # shoonya_transaction.re_enqueue_rejected_order()
            shoonya_transaction.display_stats()


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
