RATE = 0.18


class TaxCalculator:
    def compute(self, amount):
        return round_money(amount * RATE)


def round_money(x):
    return round(x, 2)
