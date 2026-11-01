def test_total(invoice):
    # `invoice` is the conftest fixture: builder.build() -> Invoice | None
    invoice.total()


def test_reset(builder):
    builder.reset()
