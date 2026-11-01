"""The agent's sandboxed tools."""

from __future__ import annotations

import shutil
from pathlib import Path

import pytest
from conftest import FIXTURE
from strata_eval.tools import MAX_SEARCH_HITS, ToolError, Workspace


@pytest.fixture
def ws(tmp_path: Path) -> Workspace:
    root = tmp_path / "repo"
    shutil.copytree(FIXTURE, root)
    (root / ".strata").mkdir(exist_ok=True)
    (root / ".strata" / "secret.txt").write_text("round_money graph data", encoding="utf-8")
    return Workspace(root)


def test_paths_cannot_leave_the_repo_or_see_the_index(ws: Workspace) -> None:
    for bad in ["../", "/etc/passwd", ".strata/secret.txt", "shop/../../x"]:
        with pytest.raises(ToolError):
            ws.resolve(bad)
    assert ".strata" not in ws.list_dir(".")
    assert "secret.txt" not in ws.search("graph data")


def test_read_file_numbers_lines_and_pages(ws: Workspace) -> None:
    text, start, end = ws.read_file("shop/billing/tax.py", 4, 6)
    assert (start, end) == (4, 6)
    assert text.splitlines()[0] == "shop/billing/tax.py (lines 4-6 of 10)"
    assert "     5\t    def compute(self, amount):" in text
    assert "call read_file with start_line=7" in text
    with pytest.raises(ToolError):
        ws.read_file("shop/billing/tax.py", 99)


def test_search_finds_lines_in_path_order_and_caps_output(ws: Workspace, tmp_path: Path) -> None:
    out = ws.search(r"def round_money")
    assert out == "shop/billing/tax.py:9: def round_money(x):"
    assert ws.search("round_money(", glob="*.py").count("\n") >= 1  # invalid regex -> literal
    assert ws.search("nothing-like-this") == "no matches"
    many = tmp_path / "repo" / "many.py"
    many.write_text("\n".join("hit = 1" for _ in range(MAX_SEARCH_HITS + 5)), encoding="utf-8")
    fresh = Workspace(tmp_path / "repo")
    capped = fresh.search("hit = 1", path="many.py")
    assert capped.endswith("narrow the pattern, path or glob")
    assert capped.count("many.py:") == MAX_SEARCH_HITS
