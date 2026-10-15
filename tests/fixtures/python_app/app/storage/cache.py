# Relationships:
# None.

"""In-memory token cache."""

_tokens = {}


def put(user, token):
    _tokens[user] = token
