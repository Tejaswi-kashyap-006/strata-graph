from shop import Invoice
from shop.billing import tax as taxes
import shop.core.base


def checkout(items) -> float:
    inv = Invoice(items)
    inv.save()
    return inv.total()


def quote(calc: taxes.TaxCalculator, amount):
    return calc.compute(amount)


def legacy():
    m = shop.core.base.Model()
    m.validate()
    return helper_only_here()


def helper_only_here():
    def inner():
        return 1

    return inner()


def fresh_total(items):
    return Invoice(items).total()
