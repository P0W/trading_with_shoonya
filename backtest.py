"""
Backtesting a short straddle strategy
"""

import argparse
import datetime
import logging
from datetime import time

import backtrader as bt
import pandas as pd


## pylint: disable=too-many-instance-attributes
class ShortStraddle(bt.Strategy):
    """Short Straddle Strategy"""

    params = (
        ("sl_factor", 1.75),
        ("target_factor", 0.4),
        ("exit_time", time(15, 15)),
    )

    def __init__(self):
        """Initialize the strategy"""
        super().__init__()
        self.ce_data = self.datas[0]
        self.pe_data = self.datas[1]
        self.ce_avg = None
        self.pe_avg = None
        self.ce_sl = None
        self.pe_sl = None
        self.target = None
        self.order = None
        self.mtm = 0
        self.straddle_premium = None
        self.o = {}
        self.logger = logging.getLogger(__name__)

    ## pylint: disable=no-member
    def next(self):
        """Define the trading logic"""
        ce_time = self.ce_data.datetime.datetime()
        pe_time = self.pe_data.datetime.datetime()
        ## check if both the data feeds are in sync
        if ce_time != pe_time:
            self.log("Data feeds are not in sync")
            self.log(f"CE Time: {ce_time}, PE Time: {pe_time}")
            return

        current_datetime = ce_time

        if not self.position and current_datetime.time() >= time(10, 00):
            self.ce_avg = self.ce_data.close[0]
            self.pe_avg = self.pe_data.close[0]

            ## If the difference between the put and call options is less than
            ## 25% of the average of the two, place the straddle
            if (
                abs(self.ce_avg - self.pe_avg) / ((self.ce_avg + self.pe_avg) / 2)
                > 0.25
            ):
                return
            self.straddle_premium = self.ce_avg + self.pe_avg
            self.target = self.straddle_premium * self.params.target_factor
            self.ce_sl = self.ce_avg * self.params.sl_factor
            self.pe_sl = self.pe_avg * self.params.sl_factor

            self.log(f"Straddle Placed Price: {self.straddle_premium}")
            self.log(f"CE Avg: {self.ce_avg}")
            self.log(f"PE Avg: {self.pe_avg}")
            self.log(f"CE SL: {self.ce_sl}")
            self.log(f"PE SL: {self.pe_sl}")

            self.o["ce"] = self.sell(data=self.ce_data, size=1, price=self.ce_avg)
            self.o["pe"] = self.sell(data=self.pe_data, size=1, price=self.pe_avg)

            self.buy(
                data=self.ce_data, size=1, price=self.ce_sl, exectype=bt.Order.Stop
            )
            self.buy(
                data=self.pe_data, size=1, price=self.pe_sl, exectype=bt.Order.Stop
            )

        elif self.position and current_datetime.time() == self.params.exit_time:
            self.close(data=self.ce_data)
            self.close(data=self.pe_data)
            self.log("Exiting the trade")

        ## exit if target is hit premium is decayed by 40%
        elif self.position and self.mtm >= self.target:
            self.close(data=self.ce_data)
            self.close(data=self.pe_data)
            self.log("Target Hit, Exiting the trade")

        if self.position:
            self.mtm = (
                self.ce_avg
                + self.pe_avg
                - (self.ce_data.close[0] + self.pe_data.close[0])
            )
            # self.log(f"Straddle Premium: {self.straddle_premium}")

    def log(self, txt, dt=None):
        """Logging function fot this strategy"""
        dt = dt or self.data.datetime[0]
        if isinstance(dt, float):
            dt = bt.num2date(dt)
        self.logger.info("%s, %s", dt.isoformat(), txt)

    def notify_order(self, order):
        super().notify_order(order)
        if order.status in [order.Submitted, order.Accepted]:
            # Buy/Sell order submitted/accepted to/by broker - Nothing to do
            self.log("ORDER ACCEPTED/SUBMITTED", dt=order.created.dt)
            ## display buy/sell order details
            buy_sell = "BUY" if order.isbuy() else "SELL"
            self.log(
                f"{buy_sell} ORDER: {order.getstatusname()}, Price: {order.created.price}"
            )
            return

        if order.status in [order.Expired]:
            self.log("BUY EXPIRED")

        ## Stop loss order executed

        elif order.status in [order.Completed]:
            ## pylint: disable=line-too-long
            if order.isbuy():
                self.log(
                    f"STOP LOSS ORDER EXECUTED, Price: {order.executed.price}, Cost: {order.executed.value}, Comm: {order.executed.comm}"
                )

            else:  # Sell
                self.log(
                    f"SELL ORDER EXECUTED, Price: {order.executed.price}, Cost: {order.executed.value}, Comm: {order.executed.comm}"
                )
        else:
            self.log("ORDER STATUS: %s", order.getstatusname())
        # Sentinel to None: new orders allowed
        self.order = None


class FixedSize(bt.Sizer):
    """Fixed size sizer"""

    params = (("stake", 50),)

    ## pylint: disable=no-member
    def _getsizing(self, comminfo, cash, data, isbuy):
        """Returns the stake size"""
        return self.params.stake


class FixedCommisionScheme(bt.CommInfoBase):
    """Fixed commission scheme"""

    params = (
        ("commission", 10),
        ("stocklike", True),
        ("commtype", bt.CommInfoBase.COMM_FIXED),
    )

    ## pylint: disable=no-member
    def _getcommission(self, size, price, pseudoexec):
        """Returns the commission based on the parameters given"""
        return self.params.commission


