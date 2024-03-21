"""
Shoonaya API Client with login caching
"""
import datetime
import logging

import pyotp
import redis
import yaml
from NorenRestApiPy.NorenApi import NorenApi


class ShoonyaApiPy(NorenApi):
    """
    Shoonya API Initializer
    """

    def __init__(self, cred_file="cred.yml", force_login=False):
        self.logger = logging.getLogger(__name__)
        self.redis_client = redis.Redis()
        self.cred_file = cred_file
        self.access_token_key = "access_token_shoonya"
        self.last_login_date_key = "last_login_date_shoonya"
        self.token_expiry = 2 * 60 * 60  # 2 hours expiry
        NorenApi.__init__(
            self,
            host="https://api.shoonya.com/NorenWClientTP/",
            websocket="wss://api.shoonya.com/NorenWSTP/",
        )
        self._login(force_login)

    def _get_credentials(self):
        """
        Load and return credentials from file
        """
        with open(self.cred_file, encoding="utf-8") as f:
            return yaml.load(f, Loader=yaml.FullLoader)

    def _login(self, force=False):
        """
        Login to the Shoonya API. If force is True, force a new login.
        If force is False, use cached access token if available and not expired.
        """
        try:
            access_token = self.redis_client.get(self.access_token_key)
            last_login_date = self.redis_client.get(self.last_login_date_key)
            today = datetime.date.today().isoformat()

            if (
                access_token
                and not force
                and last_login_date
                and last_login_date.decode("utf-8") == today
            ):
                access_token = access_token.decode("utf-8")
                cred = self._get_credentials()
                self.set_session(cred["user"], cred["pwd"], access_token)
                self.logger.debug("Access token found in cache, logging in")
            else:
                raise ValueError(
                    f"No access token found for key {self.access_token_key} or token expired"
                )
        except (redis.exceptions.RedisError, ValueError) as ex:
            self.logger.debug(
                "No access token found in cache or token expired, logging in: %s", ex
            )
            cred = self._get_credentials()

            ret = self.login(
                userid=cred["user"],
                password=cred["pwd"],
                twoFA=pyotp.TOTP(cred["totp_pin"]).now(),
                vendor_code=cred["vc"],
                api_secret=cred["apikey"],
                imei=cred["imei"],
            )
            try:
                susertoken = ret["susertoken"]
                self.redis_client.set(
                    self.access_token_key, susertoken, ex=self.token_expiry
                )
                self.redis_client.set(self.last_login_date_key, today)
            except redis.exceptions.RedisError as redis_error:
                self.logger.error(
                    "Failed to set access token or login date in cache: %s", redis_error
                )
            except Exception as all_ex: ## pylint: disable=broad-exception-caught
                self.logger.error("Failed to login: %s", all_ex)
