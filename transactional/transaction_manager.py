"""
Transaction manager
"""
import datetime
import json
import logging
import sqlite3
import sys
import threading
from typing import Dict

from const import OrderStatus
from utils import full_stack

## pylint: disable=import-error
import order_manager


class TransactionManager(order_manager.OrderManager):
    """
    Transaction manager class
    """

    def __init__(self, api_object, config):
        super().__init__(api_object, config)
        self.logger = logging.getLogger(__name__)
        self.lock = threading.Lock()
        ## create a connection to the database
        self.conn = sqlite3.connect(self.config["db_file"], check_same_thread=False)
        ## get the current unix utc_timestamp using datetime
        self.start_time = self._get_utc_timestamp()
        self._create_tables()

    def _get_utc_timestamp(self):
        """Get the current utc_timestamp"""
        return datetime.datetime.utcnow().timestamp()

    def _create_tables(self):
        """Create a table transaction in the database"""
        with self.lock:
            table_name = "transactions"
            self.logger.info("Creating table transactions")
            self.conn.execute(
                f"""CREATE TABLE IF NOT EXISTS {table_name}
                (norenordno TEXT PRIMARY KEY,
                utc_timestamp REAL,
                remarks TEXT,
                avgprice REAL,
                qty INTEGER,
                buysell char(1),
                tradingsymbol TEXT,
                status TEXT)"""
            )
            ## create a table liveltp schema : (symbolcode, ltp)
            table_name = "liveltp"
            self.logger.info("Creating table liveltp")
            self.conn.execute(
                f"""CREATE TABLE IF NOT EXISTS {table_name}
                (symbolcode TEXT PRIMARY KEY,
                ltp REAL)"""
            )

            ## create a table symbols schema : (symbolcode, exchange, tradingsymbol)
            table_name = "symbols"
            self.logger.info("Creating table symbols")
            self.conn.execute(
                f"""CREATE TABLE IF NOT EXISTS {table_name}
                (symbolcode TEXT PRIMARY KEY,
                exchange TEXT,
                tradingsymbol TEXT)"""
            )
            self.conn.commit()

    def _event_handler_order_update(self, order_data):
        """
        Event handler for order update
        """
        norenordno = order_data["norenordno"]
        remarks = order_data["remarks"]
        avgprice = -1
        qty = -1
        if "fillshares" in order_data and "flprc" in order_data:
            avgprice = order_data["flprc"]
            qty = order_data["fillshares"]
        buy_sell = order_data["trantype"]
        trading_symbol = order_data["tsym"]
        status = order_data["status"]
        utc_timestamp = self._get_utc_timestamp()
        ## upsert into the table transactions
        upsert_data = {
            "norenordno": norenordno,
            "utc_timestamp": utc_timestamp,
            "remarks": remarks,
            "avgprice": avgprice,
            "qty": qty,
            "buy_sell": buy_sell,
            "trading_symbol": trading_symbol,
            "status": status,
        }
        with self.lock:
            ## pylint: disable=line-too-long
            self.conn.execute(
                """INSERT OR REPLACE INTO transactions
                (norenordno, utc_timestamp, remarks, avgprice, qty, buysell, tradingsymbol, status)
                VALUES (:norenordno, :utc_timestamp, :remarks, :avgprice, :qty, :buy_sell, :trading_symbol, :status)""",
                upsert_data,
            )
            self.conn.commit()
        self.logger.debug(
            "Order update: %s",
            json.dumps(
                upsert_data,
                indent=2,
            ),
        )

    def _event_handler_feed_update(self, tick_data):
        """
        Event handler for feed update
        """
        try:
            if "lp" in tick_data:
                lp = float(tick_data["lp"])
                tk = tick_data["tk"]
                self.logger.debug("Feed update: %s %s", tk, lp)
                ## upsert into the table liveltp
                with self.lock:
                    self.conn.execute(
                        """INSERT OR REPLACE INTO liveltp
                        (symbolcode, ltp)
                        VALUES (?, ?)""",
                        (tk, lp),
                    )
                    self.conn.commit()
        except Exception as ex:  ## pylint: disable=broad-except
            self.logger.error("Exception in feed update: %s", ex)
            ## stacktrace
            self.logger.error(full_stack())
            sys.exit(-1)

    def subscribe_symbols(self, symbol: Dict):
        """
        Subscribe to symbols
        """
        symbolcode = symbol["symbolcode"]
        exchange = symbol["exchange"]
        tradingsymbol = symbol["tradingsymbol"]
        subscribe_code = f"{symbolcode}|{exchange}"
        self.subscribe(subscribe_code)
        with self.lock:
            ## add to the table symbols
            self.conn.execute(
                """INSERT OR REPLACE INTO symbols
                (symbolcode, exchange, tradingsymbol)
                VALUES (?, ?, ?)""",
                (symbolcode, exchange, tradingsymbol),
            )
            self.conn.commit()

    def unsubscribe_symbols(self, symbol: Dict):
        """
        Unsubscribe from symbols
        """
        symbolcode = symbol["symbolcode"]
        exchange = symbol["exchange"]
        subscribe_code = f"{symbolcode}|{exchange}"
        self.unsubscribe(subscribe_code)
        with self.lock:
            ## remove from the table symbols
            self.conn.execute(
                """DELETE FROM symbols WHERE symbolcode=? AND exchange=?""",
                (symbolcode, exchange),
            )
            self.conn.commit()

    def get_for_remarks(self, remark: str, expected: OrderStatus = None) -> str:
        """
        Get norenordno if order executed for remark,
        for utc_timestamp greater than start_time, otherwise None
        """
        reponse = None
        with self.lock:
            try:
                cursor = self.conn.execute(
                    """SELECT norenordno, status 
                        FROM transactions 
                        WHERE remarks=? AND utc_timestamp>?""",
                    (remark, self.start_time),
                )
                reponse = cursor.fetchone()
            except sqlite3.OperationalError as ex:
                self.logger.error("Exception: %s", ex)
                ## stacktrace
                self.logger.error(full_stack())
        if reponse is not None:
            return_norenordno = reponse[0]
            status = reponse[1]
            expected_list = expected.value
            if expected and isinstance(expected, OrderStatus):
                expected_list = [expected.value]
            if expected is None or status in expected_list:
                return return_norenordno
        return None

    def get_pnl(self):
        """
        Get PnL for all orders, use all three tables,
            liveltp has live prices and symbolcode,
            transactions has avgprice, qty, buysell, tradingsymbol. It does not have symbolcode
            symbols has symbolcode, exchange, tradingsymbol
        Note: symbolcode is not tradingsymbol
        """
        rows = []
        with self.lock:
            cursor = self.conn.execute(
                """SELECT transactions.avgprice, transactions.qty, transactions.buysell, 
                        transactions.tradingsymbol, liveltp.ltp
                        FROM 
                            transactions, liveltp, symbols 
                        WHERE 
                            transactions.tradingsymbol = symbols.tradingsymbol AND 
                            symbols.symbolcode = liveltp.symbolcode"""
            )
            rows = cursor.fetchall()
        total_pnl = 0
        msg = []
        for row in rows:
            avgprice = float(row[0])
            qty = int(row[1])
            buysell = row[2]
            tradingsymbol = row[3]
            ltp = float(row[4])
            if buysell == "B":
                pnl = (ltp - avgprice) * qty
            else:
                pnl = (avgprice - ltp) * qty
            msg.append(
                f"{tradingsymbol} {buysell} {qty} @ {avgprice:.2f} : {ltp:.2f} : {pnl:.2f}"
            )
            total_pnl += pnl
        if msg:
            msg.append(f"Total PnL: {total_pnl:.2f}")
            self.logger.info("\n".join(msg))
        return total_pnl

    def test(self, status: str, interval: int = 15):
        """
        Test
        """
        ## change status to "COMPLETE" for all orders,
        ## check utc_timestamp > start_time, exeute after 15 seconds
        if self._get_utc_timestamp() - self.start_time > interval:
            self.logger.info("Testing TransactionManager %.2f", self.start_time)
            query = f"""UPDATE transactions
                        SET status="{status}" 
                        WHERE utc_timestamp>{self.start_time}"""
            self.logger.info("Query :%s", query)
            with self.lock:
                self.conn.execute(query)
                self.conn.commit()
                return True
        return False
