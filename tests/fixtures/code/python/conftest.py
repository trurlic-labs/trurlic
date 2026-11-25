import pytest


@pytest.fixture
def shelf():
    return make_shelf()


def make_shelf():
    return object()
