# Relationships:
# - `app/storage/drivers/sqlite.py`: Runs statements against the session database

"""Session records."""

from .drivers import sqlite


class SessionRepo:
    def save(self, user, token):
        sqlite.execute("insert", user, token)

    def delete(self, token):
        sqlite.execute("delete", token)
