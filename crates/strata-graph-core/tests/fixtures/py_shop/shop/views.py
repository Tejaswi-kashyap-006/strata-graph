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


from collections import OrderedDict


class Cache(OrderedDict):
    def touch(self):
        pass


class Fake:
    def move_to_end(self, key):
        return key


def cache_use(c: Cache):
    # move_to_end is inherited from OrderedDict, outside the repo.
    c.move_to_end("x")
    c.touch()
