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

    return args.parse_args()


## pylint: disable=too-many-locals,too-many-statements
def main(args):
    """
    Main
    """
    logging = configure_logger(args.log_level, "shoonya_evt_driven")

    logging.debug("Input Arguments: %s", json.dumps(vars(args), indent=2))

    api = ShoonyaApiPy()
    qty = args.qty
    sl_factor = args.sl_factor
    target = args.target
    index = args.index

    ## validate the quantity
    validate(qty, index)

    strikes_data = get_staddle_strike(api, index)

    premium = args.qty * (float(strikes_data["ce_ltp"]) + float(strikes_data["pe_ltp"]))
    target_mtm = premium * target

    logging.info(
        "Strikes data: %s | Max profit :%.2f | Target : %.2f",
        json.dumps(strikes_data, indent=2),
        premium,
        target_mtm,
    )

    target_mtm = 101.0  ## REMOVE THIS

    evt_engine = EventEngine(api, target_mtm)

    def place_order(cbk_args):
        response = api.place_order(**cbk_args)
        evt_engine.add_existing_orders(response["norenordno"])

    def on_complete(cbk_args):
        """
        On complete
        """
        response = api.place_order(
            buy_or_sell=cbk_args["buy_or_sell"],
            product_type=cbk_args["product_type"],
            exchange=cbk_args["exchange"],
            tradingsymbol=cbk_args["tradingsymbol"],
            quantity=cbk_args["quantity"],
            discloseqty=cbk_args["discloseqty"],
            price_type=cbk_args["price_type"],
            price=cbk_args["price"],
            trigger_price=cbk_args["trigger_price"],
            retention=cbk_args["retention"],
            remarks=cbk_args["remarks"],
        )
        tradingsymbol = cbk_args["tradingsymbol"]
        fillshares = int(cbk_args["fillshares"])
        flprc = float(cbk_args["flprc"])
        buy_or_sell = cbk_args["trantype"]
        code = cbk_args["code"]
        norenordno = cbk_args["norenordno"]
        exchange = get_exchange(tradingsymbol)
        evt_engine.subscribe([f"{exchange}|{code}"])
        evt_engine.add_symbol_init_data(
            symbol_code=code,
            qty=fillshares,
            avg_price=flprc,
            buy_or_sell=buy_or_sell,
            norenordno=norenordno,
            tradingsymbol=tradingsymbol,
        )
        ## We'd need the response to get the order number of the stop loss order
        ## to cancel it when the target is hit
        evt_engine.add_existing_orders(response["norenordno"])

    def stop_loss_executed(cbk_args):
        """
        On stop_loss executed complete
        """
        evt_engine.subscribe([cbk_args["tsym"]])
        fillshares = int(cbk_args["fillshares"])
        flprc = float(cbk_args["flprc"])
        instrument = cbk_args["instrument"]
        buy_or_sell = cbk_args["trantype"]
        code = cbk_args["code"]
        norenordno = cbk_args["norenordno"]
        tradingsymbol = cbk_args["tsym"]
        evt_engine.subscribe([instrument])
        evt_engine.add_symbol_init_data(
            symbol_code=code,
            qty=fillshares,
            avg_price=flprc,
            buy_or_sell=buy_or_sell,
            norenordno=norenordno,
            tradingsymbol=tradingsymbol,
        )

    if args.show_strikes:
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
        code_sl = f"{strikes_data[f'{item}_sl_code']}"
        evt_engine.register(
            place_order,
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

        evt_engine.evt_register(
            f"{subscribe_msg}_stop_loss",
            stop_loss_executed,
            {"instrument": f"{get_exchange(sl_symbol)}|{code_sl}", "code": code_sl},
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
