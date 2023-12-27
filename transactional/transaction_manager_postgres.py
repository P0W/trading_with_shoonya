"""
Transaction manager
"""
import datetime
import json
import logging
import sys
import threading
from typing import Any, List
from typing import Dict

import psycopg2.extras
from const import OrderStatus
from utils import full_stack

import order_manager  ## pylint: disable=import-error


class TransactionManager(order_manager.OrderManager):
    """
    Transaction manager class
    """

    def __init__(self, api_object: Any, config: Dict):
        """
        Initialize the transaction manager
        """
        super().__init__(api_object, config)
        self.logger = logging.getLogger(__name__)
        self.lock = threading.Lock()
        ## create a connection to the database
        conn_string = f"user={config['user']} \
            password={config['password']} \
                port={config['port']} \
                    dbname={config['dbname']}"
        self.logger.info("Connecting to database %s", conn_string)
        self.instance_id = config["instance_id"]

        self.conn = psycopg2.connect(conn_string)
        self.cursor = self.conn.cursor(cursor_factory=psycopg2.extras.NamedTupleCursor)
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
                status TEXT,
                instance TEXT)"""
            )
            ## create a table liveltp schema : (symbolcode, ltp)
            table_name = "liveltp"
            self.logger.info("Creating table liveltp")
            self.cursor.execute(
                f"""CREATE TABLE IF NOT EXISTS {table_name}
                (symbolcode TEXT PRIMARY KEY,
                ltp REAL)"""
            )

            ## create a table symbols schema : (symbolcode, exchange, tradingsymbol, instance)
            table_name = "symbols"
            self.logger.info("Creating table symbols")
            self.cursor.execute(
                f"""CREATE TABLE IF NOT EXISTS {table_name}
                (symbolcode TEXT PRIMARY KEY,
                exchange TEXT,
                tradingsymbol TEXT,
                instance TEXT)"""
            )

            self.conn.commit()

    def _check_for_self(self, remarks: str) -> bool:
        """
        Check if the order is placed by self
        """
        return remarks.startswith(self.instance_id)

    def _event_handler_order_update(self, order_data: Dict):
        """
        Event handler for order update
        """
        remarks = order_data["remarks"]
        if not self._check_for_self(remarks):
            logging.debug("Ignoring other instance order update %s", remarks)
            return

        norenordno = order_data["norenordno"]
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
            "instance": self.instance_id,
        }
        with self.lock:
            ## pylint: disable=line-too-long
            self.cursor.execute(
                """INSERT INTO transactions
                (norenordno, utc_timestamp, remarks, avgprice, qty, buysell, tradingsymbol, status, instance)
                VALUES (%(norenordno)s, to_timestamp(%(utc_timestamp)s), %(remarks)s, %(avgprice)s, %(qty)s, %(buysell)s, %(tradingsymbol)s, %(status)s , %(instance)s)
                ON CONFLICT (norenordno) DO UPDATE
                SET utc_timestamp = to_timestamp(%(utc_timestamp)s),
                remarks = %(remarks)s,
                avgprice = %(avgprice)s,
                qty = %(qty)s,
                buysell = %(buysell)s,
                tradingsymbol = %(tradingsymbol)s,
                status = %(status)s,
                instance = %(instance)s
                """,
                upsert_data,
            )
            self.conn.commit()
        self.logger.debug(
            "Upserting into table transactions: %s", json.dumps(upsert_data, indent=2)
        )

    def _event_handler_feed_update(self, tick_data: Dict):
        """
        Event handler for feed update
        """
        try:
            if "lp" in tick_data:
                lp = float(tick_data["lp"])
                tk = tick_data["tk"]
                ## upsert into the table liveltp
                with self.lock:
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
            sys.exit(-1)

    def subscribe_symbols(self, symbol: Dict):
        """
        Subscribe to symbols
        """
        symbolcode = symbol["symbolcode"]
        exchange = symbol["exchange"]
        tradingsymbol = symbol["tradingsymbol"]
        subscribe_code = f"{exchange}|{symbolcode}"
        self.subscribe(subscribe_code)

        ## upsert into the table symbols
        upsert_data = {
            "symbolcode": symbolcode,
            "exchange": exchange,
            "tradingsymbol": tradingsymbol,
            "instance": self.instance_id,
        }
        with self.lock:
            self.logger.info(
                "Upserting into table symbols %s", json.dumps(upsert_data, indent=2)
            )
            self.cursor.execute(
                """INSERT INTO symbols
                (symbolcode, exchange, tradingsymbol, instance)
                VALUES (%(symbolcode)s, %(exchange)s, %(tradingsymbol)s, %(instance)s)
                ON CONFLICT (symbolcode) DO UPDATE
                SET exchange = %(exchange)s,
                tradingsymbol = %(tradingsymbol)s,
                instance = %(instance)s
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
        subscribe_code = f"{exchange}|{symbolcode}"
        self.unsubscribe(subscribe_code)

        ## delete from the table symbols
        ## Note: Don't delete from the table symbols,
        ## to keep track of all the symbols and correctly calculate PnL
        # with self.lock:
        #     self.logger.info("Deleting from table symbols")
        #     self.cursor.execute(
        #         """DELETE FROM symbols
        #         WHERE symbolcode=%s AND instance=%s
        #         """,
        #         (symbolcode, self.instance_id),
        #     )
        #     self.conn.commit()

    def get_for_remarks(
        self, remarks: str, expected: OrderStatus = None
    ) -> (str, OrderStatus):
        """
        Get norenordno if order executed for remark,
        for utc_timestamp greater than start_time, otherwise None
        """
        response = None
        with self.lock:
            try:
                self.cursor.execute(
                    """SELECT norenordno, status
                    FROM transactions
                    WHERE remarks=%s AND instance=%s
                    """,
                    (remarks, self.instance_id),
                )
                response = self.cursor.fetchone()
            except psycopg2.OperationalError as ex:
                self.logger.error("Exception: %s", ex)
                ## stacktrace
                self.logger.error(full_stack())
        if response is not None:
            norenordno = response.norenordno
            status = response.status
            expected_list = expected
            if expected and isinstance(expected, OrderStatus):
                expected_list = [expected.value]
            if expected is None or status in expected_list:
                return norenordno, OrderStatus(status)
        return None, None

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
            try:
                self.cursor.execute(
                    """SELECT transactions.avgprice, transactions.qty, transactions.buysell, 
                            transactions.tradingsymbol, liveltp.ltp
                    FROM transactions
                    JOIN symbols ON transactions.instance = symbols.instance 
                                    AND transactions.tradingsymbol = symbols.tradingsymbol
                    JOIN liveltp ON symbols.symbolcode = liveltp.symbolcode
                    WHERE transactions.instance = %s""",
                    (self.instance_id,),
                )
                rows = self.cursor.fetchall()
            except Exception as e:  ## pylint: disable=broad-exception-caught
                self.logger.error("Failed to execute SQL query %s", e)
                self.logger.error(full_stack())
                return -999.999
        total_pnl = 0
        msg = []
        for row in rows:
            avgprice = float(row.avgprice)
            qty = int(row.qty)
            buysell = row.buysell
            tradingsymbol = row.tradingsymbol
            ltp = float(row.ltp)
            if avgprice == -1 or qty == -1:
                continue
            if buysell == "B":
                pnl = (ltp - avgprice) * qty
            else:
                pnl = (avgprice - ltp) * qty
            key = f"{tradingsymbol} {buysell} {qty} @ {avgprice:.2f}"
            msg.append({key: f"{ltp:.2f} : {pnl:.2f}"})
            total_pnl += pnl
        if msg:
            msg.append({"Total": f"{total_pnl:.2f}"})
            self.logger.info(json.dumps(msg, indent=1))
        return total_pnl

    def get_orders(self) -> List[Dict]:
        """Get all orders for this instance"""
        rows = []
        with self.lock:
            try:
                self.cursor.execute(
                    """SELECT norenordno, remarks, avgprice, qty, buysell, tradingsymbol, status
                    FROM transactions
                    WHERE instance = %s""",
                    (self.instance_id,),
                )
                rows = self.cursor.fetchall()
            except Exception as e:  ## pylint: disable=broad-exception-caught
                self.logger.error("Failed to execute SQL query %s", e)
                self.logger.error(full_stack())
                return []
        orders = []
        for row in rows:
            orders.append(
                {
                    "norenordno": row.norenordno,
                    "remarks": row.remarks,
                    "avgprice": row.avgprice,
                    "qty": row.qty,
                    "buysell": row.buysell,
                    "tradingsymbol": row.tradingsymbol,
                    "status": OrderStatus(row.status),
                }
            )
        return orders

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
                    WHERE instance = %s
                    """,
                    (status.value, self.instance_id),
                )
                self.conn.commit()
            self.logger.info("Test complete")
            return True
        return False
