from typing import Optional

from .invoice import Invoice


def make_invoice(items) -> Invoice:
    return Invoice(items)


def maybe_invoice() -> Optional[Invoice]:
    return None


def lazy() -> "Invoice":
    return Invoice([])


class Builder:
    def start(self) -> "Builder":
        return self

    def clone(self) -> "Self":
        return self

    def build(self) -> Invoice | None:
        return None

    def reset(self):
        pass

    def run(self):
        self.build().total()
        b = self.start()
        b.reset()

    def finish(self):
        self.clone().reset()


def use():
    make_invoice([]).total()
    inv = maybe_invoice()
    inv.subtotal()
    lazy().save()


def plain_factory():
    return Builder()


def use_plain():
    plain_factory().reset()
