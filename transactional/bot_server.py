"""Bot Server class to get PnL, VM stats and kill bot instances"""

import datetime
import logging
import os
import platform
import signal
import sys
import time
from contextlib import contextmanager
from typing import Dict
from typing import Tuple

import psutil
import psycopg2.extras
import yaml
from data_store import DataStore
from flask import abort
from flask import Flask
from flask import jsonify
from flask import render_template
from flask import request
from flask import Response
from flask import stream_with_context
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
app.config["JWT_SECRET_KEY"] = f"shoonya_bot_{yml_config['apikey']}"
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
        self.instances = self.update_pids()
        self.redis_store = DataStore()
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
            return self.instances
        return None

    def get_log_file(self):
        """Get log file for today"""
        today = datetime.datetime.now().strftime("%Y%m%d")
        for file in os.listdir("logs"):
            if (
                file.endswith(today)
                and file.startswith("shoonya_transaction")
                and "log" in file
            ):
                file_name = file
                break
        return file_name

    def get_errors(self):
        """Get errors from the log file"""
        file_name = self.get_log_file()
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

    def kill_bot(self, instance_id=None):
        """Kill all instances"""
        try:
            if not instance_id:
                for pid in self.pids:
                    if os.name == "posix":
                        os.kill(pid, signal.SIGKILL)  ## pylint: disable=no-member
                    else:
                        os.kill(pid, signal.SIGTERM)
                self.pids = []
                self.instances = []
                return True
            for pid in self.pids:
                if instance_id in f"shoonya_{pid}":
                    if os.name == "posix":
                        os.kill(pid, signal.SIGKILL)  ## pylint: disable=no-member
                    else:
                        os.kill(pid, signal.SIGTERM)
                    self.pids.remove(pid)
                    self.instances.remove(instance_id)
                    return True
            return False
        except Exception as e:  ## pylint: disable=broad-exception-caught
            self.logger.error("Failed to kill instances %s", e)
            return False

    def vm_stats(self):
        """Get VM stats in an OS-independent and optimized manner.

        Returns:
            dict: A dictionary containing various VM statistics such as CPU,
                  memory, disk usage, load average (if applicable),
                  swap memory, network I/O statistics, and system boot time.
        """
        vm_stats = {}
        try:
            # CPU and memory usage
            vm_stats["cpu"] = psutil.cpu_percent()
            vm_stats["memory"] = psutil.virtual_memory().percent

            # Disk usage
            disk_usage = psutil.disk_usage("/")
            vm_stats["disk"] = disk_usage.percent

            # Load average (if applicable)
            if (
                platform.system() != "Windows"
            ):  # Load average is not available on Windows
                vm_stats["load_avg"] = os.getloadavg()  ## pylint: disable=no-member
            else:
                vm_stats["load_avg"] = "N/A"  # Placeholder for unsupported OS

            # Swap memory
            swap_memory = psutil.swap_memory()
            vm_stats["swap"] = swap_memory.percent

            # Network I/O statistics
            net_io = psutil.net_io_counters()
            vm_stats["net_sent"] = net_io.bytes_sent
            vm_stats["net_recv"] = net_io.bytes_recv

            # System boot time
            boot_time = datetime.datetime.fromtimestamp(psutil.boot_time())
            vm_stats["boot_time"] = boot_time.strftime("%Y-%m-%d %H:%M:%S")
        except Exception as e:  ## pylint: disable=broad-exception-caught
            logging.error("Failed to gather system statistics: %s", e)

        return vm_stats

    def stream_logs(self, file_name):
        """A generator function to stream logs."""
        try:
            file_size = os.path.getsize(file_name)
            while True:
                with open(file_name, "r", encoding="utf-8") as f:
                    # Check if the file has been updated
                    new_size = os.path.getsize(file_name)
                    if new_size > file_size:
                        f.seek(file_size)
                        log_data = f.read()
                        file_size = new_size
                        yield log_data
                    else:
                        yield ""
                time.sleep(1)  # Sleep for a bit before checking for new logs
        except GeneratorExit:
            # Handle client disconnection
            self.logger.info("Client disconnected, stopping log stream.")

    def modify_target(self, target, instance_id):
        """Modify target for an instance"""
        try:
            self.redis_store.set_param("target_mtm", target, instance_id)
            return True
        except Exception as e:  ## pylint: disable=broad-exception-caught
            self.logger.error("Failed to modify target %s", e)
            return False