class Expectancy(bt.Analyzer):
    """Analyzer to calculate the expectancy of the strategy"""

    def __init__(self):
        """Initialize the analyzer"""
        super().__init__()
        self.wins = 0
        self.losses = 0
        self.total_gain = 0
        self.total_loss = 0

    def notify_trade(self, trade):
        """Update the wins and losses based on the trade results"""
        if trade.isclosed:
            if trade.pnl > 0:
                self.wins += 1
                self.total_gain += trade.pnl
            else:
                self.losses += 1
                self.total_loss += trade.pnl

    def get_analysis(self):
        """Returns the expectancy of the strategy"""
        try:
            expectancy = (
                (self.total_gain / self.wins) / (self.total_loss / self.losses)
                if self.losses > 0
                else float("inf")
            )
            return {"expectancy": expectancy}
        except:  ## pylint: disable=bare-except
            return {"expectancy": 0}


def main(ce_srike_file: str, pe_srike_file: str):
    """Main function to run the backtest"""
    cerebro = bt.Cerebro()
    cerebro.addstrategy(ShortStraddle, sl_factor=1.55, target_factor=0.4)

    pe_df = pd.read_csv(ce_srike_file, parse_dates=True, index_col=0)
    ce_df = pd.read_csv(pe_srike_file, parse_dates=True, index_col=0)

    ## get the starttime which is same for both the dataframes, the first one
    start_time = max(pe_df.index[0], ce_df.index[0])

    print(start_time)

    # Filter the DataFrame
    ce_df = ce_df[ce_df.index >= start_time]
    pe_df = pe_df[pe_df.index >= start_time]

    pe_df.index = pd.to_datetime(pe_df.index)
    pe_df = pe_df.resample("1min").ffill()
    ce_df.index = pd.to_datetime(ce_df.index)
    ce_df = ce_df.resample("1min").ffill()

    ##pylint: disable=unexpected-keyword-arg
    cerebro.adddata(bt.feeds.PandasData(dataname=pe_df), name="pe_data")
    cerebro.adddata(bt.feeds.PandasData(dataname=ce_df), name="ce_data")

    ## set cash
    cerebro.addsizer(FixedSize)
    cerebro.broker.setcash(100000.0)
    ## Add a flat fee of 10 INR per trade
    cerebro.broker.addcommissioninfo(FixedCommisionScheme())
    ## Add analyzers
    cerebro.addanalyzer(bt.analyzers.PyFolio, _name="pyfolio")
    # Add analyzers
    cerebro.addanalyzer(Expectancy, _name="expectancy")
    cerebro.addanalyzer(bt.analyzers.SQN, _name="sqn")
    cerebro.addanalyzer(bt.analyzers.DrawDown, _name="drawdown")
    cerebro.addanalyzer(bt.analyzers.TradeAnalyzer, _name="trades")
    ## Add observers
    cerebro.addobserver(bt.observers.Value)
    cerebro.addobserver(bt.observers.Trades)
    cerebro.addobserver(bt.observers.BuySell)
    cerebro.addobserver(bt.observers.DrawDown)
    cerebro.addobserver(bt.observers.Broker)

    logging.info("Starting Portfolio Value: %.2f", cerebro.broker.getvalue())

    results = cerebro.run()

    logging.info("Final Portfolio Value: %.2f", cerebro.broker.getvalue())

    # Get analysis results
    expectancy = results[0].analyzers.expectancy.get_analysis()["expectancy"]
    sqn = results[0].analyzers.sqn.get_analysis()["sqn"]
    trades = results[0].analyzers.trades.get_analysis()

    ## Display the results
    logging.info("Expectancy: %.2f", expectancy)
    logging.info("SQN: %.2f", sqn)
    ## Display details from the trades its a AutoOrderedDict
    wons = trades["won"]["total"]
    lost = trades["lost"]["total"]
    drawndown = results[0].analyzers.drawdown.get_analysis()
    logging.info("Trades Won: %d", wons)
    logging.info("Trades Lost: %d", lost)
    logging.info("Drawdown: %.2f", drawndown["max"]["drawdown"])

    cerebro.plot()


## add argument for ce and pe files ATM strike price and index
def get_args():
    """Parse the arguments"""
    parser = argparse.ArgumentParser(description="Short Straddle Strategy")
    ## add strike price
    parser.add_argument(
        "-s",
        "--strike",
        type=float,
        required=True,
        help="The ATM strike price",
    )
    ## index
    parser.add_argument(
        "-i",
        "--index",
        type=str,
        required=True,
        help="The index value of the ATM strike price",
        choices=["BANKEX", "NIFTY", "SENSEX", "FINNIFTY", "BANKNIFTY"],
    )
    ## add expiry
    parser.add_argument(
        "-e",
        "--expiry",
        type=str,
        required=True,
        help="The expiry date of the options format: DD-MMM-YYYY",
    )
    ## add data root folder
    parser.add_argument(
        "-d",
        "--data-root",
        type=str,
        required=True,
        help="The root folder where the data is stored",
    )

    return parser.parse_args()


if __name__ == "__main__":
    ## Set the logging level
    logging.basicConfig(level=logging.INFO)
    args = get_args()
    ## convert the expiry date to YYYYMMDD format
    folder_date_dt = datetime.datetime.strptime(args.expiry, "%Y%m%d")
    folder_date = folder_date_dt.strftime("%Y-%m-%d")

    expiry_date = folder_date_dt.strftime("%d_%b_%Y").upper()
    ##pylint: disable=line-too-long
    ce_srike = f"{args.data_root}/{folder_date}/{args.index}/{args.index}_{expiry_date}_CE_{args.strike:.2f}.csv"
    pe_srike = f"{args.data_root}/{folder_date}/{args.index}/{args.index}_{expiry_date}_PE_{args.strike:.2f}.csv"

    logging.info("CE Strike File: %s", ce_srike)
    logging.info("PE Strike File: %s", pe_srike)
    main(ce_srike, pe_srike)
