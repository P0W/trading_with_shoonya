"""
    Event driven pub-sub trading strategy for iron fly short straddle
"""
import argparse
import json
import sys
import time

from client_shoonya import ShoonyaApiPy
from event_engine import EventEngine
from utils import configure_logger
from utils import get_exchange
from utils import get_staddle_strike
from utils import round_to_point5
from utils import validate


def parse_args():
    """
    Parse the arguments
    """
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
            "SENSEX",
            "BANKEX",
        ],
    )
    args.add_argument("--qty", required=True, type=int, help="Quantity to trade")
    args.add_argument(
        "--sl_factor",
        default=1.30,
        type=float,
        help="Stop loss factor | default 30%% on individual leg",
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
    args.add_argument(
        "--pnl-display-interval",
        default=15,
        type=int,
        help="PnL display interval in seconds",
    )
    args.add_argument(
        "--target-mtm",
        default=-1,
        type=float,
        help="Target MTM profit",
    )
    args.add_argument(
        "--book-profit", default=0.2, type=float, help="Book profit % of premium left"
    )

    return args.parse_args()


## pylint: disable=too-many-locals,too-many-statements
def main(args):
    """
    Main
    """
    qty = args.qty
    sl_factor = args.sl_factor
    target = args.target
    index = args.index
    show_strikes = args.show_strikes
    target_mtm = args.target_mtm
    book_profit = args.book_profit

    logging = configure_logger(args.log_level, f"shoonya_evt_driven_{index}")
    logging.debug("Input Arguments: %s", json.dumps(vars(args), indent=2))

    api = ShoonyaApiPy()
    pnl_display_interval = args.pnl_display_interval

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

    evt_engine = EventEngine(api, target_mtm, pnl_display_interval)

    def place_short_straddle(cbk_args):
        """
        Executes for placing a straddle leg - very first step
        """
        response = api.place_order(**cbk_args)
        evt_engine.add_existing_orders(response["norenordno"])

    def on_straddle_leg_complete(cbk_args):
        """
        Executes when a straddle leg is completed/filled
        """
        ## cbk_args is the callback arguments passed from the event
        ## engine when the order is completed and filled
        ## Place a stop loss order on OTM leg from user_data

        user_data = cbk_args["user_data"]
        ## code/token of the straddle leg is not send in the callback args,
        ## get from user_data
        code = user_data["code"]

        response = api.place_order(
            buy_or_sell=user_data["buy_or_sell"],
            product_type=user_data["product_type"],
            exchange=user_data["exchange"],
            tradingsymbol=user_data["tradingsymbol"],
            quantity=user_data["quantity"],
            discloseqty=user_data["discloseqty"],
            price_type=user_data["price_type"],
            price=user_data["price"],
            trigger_price=user_data["trigger_price"],
            retention=user_data["retention"],
            remarks=user_data["remarks"],
        )

        ## The tradingsymbol,fillshares,flprc,buy_or_sell,norenordno
        ## are of the placed aka straddle leg
        tsym = cbk_args["tsym"]
        fillshares = int(cbk_args["fillshares"])
        flprc = float(cbk_args["flprc"])
        buy_or_sell = cbk_args["trantype"]
        norenordno = cbk_args["norenordno"]

        ## Subscribe to the straddle leg to get the pnl updates
        exchange = get_exchange(tsym)
        evt_engine.subscribe(f"{exchange}|{code}")

        ## Now add the straddle leg to the event engine which is placed
        ## This is required to track pnl and squaring off the position

        evt_engine.add_symbol_init_data(
            symbol_code=code,
            qty=fillshares,
            avg_price=flprc,
            buy_or_sell=buy_or_sell,
            norenordno=norenordno,
            tradingsymbol=tsym,
        )
        ## We'd need the response to get the order number of the stop loss order
        ## to cancel it when the target is hit
        evt_engine.add_existing_orders(response["norenordno"])

        ## Place a book profit order
        logging.info("Placing book profit order")
        response = api.place_order(**user_data["book_profit_order"])
        evt_engine.add_existing_orders(response["norenordno"])

    def stop_loss_executed(cbk_args):
        """
        Executes when stop loss, an OTM leg gets executed
        """
        ## code/token of the straddle leg is not send in the callback args,
        ## get from user_data
        user_data = cbk_args["user_data"]
        code = user_data["code"]
        instrument = user_data["instrument"]
        evt_engine.subscribe(instrument)

        fillshares = int(cbk_args["fillshares"])
        flprc = float(cbk_args["flprc"])
        buy_or_sell = cbk_args["trantype"]
        norenordno = cbk_args["norenordno"]
        tsym = cbk_args["tsym"]

        evt_engine.add_symbol_init_data(
            symbol_code=code,
            qty=fillshares,
            avg_price=flprc,
            buy_or_sell=buy_or_sell,
            norenordno=norenordno,
            tradingsymbol=tsym,
        )

    def book_profit_executed(cbk_args):
        """
        Executes when book profit order, an ITM leg gets executed
        """
        ## code/token of the straddle leg is not send in the callback args,
        ## get from user_data
        user_data = cbk_args["user_data"]
        instrument = user_data["instrument"]
        evt_engine.unsubscribe(instrument)

        ## get all pending orders and cancel them
        remark = user_data["remarks"]
        ## stripoff _book_profit from the remark and add _stop_loss
        remark = remark.replace("_book_profit", "_stop_loss")
        evt_engine.cancel_all_orders(remark)

    if show_strikes:
        sys.exit(0)

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
        evt_engine.register(
            place_short_straddle,
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
            },
            subscribe_msg,
            on_straddle_leg_complete,
            {
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
                "code": code,
                "book_profit_order": {
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
            },
        )

        evt_engine.evt_register(
            f"{subscribe_msg}_stop_loss",
            stop_loss_executed,
            {"instrument": f"{get_exchange(sl_symbol)}|{code_sl}", "code": code_sl},
        )

        evt_engine.evt_register(
            f"{subscribe_msg}_book_profit",
            book_profit_executed,
            {"instrument": f"{get_exchange(symbol)}|{code}"},
        )

    evt_engine.start()
    while evt_engine.is_running() and not evt_engine.day_over():
        ## Look for any registered events and process them, otherwise keep waiting
        evt_engine.run()
    logging.info("Exiting")
    time.sleep(5)
    evt_engine.stop()
    time.sleep(2)
    logging.info("Good Bye!")


if __name__ == "__main__":
    cli_args = parse_args()
    main(cli_args)
