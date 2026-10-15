# Relationships:
# - `app/session/service.py`: Starts the session service
# - `external:operator` (inbound): Starts the application from a terminal

"""Entry point."""

from app.session import SessionService


def main():
    service = SessionService()
    service.login("ada")
