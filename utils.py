"""
Utility functions for the project shoonya trading
"""
import datetime
import logging
import os
import pathlib
import sys
import traceback
import zipfile

import pandas as pd
import requests
import colorlog

from const import INDICES_TOKEN
from const import LOT_SIZE
from const import EXCHANGE


def round_to_point5(x):
    """
    Round to nearest 0.5
    """
    return round(x * 2) / 2


def full_stack():
    """
    Get the full stack trace
    """
    exc = sys.exc_info()[0]
    stack = traceback.extract_stack()[:-1]  # last one would be full_stack()
    if exc is not None:  # i.e. an exception is present
        del stack[-1]  # remove call of full_stack, the printed exception
        # will contain the caught exception caller instead
    trc = "Traceback (most recent call last):\n"
    stackstr = trc + "".join(traceback.format_list(stack))
    if exc is not None:
        stackstr += "  " + traceback.format_exc()
    return stackstr


def validate(index_qty, index_value):
    """
    Validate the quantity
    """
    if index_value not in INDICES_TOKEN:
        logging.error("Invalid index %s", index_value)
        sys.exit(-1)
    if index_qty % LOT_SIZE[index_value] != 0:
        logging.error("Quantity must be multiple of %s", LOT_SIZE[index_value])
        sys.exit(-1)


def configure_logger(log_level, prefix_log_file: str = "shoonya_daily_short"):
    """
    Configure the logger
    """
    # Setup logging
    # create a directory logs if it does not exist
    pathlib.Path.mkdir(pathlib.Path("logs"), exist_ok=True)
    # Create a filename suffixed with current date DDMMYY format with
    # current date inside logs directory
    log_file = pathlib.Path("logs") / (
        f"{prefix_log_file}_{datetime.datetime.now().strftime('%Y%m%d')}.log"
    )

    # Define log colors
    log_colors_config = {
        "DEBUG": "cyan",
        "INFO": "green",
        "WARNING": "yellow",
        "ERROR": "red",
        "CRITICAL": "red",
    }

    # Create a stream handler with color support
    color_stream_handler = colorlog.StreamHandler()
    color_stream_handler.setFormatter(
        colorlog.ColoredFormatter(
            fmt="%(log_color)s%(asctime)s.%(msecs)d %(filename)s:%(lineno)d:%(funcName)s() %(levelname)s %(message)s",
            datefmt="%A,%d/%m/%Y|%H:%M:%S",
            log_colors=log_colors_config,
        )
    )

    # Configure the logger
    logging.basicConfig(
        handlers=[
            color_stream_handler,
            logging.FileHandler(log_file),
        ],
        level=log_level,
    )

    return logging.getLogger(prefix_log_file)


def download_scrip_master(file_id="NFO_symbols"):
    """
    Download the scrip master from the Shoonya endpoint website
    Headers:
        Exchange,Token,LotSize,Symbol,TradingSymbol,Expiry,\
            Instrument,OptionType,StrikePrice,TickSize
    file_id: NFO_symbols, CDS_symbols, BSE_symbols, NSE_symbols,\
        BFO_symbols, MCX_symbols
    """
    today = datetime.datetime.now().strftime("%Y%m%d")
    downloads_folder = "./downloads"
    zip_file_name = f"{downloads_folder}/{file_id}.txt_{today}.zip"
    todays_nse_fo = f"{downloads_folder}/{file_id}.{today}.txt"

    ## unzip and read the file
    ## create a download folder, if not exists
    if not os.path.exists(downloads_folder):
        os.mkdir(downloads_folder)
    if not os.path.exists(todays_nse_fo):
        shoonya_url = f"https://api.shoonya.com/{file_id}.txt.zip"
        logging.info("Downloading file %s", shoonya_url)
        nse_fo = requests.get(shoonya_url, timeout=15)
        if nse_fo.status_code != 200:
            logging.error("Could not download file")
            return None
        with open(zip_file_name, "wb") as f:
            f.write(nse_fo.content)

        ## extract the file in the download folder
        with zipfile.ZipFile(zip_file_name, "r") as zip_ref:
            zip_ref.extractall(downloads_folder)
        ## remove the zip file
        os.remove(zip_file_name)
        ## rename the file with date suffix
        os.rename(f"{downloads_folder}/{file_id}.txt", todays_nse_fo)
    df = pd.read_csv(todays_nse_fo, sep=",")
    return df


def refresh_indices_code():
    """
    Refresh the token
    """
    data_frame = download_scrip_master(file_id="NSE_symbols")
    indices_symbols = [
        ("Nifty 50", "NIFTY"),
        ("Nifty Bank", "BANKNIFTY"),
        ("Nifty Fin Services", "FINNIFTY"),
        ("INDIAVIX", "INDIAVIX"),
        ("NIFTY MID SELECT", "MIDCPNIFTY"),
    ]
    for index_name, index_value in indices_symbols:
        token = data_frame[data_frame["Symbol"] == index_name]["Token"].values[0]
        INDICES_TOKEN[index_value] = token

    ## BSE Futures & Options symbols
    data_frame = download_scrip_master(file_id="CDS_symbols")
    indices_symbols = [
        ("USDINR", "USDINR"),
        ("EURINR", "EURINR"),
        ("GBPINR", "GBPINR"),
        ("JPYINR", "JPYINR"),
    ]
    for index_name, index_value in indices_symbols:
        token = data_frame[data_frame["Symbol"] == index_name]["Token"].values[0]
        INDICES_TOKEN[index_value] = token


def get_index(tradingsymbol):
    """
    Get the index name from the trading symbol
    """
    return tradingsymbol[
        : tradingsymbol.index(next(filter(str.isdigit, tradingsymbol)))
    ]


def get_exchange(tradingsymbol, is_index=False):
    """
    Get the exchange from the trading symbol
    """
    if is_index:
        if tradingsymbol in ["NIFTY", "BANKNIFTY", "FINNIFTY", "MIDCPNIFTY"]:
            return "NSE"
        if tradingsymbol in ["SENSEX"]:
            return "BSE"
        if tradingsymbol in ["USDINR", "EURINR", "GBPINR", "JPYINR"]:
            return "CDS"
    return EXCHANGE[get_index(tradingsymbol)]


refresh_indices_code()
