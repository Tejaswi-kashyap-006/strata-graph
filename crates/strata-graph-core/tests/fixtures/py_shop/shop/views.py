class HtmlView:
    def render(self):
        return "<p/>"


class JsonView:
    def render(self):
        return "{}"


def show(view):
    return view.render()


def ping(x):
    return ", ".join([x.pong()])


class Registry:
    def set(self, key):
        return key


def show_all(round_money):
    # The parameter shadows shop.billing.tax.round_money.
    return round_money(1)


def touch(evt):
    # A builtin method name on an untyped receiver (e.g. threading.Event).
    evt.set()