bot_server = BotServer(
    {"user": "admin", "password": "admin", "port": 6000, "dbname": "shoonya"}
)


@app.route("/shoonya/<path:filename>")
def dynamic_html(filename):
    """Dynamically serve HTML files."""
    # Ensure the file requested is an HTML file
    if not filename.endswith(".html"):
        abort(404)  # Not Found

    try:
        # Attempt to render the template, assuming it exists in the 'templates' directory
        return render_template(filename)
    except Exception: ## pylint: disable=broad-exception-caught
        # Log the error or handle it as needed
        abort(404)  # Not Found if the template does not exist


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
            "method": "POST",
            "description": "Kill an instances",
        },
        {
            "route": "/api/v1/shoonya/target",
            "method": "POST",
            "description": "Modify target for an instance",
        },
        {
            "route": "/api/v1/shoonya/logs",
            "method": "GET",
            "description": "Stream logs",
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


def validate_parameters(data, required_params, param_types):
    """Validate required parameters and their types in the provided data."""
    if not data:
        raise ValueError("Missing data")

    for param in required_params:
        if param not in data:
            raise ValueError(f"Missing {param}")
        if not isinstance(data[param], param_types[param]):
            expected_type_name = param_types[param].__name__
            raise ValueError(f"Invalid {param}, must be a {expected_type_name}")


@app.route("/api/v1/shoonya/kill", methods=["POST"])
@jwt_required()
def kill_bot():
    """Kill all instances or a specific instance based on the instance_id."""
    try:
        data = request.get_json() or {}
        required_params = []
        param_types = {"instance_id": str}

        validate_parameters(data, required_params, param_types)

        instance_id = data.get("instance_id")
        if instance_id:
            if bot_server.kill_bot(instance_id):
                return jsonify({"message": "Instance killed"}), 200
            return jsonify({"message": "Failed to kill instance"}), 200
        if bot_server.kill_bot():
            return jsonify({"message": "All instances killed"}), 200
        return jsonify({"message": "Failed to kill all instances"}), 200
    except Exception as e:  ## pylint: disable=broad-exception-caught
        return jsonify({"message": f"Failed to kill instances: {e}"}), 500


@app.route("/api/v1/shoonya/refresh", methods=["GET"])
@jwt_required()
def refresh_pid():
    """Refresh pids"""
    instances = bot_server.update_pids()
    if instances:
        ## return instances
        return jsonify({"instances": instances}), 200
    return jsonify({"message": "No instances running"}), 200


@app.route("/api/v1/shoonya/target", methods=["POST"])
@jwt_required()
def modify_target():
    """Modify target for an instance"""
    try:
        data = request.get_json()
        required_params = ["target", "instance_id"]
        param_types = {"target": float, "instance_id": str}

        # Validate parameters
        validate_parameters(data, required_params, param_types)

        # Convert target to float after validation
        target = float(data.get("target"))
        instance_id = data.get("instance_id")

        if bot_server.modify_target(target, instance_id):
            return jsonify({"message": "Target modified"}), 200
        return jsonify({"message": "Failed to modify target"}), 400
    except ValueError as e:
        return jsonify({"message": str(e)}), 400
    except Exception as e:  ## pylint: disable=broad-exception-caught
        return jsonify({"message": f"Failed to modify target: {e}"}), 500


# stream logs
@app.route("/api/v1/shoonya/logs", methods=["GET"])
@jwt_required()
def stream_logs():
    """Stream logs."""
    file_name = bot_server.get_log_file()
    if not file_name:
        return jsonify({"message": "No log file found"}), 404
    return (
        Response(
            stream_with_context(bot_server.stream_logs(file_name)),
            content_type="text/event-stream",
        ),
        200,
    )


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=5000)
