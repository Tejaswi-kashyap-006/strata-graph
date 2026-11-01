from ..core.base import Model
from . import tax


class Invoice(Model):
    def __init__(self, items):
        self.items = items
        self.calc = tax.TaxCalculator()

    def subtotal(self):
        return sum(i.price for i in self.items)

    def total(self):
        return self.subtotal() + self.calc.compute(self.subtotal())

    def save(self):
        super().save()
        self.validate()
