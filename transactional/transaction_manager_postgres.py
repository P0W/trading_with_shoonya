"""
Transaction manager
"""
import datetime
import json
import logging
import sys
import threading
from typing import Dict

import psycopg2
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
        conn_string = f"user={config['user']} \
            password={config['password']} \
                port={config['port']} \
                    dbname={config['dbname']}"
        self.logger.info("Connecting to database %s", conn_string)
        self.conn = psycopg2.connect(conn_string)
        self.cursor = self.conn.cursor()
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
            self.cursor.execute(
                f"""CREATE TABLE IF NOT EXISTS {table_name}
                (norenordno TEXT PRIMARY KEY,
                utc_timestamp TIMESTAMP,
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
            self.cursor.execute(
                f"""CREATE TABLE IF NOT EXISTS {table_name}
                (symbolcode TEXT PRIMARY KEY,
                ltp REAL)"""
            )

            ## create a table symbols schema : (symbolcode, exchange, tradingsymbol)
            table_name = "symbols"
            self.logger.info("Creating table symbols")
            self.cursor.execute(
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
        buysell = order_data["trantype"]
        tradingsymbol = order_data["tsym"]
        status = order_data["status"]
        utc_timestamp = self._get_utc_timestamp()
        ## upsert into the table transactions
        upsert_data = {
            "norenordno": norenordno,
            "utc_timestamp": utc_timestamp,
            "remarks": remarks,
            "avgprice": avgprice,
            "qty": qty,
            "buysell": buysell,
            "tradingsymbol": tradingsymbol,
            "status": status,
        }
        with self.lock:
            self.logger.info("Upserting into table transactions")
            ## pylint: disable=line-too-long
            self.cursor.execute(
                """INSERT INTO transactions
                (norenordno, utc_timestamp, remarks, avgprice, qty, buysell, tradingsymbol, status)
                VALUES (%(norenordno)s, to_timestamp(%(utc_timestamp)s), %(remarks)s, %(avgprice)s, %(qty)s, %(buysell)s, %(tradingsymbol)s, %(status)s)
                ON CONFLICT (norenordno) DO UPDATE
                SET utc_timestamp = excluded.utc_timestamp,
                remarks = excluded.remarks,
                avgprice = excluded.avgprice,
                qty = excluded.qty,
                buysell = excluded.buysell,
                tradingsymbol = excluded.tradingsymbol,
                status = excluded.status
                """,
                upsert_data,
            )
            self.conn.commit()
        self.logger.info("Order update: %s", json.dumps(order_data, indent=2))

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
                    self.logger.info("Upserting into table liveltp")
                    self.cursor.execute(
                        """INSERT INTO liveltp
                        (symbolcode, ltp)
                        VALUES (%(tk)s, %(lp)s)
                        ON CONFLICT (symbolcode) DO UPDATE
                        SET ltp = %(lp)s
                        """,
                        {"tk": tk, "lp": lp},
                    )
                    self.conn.commit()
        except Exception as e:  ## pylint: disable=broad-except
            self.logger.error("Exception: %s", e)
            self.logger.error("Stack Trace : %s", full_stack())
            sys.exit(1)

    def subscribe_symbols(self, symbol: Dict):
        """
        Subscribe to symbols
        """
        symbolcode = symbol["symbolcode"]
        exchange = symbol["exchange"]
        tradingsymbol = symbol["tradingsymbol"]
        subscribe_code = f"{symbolcode}|{exchange}"
        self.subscribe(subscribe_code)

        ## upsert into the table symbols
        upsert_data = {
            "symbolcode": symbolcode,
            "exchange": exchange,
            "tradingsymbol": tradingsymbol,
        }
        with self.lock:
            self.logger.info("Upserting into table symbols")
            self.cursor.execute(
                """INSERT INTO symbols
                (symbolcode, exchange, tradingsymbol)
                VALUES (%(symbolcode)s, %(exchange)s, %(tradingsymbol)s)
                ON CONFLICT (symbolcode) DO UPDATE
                SET exchange = %(exchange)s,
                tradingsymbol = %(tradingsymbol)s
                """,
                upsert_data,
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

        ## delete from the table symbols
        with self.lock:
            self.logger.info("Deleting from table symbols")
            self.cursor.execute(
                f"""DELETE FROM symbols
                WHERE symbolcode = '{symbolcode}'
                """
            )
            self.conn.commit()

    def get_for_remarks(self, remarks: str, expected: OrderStatus = None) -> str:
        """
        Get norenordno if order executed for remark,
        for utc_timestamp greater than start_time, otherwise None
        """
        reponse = None
        with self.lock:
            try:
                self.cursor.execute(
                    """SELECT norenordno, status
                    FROM transactions
                    WHERE remarks=%s AND utc_timestamp > to_timestamp(%s)
                    """,
                    (remarks, self.start_time),
                )
                reponse = self.cursor.fetchone()
            except psycopg2.OperationalError as ex:
                self.logger.error("Exception: %s", ex)
                ## stacktrace
                self.logger.error(full_stack())
        if reponse is not None:
            return_norenordno = reponse[0]
            status = reponse[1]
            expected_list = expected
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
            self.cursor.execute(
                """SELECT transactions.avgprice, transactions.qty, transactions.buysell, 
                        transactions.tradingsymbol, liveltp.ltp
                        FROM 
                            transactions, liveltp, symbols 
                        WHERE 
                            transactions.tradingsymbol = symbols.tradingsymbol AND 
                            symbols.symbolcode = liveltp.symbolcode"""
            )
            rows = self.cursor.fetchall()

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

    def test(self, status: OrderStatus, interval: int = 15):
        """
        Test
        """
        ## change status to "COMPLETE" for all orders,
        ## check utc_timestamp > start_time, exeute after 15 seconds
        if self._get_utc_timestamp() - self.start_time > interval:
            with self.lock:
                self.logger.info("Updating status to COMPLETE")
                self.cursor.execute(
                    """UPDATE transactions
                    SET status = %s
                    WHERE utc_timestamp > to_timestamp(%s)
                    """,
                    (status.value, self.start_time),
                )
                self.conn.commit()
            self.logger.info("Test complete")
            return True
        return False
