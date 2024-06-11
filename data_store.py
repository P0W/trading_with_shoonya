"""Module to handle data storage in Redis"""
import logging

import redis


class DataStore:
    """Class to handle data storage in Redis"""

    ## pylint: disable=too-many-arguments
    def __init__(
        self, instance_id=None, logger=None
    ):
        """
        Initialize the DataStore with Redis connection parameters and instance ID.

        :param host: Redis server hostname.
        :param port: Redis server port.
        :param db: Redis database number.
        :param instance_id: Unique identifier for the instance.
        :param logger: Custom logger instance.
        """
        self.instance_id = instance_id
        self.logger = logger if logger else logging.getLogger(__name__)
        try:
            self.r = redis.Redis()
            self.r.ping()  # Test the connection
        except redis.ConnectionError as e:
            self.logger.error("Failed to connect to Redis: %s", e)
            raise

    def __enter__(self):
        """
        Return the instance of the class to be used in a with statement.
        """
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        """
        Close the connection to Redis.
        """
        try:
            if self.r:
                self.r.close()
        except redis.RedisError as e:
            self.logger.error("Failed to close Redis connection: %s", e)

    def _get_cache_key(self, key, instance_id=None):
        """
        Get the cache key to use for the Redis instance.

        :param key: Key to use in the cache.
        :return: The cache key to use.
        """
        return f"{instance_id}_{key}" if instance_id else f"{self.instance_id}_{key}"


    def set_param(self, key, value, instance_id=None):
        """
        Add a key-value pair to Redis.

        :param key: Key to add.
        :param value: Value to associate with the key.
        """
        try:
            self.r.set(self._get_cache_key(key, instance_id), value)
        except redis.RedisError as e:
            self.logger.error("Failed to set key %s in Redis: %s", key, e)
            raise

    def retrieve_param(self, key, instance_id=None, cast_to=float):
        """
        Retrieve a value from Redis using the key.

        :param key: Key to retrieve.
        :param cast_to: Type to cast the retrieved value to.
        :return: The value cast to the specified type or None if the key does not exist.
        """
        try:
            value = self.r.get(self._get_cache_key(key, instance_id))
            if value is None:
                return None
            value = value.decode("utf-8")  # Decode the byte value to string
            return cast_to(value)
        except (redis.RedisError, ValueError) as e:
            self.logger.error("Failed to get or cast key %s in Redis: %s", key, e)
            raise

    def get_keys(self, instance_id=None):
        """
        Get all keys stored in Redis.

        :return: List of keys stored in Redis.
        """
        try:
            keys = self.r.keys(f"{instance_id}*")
            return [key.decode("utf-8") for key in keys]
        except redis.RedisError as e:
            self.logger.error("Failed to get keys in Redis: %s", e)
            raise