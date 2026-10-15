# Relationships:
# - `app/session/tokens.py`: Signs and verifies session tokens
# - `app/storage/repo.py`: Stores and retrieves session records
# - `external:web-client` (inbound): Submits login and logout requests

"""The session service."""

from app.storage import SessionRepo, cache

from . import tokens


class SessionService:
    def __init__(self):
        self.repo = SessionRepo()

    def login(self, user):
        token = tokens.sign(user)
        self.repo.save(user, token)
        cache.put(user, token)
        return token

    def logout(self, token):
        if tokens.verify(token):
            self.repo.delete(token)
