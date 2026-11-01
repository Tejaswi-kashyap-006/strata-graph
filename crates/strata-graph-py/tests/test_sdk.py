"""Tests for the strata_graph Python package, run against an installed wheel."""

import shutil
from pathlib import Path

import pytest
import strata_graph

FIXTURE = (
    Path(__file__).resolve().parents[2] / "strata-graph-core" / "tests" / "fixtures" / "py_shop"
)


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    dst = tmp_path / "py_shop"
    shutil.copytree(FIXTURE, dst, ignore=shutil.ignore_patterns(".strata"))
    return dst


def test_open_builds_index(repo: Path) -> None:
    g = strata_graph.open(str(repo))
    assert (repo / ".strata" / "index.db").exists()
    assert Path(g.root) == repo.resolve()
    stats = g.stats()
    assert stats["files"] >= 10
    assert stats["calls"]["resolved_t1"] > 0


def test_queries(repo: Path) -> None:
    g = strata_graph.open(str(repo))
    callers = [h["qname"] for h in g.callers("TaxCalculator.compute", min_tier="t1")]
    assert callers == ["shop.api.quote", "shop.billing.invoice.Invoice.total"]

    hit = g.callers("TaxCalculator.compute")[0]
    assert {"qname", "kind", "file", "line", "depth", "via", "tier", "at"} <= hit.keys()
    assert hit["tier"] == "T1"

    assert g.path("report.main", "round_money") == [
        "report.main",
        "shop.api.checkout",
        "shop.billing.invoice.Invoice.total",
        "shop.billing.tax.TaxCalculator.compute",
        "shop.billing.tax.round_money",
    ]
    assert g.path("round_money", "report.main") is None

    impacted = {h["qname"] for h in g.impact("round_money")}
    assert "shop.api.checkout" in impacted

    assert g.node("Invoice.total")["kind"] == "method"
    assert any(n["qname"] == "shop.billing.invoice.Invoice.total" for n in g.search("invoice tot"))


def test_context_pack(repo: Path) -> None:
    g = strata_graph.open(str(repo))
    pack = g.context("TaxCalculator.compute")
    assert pack.startswith("shop.billing.tax.TaxCalculator.compute  (method,")
    assert "return round_money(amount * RATE)" in pack
    assert "--- called by (2) ---" in pack
    assert len(g.context("shop.billing.invoice.Invoice", max_tokens=64)) < len(
        g.context("shop.billing.invoice.Invoice", max_tokens=4000)
    )


def test_errors(repo: Path) -> None:
    g = strata_graph.open(str(repo))
    with pytest.raises(strata_graph.StrataError, match="no symbol"):
        g.callers("does_not_exist")
    with pytest.raises(strata_graph.StrataError, match="matches"):
        g.node("render")  # HtmlView.render and JsonView.render
    with pytest.raises(strata_graph.StrataError, match="min_tier"):
        g.callers("round_money", min_tier="t9")
    with pytest.raises(strata_graph.StrataError):
        strata_graph.open(str(repo / "missing"))


def test_auto_refresh_follows_edits(repo: Path) -> None:
    g = strata_graph.open(str(repo))
    assert g.stale_files() == 0
    views = repo / "shop" / "views.py"
    views.write_text(views.read_text() + "\n\ndef from_sdk():\n    return show_all(None)\n")
    assert g.stale_files() == 1
    callers = [h["qname"] for h in g.callers("shop.views.show_all")]
    assert "shop.views.from_sdk" in callers
    assert g.stale_files() == 0


def test_manual_refresh(repo: Path) -> None:
    g = strata_graph.open(str(repo), auto_refresh=False)
    (repo / "shop" / "extra.py").write_text("def ping_extra():\n    return 1\n")
    assert g.find("ping_extra") == []
    report = g.refresh()
    assert report["parsed"] == 1
    assert g.find("ping_extra")[0]["qname"] == "shop.extra.ping_extra"


def test_annotate(repo: Path) -> None:
    g = strata_graph.open(str(repo))
    text = g.annotate("shop/billing/tax.py")
    assert text is not None
    assert text.startswith("[strata-graph] shop/billing/tax.py\n")
    assert "TaxCalculator.compute (method): called by 2: shop.api.quote T1" in text
    assert "calls 1: round_money T1" in text

    inverse = g.annotate("shop/billing/tax.py", density="inverse")
    assert inverse is not None and "calls 1:" not in inverse

    part = g.annotate("shop/billing/tax.py", start=5, end=6)
    assert part is not None and part.startswith("[strata-graph] shop/billing/tax.py:5-6\n")
    assert g.annotate("shop/billing/tax.py", start=9000, end=9001) is None

    with pytest.raises(strata_graph.StrataError):
        g.annotate("shop/billing/tax.py", density="dense")
    with pytest.raises(strata_graph.StrataError):
        g.annotate("not/indexed.py")


def test_shape(repo: Path) -> None:
    shape = strata_graph.open(str(repo)).shape()
    assert shape["auto_density"] in {"full", "capped", "inverse"}
    assert shape["shape"]["avg_out_degree"] > 0
    assert {"hub_min_callers", "hubs", "hub_call_share"} <= shape["shape"].keys()
