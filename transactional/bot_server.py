"""Bot Server class to get PnL, VM stats and kill bot instances"""

import datetime
import logging
import os
import signal
import sys
from contextlib import contextmanager
from typing import Dict
from typing import Tuple

import psutil
import psycopg2.extras
import yaml
from flask import Flask
from flask import jsonify
from flask import render_template
from flask import request
from flask_jwt_extended import create_access_token
from flask_jwt_extended import jwt_required
from flask_jwt_extended import JWTManager
from psycopg2.pool import PoolError
from psycopg2.pool import ThreadedConnectionPool
from werkzeug.security import check_password_hash
from werkzeug.security import generate_password_hash

# Read YAML file
with open("cred.yml", "r", encoding="utf-8") as yml_file:
    yml_config = yaml.safe_load(yml_file)

app = Flask(__name__)
app.config["JWT_SECRET_KEY"] = f"shoonya_{os.urandom(16)}"
jwt = JWTManager(app)

users = {
    yml_config["user"]: generate_password_hash(yml_config["pwd"]),
    # add more users if needed
}


class BotServer:
    """Bot Server class to get PnL, VM stats and kill bot instances"""

    def __init__(self, config: dict):
        self.logger = logging.getLogger(__name__)
        self.pids = []
        self.instances = []
        self.update_pids()
        if self.pids:
            self.instances = [f"shoonya_{pid}" for pid in self.pids]
            self.logger.debug("Instances running: %s", self.instances)
        else:
            ## No instances running
            ## Exit the program
            self.logger.error("No instances running")
        ## create a connection to the database
        conn_string = f"user={config['user']} \
            password={config['password']} \
                port={config['port']} \
                host=localhost\
                    dbname={config['dbname']}"

        self.conn_pool = ThreadedConnectionPool(
            1,  ## Minimum number of connections
            3,  ## Maximum number of connections
            conn_string,
        )

    @contextmanager
    def _getcursor(self):
        """Get a cursor from the connection pool"""
        con = self.conn_pool.getconn()
        try:
            yield con.cursor(cursor_factory=psycopg2.extras.NamedTupleCursor)
        except psycopg2.OperationalError as ex:
            self.logger.error("OperationalError Exception: %s", ex)
            sys.exit(-1)
        except PoolError as ex:
            self.logger.error("PoolError Exception: %s", ex)
            ## stacktrace
            sys.exit(-1)
        finally:
            self.conn_pool.putconn(con)

    def _get_pids_of_process(self, process_name):
        pids = []
        for proc in psutil.process_iter(["pid", "name", "cmdline"]):
            try:
                if (
                    proc.info["name"] == "python"
                    and process_name in proc.info["cmdline"]
                ):
                    pids.append(proc.info["pid"])
            except (psutil.NoSuchProcess, psutil.AccessDenied, psutil.ZombieProcess):
                pass
        return pids

    def update_pids(self):
        """Update pids of the process"""
        process_name = "shoonya_transaction.py"
        self.pids = self._get_pids_of_process(process_name)
        if self.pids:
            self.instances = [f"shoonya_{pid}" for pid in self.pids]
            return True
        return False

    def get_errors(self):
        """Get errors from the log file"""
        today = datetime.datetime.now().strftime("%Y%m%d")
        file_name = None

        ## find the log file which ends with today's date and starts with "shoonya_transaction"
        for file in os.listdir("logs"):
            if (
                file.endswith(today)
                and file.startswith("shoonya_transaction")
                and "log" in file
            ):
                file_name = file
                break
        if file_name:
            with open(file_name, "r", encoding="utf-8") as f:
                logs = f.read()
            errors = [line for line in logs.split("\n") if "ERROR" in line]
            return errors
        return {"message": "No errors found"}

    def _get_pnl(self, instance_id) -> Tuple[float, Dict]:
        """
        Get PnL for all orders, use all three tables,
            liveltp has live prices and symbolcode,
            transactions has avgprice, qty, buysell, tradingsymbol. It does not have symbolcode
            symbols has symbolcode, exchange, tradingsymbol
        Note: symbolcode is not tradingsymbol
        """
        rows = []
        try:
            with self._getcursor() as cursor:
                cursor.execute(
                    """SELECT transactions.avgprice, transactions.qty, transactions.buysell, 
                        transactions.tradingsymbol, liveltp.ltp
                        FROM transactions
                        JOIN symbols ON transactions.instance = symbols.instance 
                                        AND transactions.tradingsymbol = symbols.tradingsymbol
                        JOIN liveltp ON symbols.symbolcode = liveltp.symbolcode
                        WHERE transactions.instance LIKE %s""",
                    ("%" + instance_id + "%",),
                )
                rows = cursor.fetchall()
        except Exception as e:  ## pylint: disable=broad-exception-caught
            self.logger.error("Failed to execute SQL query %s", e)
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

    def get_pnl(self):
        """Get PnL for all instances"""
        pnl = {}
        for instance in self.instances:
            pnl[instance] = self._get_pnl(instance)
        return pnl

    def kill_bot(self):
        """Kill all instances"""
        try:
            for pid in self.pids:
                if os.name == "posix":
                    os.kill(pid, signal.SIGKILL)  ## pylint: disable=no-member
                else:
                    os.kill(pid, signal.SIGTERM)
            self.pids = []
            self.instances = []
            return True
        except Exception as e:  ## pylint: disable=broad-exception-caught
            self.logger.error("Failed to kill instances %s", e)
            return False

    def vm_stats(self):
        """Get VM stats"""
        vm_stats = {}
        vm_stats["cpu"] = psutil.cpu_percent()
        vm_stats["memory"] = psutil.virtual_memory().percent
        # get disk usage
        disk_usage = psutil.disk_usage("/")
        vm_stats["disk"] = disk_usage.percent
        ## get load average
        vm_stats["load_avg"] = psutil.cpu_percent(interval=1, percpu=True)
        return vm_stats


