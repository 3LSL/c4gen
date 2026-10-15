# Relationships:
# - `external:sessions-db`: Reads and writes session rows

"""SQLite driver."""

import sqlite3


def execute(statement, *values):
    connection = sqlite3.connect("sessions.db")
    connection.execute(statement, values)
    connection.commit()
