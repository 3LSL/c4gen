# Relationships:
# - `app/storage/repo.py`: Loads signing keys

"""Token signing."""

import hashlib


def sign(user):
    return hashlib.sha256(user.encode()).hexdigest()


def verify(token):
    return len(token) == 64
