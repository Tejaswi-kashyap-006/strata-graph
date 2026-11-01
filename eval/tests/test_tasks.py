"""Gold locations, patch handling and task selection."""

from __future__ import annotations

import io
import tarfile
from pathlib import Path

import pytest
from conftest import FIXTURE, PATCH
from strata_eval.tasks import (
    apply_patch,
    def_ranges,
    gold_locations,
    parse_patch,
    safe_extract,
    select,
)


def test_gold_locations_from_a_real_patch() -> None:
    files, functions = gold_locations(FIXTURE, PATCH)
    assert files == ["shop/billing/tax.py"]
    assert functions == [
        "shop/billing/tax.py::TaxCalculator.compute",
        "shop/billing/tax.py::round_money",
    ]


def test_new_functions_and_new_files_are_not_gold(tmp_path: Path) -> None:
    (tmp_path / "m.py").write_text("def a():\n    return 1\n", encoding="utf-8")
    patch = """--- a/m.py
+++ b/m.py
@@ -1,2 +1,6 @@
 def a():
     return 1
+
+
+def brand_new():
+    return 2
--- /dev/null
+++ b/added.py
@@ -0,0 +1 @@
+x = 1
"""
    files, functions = gold_locations(tmp_path, patch)
    # m.py changed, but only by adding a function that did not exist yet.
    assert files == ["m.py"]
    assert functions == []


def test_nested_functions_and_decorators_credit_the_outer_definition() -> None:
    src = (
        "@dec\ndef outer():\n    def inner():\n        return 1\n    return inner\n\n"
        "class K:\n    @property\n    def p(self):\n        return 2\n"
    )
    ranges = def_ranges(src)
    assert ("outer", 1, 5) in ranges
    assert ("K.p", 8, 10) in ranges
    assert all(name != "outer.inner" and name != "inner" for name, _, _ in ranges)


def test_definitions_inside_if_and_try_blocks_are_found() -> None:
    src = (
        "if TYPE_CHECKING:\n"
        "    class Opts:\n"
        "        headers: dict\n"
        "try:\n"
        "    from fast import go\n"
        "except ImportError:\n"
        "    def go():\n"
        "        return 0\n"
        "class K:\n"
        "    if True:\n"
        "        def m(self):\n"
        "            return 1\n"
    )
    names = {name for name, _, _ in def_ranges(src)}
    assert names == {"Opts", "go", "K", "K.m"}


def test_apply_patch_checks_context_and_handles_zero_context_hunks() -> None:
    old = ["a", "b", "c"]
    fp = parse_patch("--- a/f\n+++ b/f\n@@ -2,0 +3 @@\n+new\n")[0]
    new, removed, added = apply_patch(old, fp)
    assert new == ["a", "b", "new", "c"] and removed == set() and added == {3}
    bad = parse_patch("--- a/f\n+++ b/f\n@@ -1,1 +1,1 @@\n-zzz\n+y\n")[0]
    with pytest.raises(ValueError):
        apply_patch(old, bad)


def test_selection_is_deterministic_and_weighted_to_multi_file() -> None:
    def row(i: int, files: int) -> dict[str, str]:
        diff = "".join(f"--- a/f{j}.py\n+++ b/f{j}.py\n@@ -1 +1 @@\n-a\n+b\n" for j in range(files))
        return {"instance_id": f"r-{i:03d}", "patch": diff}

    rows = [row(i, 2 if i % 5 == 0 else 1) for i in range(100)]  # 20 multi-file
    a = select(rows, 30, seed=1, multi_share=0.5)
    b = select(list(reversed(rows)), 30, seed=1, multi_share=0.5)
    assert [r["instance_id"] for r in a] == [r["instance_id"] for r in b]
    assert sum(len(parse_patch(r["patch"])) >= 2 for r in a) == 15
    assert len(a) == 30


def test_safe_extract_refuses_paths_outside_the_workspace(tmp_path: Path) -> None:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w") as tar:
        data = b"x"
        info = tarfile.TarInfo("../escape.txt")
        info.size = len(data)
        tar.addfile(info, io.BytesIO(data))
    buf.seek(0)
    with tarfile.open(fileobj=buf) as tar, pytest.raises(ValueError):
        safe_extract(tar, tmp_path / "ws")
    assert not (tmp_path / "escape.txt").exists()
