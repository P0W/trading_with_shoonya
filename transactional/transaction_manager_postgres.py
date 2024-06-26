"""
Transaction manager
"""

import datetime
import json
import logging
import sys
from contextlib import contextmanager
from typing import Any
from typing import Dict
from typing import List
from typing import Tuple

import psycopg2.extras
from const import OrderStatus
from psycopg2.pool import PoolError
from psycopg2.pool import ThreadedConnectionPool
from utils import full_stack
from utils import log_execution_time

import order_manager  ## pylint: disable=import-error


class TransactionManager(order_manager.OrderManager):
    """
    Transaction manager class
    """

    MIN_CONNECTIONS = (
        3  ## minimum number of connections in the pool, these are created instantly
    )
    MAX_CONNECTIONS = 10  ## maximum number of connections in the pool

    @log_execution_time("Initiate TransactionManager")
    def __init__(self, api_object: Any, config: Dict):
        """
        Initialize the transaction manager
        """
        super().__init__(api_object, config)
        self.logger = logging.getLogger(__name__)
        ## create a connection to the database
        conn_string = f"user={config['user']} \
            password={config['password']} \
                port={config['port']} \
                host=localhost\
                    dbname={config['dbname']}"
        self.logger.info("Connecting to database %s", conn_string)
        self.instance_id = config["instance_id"]

        self.conn_pool = ThreadedConnectionPool(
            TransactionManager.MIN_CONNECTIONS,
            TransactionManager.MAX_CONNECTIONS,
            conn_string,
        )

        ## get the current unix utc_timestamp using datetime
        self.start_time = self._get_utc_timestamp()
        self.active_connections = 0
        self._create_tables()

    @contextmanager
    def getcursor(self):
        """Get a cursor from the connection pool"""
        con = self.conn_pool.getconn()
        self.active_connections += 1
        try:
            yield con.cursor(cursor_factory=psycopg2.extras.NamedTupleCursor)
        except psycopg2.OperationalError as ex:
            self.logger.error("OperationalError Exception: %s", ex)
            ## stacktrace
            self.logger.error(full_stack())
            sys.exit(-1)
        except PoolError as ex:
            self.logger.error("PoolError Exception: %s", ex)
            ## stacktrace
            self.logger.error(full_stack())
            sys.exit(-1)
        finally:
            self.conn_pool.putconn(con)
            self.active_connections -= 1

    def get_active_connections(self):
        """Get the number of active connections in the pool"""
        return self.active_connections

    def _get_utc_timestamp(self):
        """Get the current utc_timestamp"""
        return datetime.datetime.now().timestamp()

    def _create_tables(self):
        """Create a table transaction in the database"""
        with self.getcursor() as cursor:
            table_name = "transactions"
            self.logger.info("Creating table transactions")
            cursor.execute(
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
            cursor.execute(
                f"""CREATE TABLE IF NOT EXISTS {table_name}
                (symbolcode TEXT PRIMARY KEY,
                ltp REAL)"""
            )

            ## create a table symbols schema : (symbolcode, exchange, tradingsymbol, instance)
            table_name = "symbols"
            self.logger.info("Creating table symbols")
            cursor.execute(
                f"""CREATE TABLE IF NOT EXISTS {table_name}
                (symbolcode TEXT,
                exchange TEXT,
                tradingsymbol TEXT,
                instance TEXT,
                PRIMARY KEY (symbolcode, instance))"""
            )

            ## create a table order_prices schema : (tradingsymbol, price, qty, remarks, instance),
            ## make tradingsymbol, instance as primary key
            table_name = "order_prices"
            self.logger.info("Creating table order_prices")
            cursor.execute(
                f"""CREATE TABLE IF NOT EXISTS {table_name}
                (tradingsymbol TEXT,
                price REAL,
                qty INTEGER,
                remarks TEXT,
                instance TEXT,
                PRIMARY KEY (tradingsymbol, instance))"""
            )
            cursor.connection.commit()

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
        price = order_data["prc"]  ## always present
        qty = order_data["qty"]  ## always present
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
        ## pylint: disable=line-too-long
        with self.getcursor() as cursor:
            cursor.execute(
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
            cursor.connection.commit()
        self.logger.debug(
            "Upserting into table transactions: %s", json.dumps(upsert_data, indent=2)
        )

        ## update the order_prices table
        upsert_data = {
            "tradingsymbol": tradingsymbol,
            "price": price,
            "qty": qty,
            "remarks": remarks,
            "instance": self.instance_id,
        }
        with self.getcursor() as cursor:
            ## traddingsymbol and instance are primary keys
            cursor.execute(
                """INSERT INTO order_prices
                (tradingsymbol, price, qty, remarks, instance)
                VALUES (%(tradingsymbol)s, %(price)s, %(qty)s, %(remarks)s, %(instance)s)
                ON CONFLICT (tradingsymbol, instance) DO UPDATE
                SET price = %(price)s,
                qty = %(qty)s,
                remarks = %(remarks)s
                """,
                upsert_data,
            )
            cursor.connection.commit()
        self.logger.debug(
            "Upserting into table order_prices: %s",
            json.dumps(upsert_data, indent=2),
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
                with self.getcursor() as cursor:
                    cursor.execute(
                        """INSERT INTO liveltp
                        (symbolcode, ltp)
                        VALUES (%(tk)s, %(lp)s)
                        ON CONFLICT (symbolcode) DO UPDATE
                        SET ltp = %(lp)s
                        """,
                        {"tk": tk, "lp": lp},
                    )
                    cursor.connection.commit()
        except Exception as e:  ## pylint: disable=broad-except
            self.logger.error("Exception: %s", e)
            self.logger.error("Stack Trace : %s", full_stack())
            sys.exit(-1)

    @log_execution_time("Subscribe")
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
        self.logger.info(
            "Upserting into table symbols %s", json.dumps(upsert_data, indent=2)
        )
        with self.getcursor() as cursor:
            cursor.execute(
                """INSERT INTO symbols
                (symbolcode, exchange, tradingsymbol, instance)
                VALUES (%(symbolcode)s, %(exchange)s, %(tradingsymbol)s, %(instance)s)
                ON CONFLICT (symbolcode, instance) DO UPDATE
                SET exchange = %(exchange)s,
                tradingsymbol = %(tradingsymbol)s
                """,
                upsert_data,
            )
            cursor.connection.commit()

    @log_execution_time("Unsubscribe")
    def unsubscribe_symbols(self, symbol: Dict):
        """
        Unsubscribe from symbols
        """
        symbolcode = symbol["symbolcode"]
        exchange = symbol["exchange"]
        subscribe_code = f"{exchange}|{symbolcode}"
        self.unsubscribe(subscribe_code)

    def get_for_remarks(
        self, remarks: str, expected: OrderStatus = None
    ) -> Tuple[str, OrderStatus]:
        """
        Get norenordno if order executed for remark,
        for utc_timestamp greater than start_time, otherwise None
        """
        response = None
        with self.getcursor() as cursor:
            try:
                cursor.execute(
                    """SELECT norenordno, status
                    FROM transactions
                    WHERE remarks=%s AND instance=%s
                    """,
                    (remarks, self.instance_id),
                )
                response = cursor.fetchone()
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

    @log_execution_time("PnL")
    def get_pnl(self) -> Tuple[float, Dict]:
        """
        Get PnL for all orders, use all three tables,
            liveltp has live prices and symbolcode,
            transactions has avgprice, qty, buysell, tradingsymbol. It does not have symbolcode
            symbols has symbolcode, exchange, tradingsymbol
        Note: symbolcode is not tradingsymbol
        """
        rows = []
        try:
            with self.getcursor() as cursor:
                cursor.execute(
                    """SELECT transactions.avgprice, transactions.qty, transactions.buysell, 
                            transactions.tradingsymbol, liveltp.ltp
                    FROM transactions
                    JOIN symbols ON transactions.instance = symbols.instance 
                                    AND transactions.tradingsymbol = symbols.tradingsymbol
                    JOIN liveltp ON symbols.symbolcode = liveltp.symbolcode
                    WHERE transactions.instance = %s""",
                    (self.instance_id,),
                )
                rows = cursor.fetchall()
        except Exception as e:  ## pylint: disable=broad-exception-caught
            self.logger.error("Failed to execute SQL query %s", e)
            self.logger.error(full_stack())
            return -999.999
        total_pnl = 0
        msg = {}
        for row in rows:
            avgprice = round(float(row.avgprice), 2)
            qty = int(row.qty)
            buysell = row.buysell
            tradingsymbol = row.tradingsymbol
            ltp = round(float(row.ltp), 2)
            if avgprice == -1 or qty == -1:
                continue
            if buysell == "B":
                pnl = (ltp - avgprice) * qty
            else:
                pnl = (avgprice - ltp) * qty
            total_pnl += pnl
            msg[tradingsymbol] = {
                "buysell": buysell,
                "qty": int(qty),
                "avgprice": avgprice,
                "ltp": ltp,
                "pnl": round(pnl, 2),
            }
        if msg:
            ## sort msg by key
            msg = dict(sorted(msg.items()))
            msg["Total"] = round(total_pnl, 2)
        return total_pnl, msg

    def get_orders(self) -> List[Dict]:
        """Get all orders for this instance"""
        rows = []
        try:
            with self.getcursor() as cursor:
                cursor.execute(
                    """SELECT norenordno, remarks, avgprice, qty, buysell, tradingsymbol, status
                    FROM transactions
                    WHERE instance = %s""",
                    (self.instance_id,),
                )
                rows = cursor.fetchall()
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
            self.logger.info("Updating status to COMPLETE")
            with self.getcursor() as cursor:
                cursor.execute(
                    """UPDATE transactions
                    SET status = %s
                    WHERE instance = %s
                    """,
                    (status.value, self.instance_id),
                )
                cursor.connection.commit()
            self.logger.info("Test complete")
            return True
        return False

    def get_ltp(self, tradingsymbol: str) -> float:
        """
        Get the last traded price of the symbol
        """
        with self.getcursor() as cursor:
            cursor.execute(
                """SELECT ltp
                FROM liveltp
                JOIN symbols ON liveltp.symbolcode = symbols.symbolcode
                WHERE symbols.tradingsymbol = %s AND symbols.instance = %s
                """,
                (tradingsymbol, self.instance_id),
            )
            row = cursor.fetchone()
            if row is not None:
                return float(row.ltp)
        return None

    def get_order_prices(self, tradingsymbol: str, remarks: str) -> Tuple[float, int]:
        """
        Get the order price and quantity of the symbol
        """
        with self.getcursor() as cursor:
            cursor.execute(
                """SELECT price, qty
                FROM order_prices
                WHERE tradingsymbol = %s AND instance = %s AND remarks = %s
                """,
                (tradingsymbol, self.instance_id, remarks),
            )
            row = cursor.fetchone()
            if row is not None:
                return float(row.price), int(row.qty)
        return None, None
