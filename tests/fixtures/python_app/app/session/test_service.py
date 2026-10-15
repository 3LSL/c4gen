from app.session.service import SessionService
from app.storage import cache


def test_login():
    assert SessionService().login("ada")
    assert cache