bot_server = BotServer(
    {"user": "admin", "password": "admin", "port": 6000, "dbname": "shoonya"}
)


@app.route("/shoonya/", methods=["GET"])
def home():
    """Home page"""
    endpoints = [
        {
            "route": "/api/v1/shoonya/refresh",
            "method": "GET",
            "description": "Refresh instances",
        },
        {
            "route": "/api/v1/shoonya/pnl",
            "method": "GET",
            "description": "Get PnL for all instances",
        },
        {
            "route": "/api/v1/shoonya/vmstats",
            "method": "GET",
            "description": "Get VM stats",
        },
        {
            "route": "/api/v1/shoonya/errors",
            "method": "GET",
            "description": "Get errors from the log file",
        },
        {
            "route": "/api/v1/shoonya/kill",
            "method": "GET",
            "description": "Kill all instances",
        },
    ]
    return render_template("index.html", endpoints=endpoints)


@app.route("/api/v1/shoonya/signin", methods=["POST"])
def signin():
    """Sign in with username and password"""
    data = request.get_json()
    username = data.get("username")
    password = data.get("password")
    if not username or not password:
        return jsonify({"msg": "Missing username or password"}), 400
    if username in users and check_password_hash(users[username], password):
        access_token = create_access_token(identity=username)
        return jsonify(access_token=access_token), 200
    return jsonify({"msg": "Bad username or password"}), 401


@app.route("/api/v1/shoonya/pnl", methods=["GET"])
@jwt_required()
def get_pnl():
    """Get PnL for all instances"""
    return jsonify(bot_server.get_pnl()), 200


@app.route("/api/v1/shoonya/vmstats", methods=["GET"])
@jwt_required()
def get_vm_stats():
    """Get VM stats"""
    return jsonify(bot_server.vm_stats()), 200


@app.route("/api/v1/shoonya/errors", methods=["GET"])
@jwt_required()
def get_errors():
    """Get errors from the log file"""
    return jsonify(bot_server.get_errors()), 200


@app.route("/api/v1/shoonya/kill", methods=["GET"])
@jwt_required()
def kill_bot():
    """Kill all instances"""
    if bot_server.kill_bot():
        return jsonify({"message": "All instances killed"}), 200
    return jsonify({"message": "Failed to kill instances"}), 200


@app.route("/api/v1/shoonya/refresh", methods=["GET"])
@jwt_required()
def refresh_pid():
    """Refresh pids"""
    if bot_server.update_pids():
        return jsonify({"message": "Pids updated"}), 200
    return jsonify({"message": "No instances running"}), 200


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=5000)
