"""
    Shoonaya API Client with login caching
"""
import logging
import pyotp
import redis
import yaml

from NorenRestApiPy.NorenApi import NorenApi


class ShoonyaApiPy(NorenApi):
    """
    Shoonya API Initializer
    """

    def __init__(self, force_login=False):
        NorenApi.__init__(
            self,
            host="https://api.shoonya.com/NorenWClientTP/",
            websocket="wss://api.shoonya.com/NorenWSTP/",
        )
        self._login(force_login)

    def _login(self, force=False):
        """
        Login to the Shoonya API
        """
        ACCESS_TOKEN_KEY = "access_token_shoonya"  ## pylint: disable=invalid-name
        try:
            redis_client = redis.Redis()
            access_token = redis_client.get(ACCESS_TOKEN_KEY)
            if access_token and not force:
                access_token = access_token.decode("utf-8")
                with open("cred.yml", encoding="utf-8") as f:
                    cred = yaml.load(f, Loader=yaml.FullLoader)
                    self.set_session(cred["user"], cred["pwd"], access_token)
                logging.debug("Access token found in cache, logging in")
            else:
                raise ValueError("No access token found")
        except Exception as ex:  ## pylint: disable=broad-except
            logging.debug("No access token found in cache, logging in: %s", ex)
            with open("cred.yml", encoding="utf-8") as f:
                cred = yaml.load(f, Loader=yaml.FullLoader)

                ret = self.login(
                    userid=cred["user"],
                    password=cred["pwd"],
                    twoFA=pyotp.TOTP(cred["totp_pin"]).now(),
                    vendor_code=cred["vc"],
                    api_secret=cred["apikey"],
                    imei=cred["imei"],
                )
                susertoken = ret["susertoken"]
                try:
                    redis_client.set(
                        ACCESS_TOKEN_KEY, susertoken, ex=2 * 60 * 60
                    )  # 2 hours expiry
                except Exception:  ## pylint: disable=broad-except
                    pass
