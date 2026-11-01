import pytest

from shop.billing.factory import Builder


@pytest.fixture
def builder():
    b = Builder()
    return b


@pytest.fixture
def invoice(builder):
    return builder.build()
