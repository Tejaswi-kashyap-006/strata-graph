"""Shared fixtures: a fake SWE-bench task built from the py_shop fixture repo,
and a scripted stand-in for the model."""

from __future__ import annotations

import json
import shutil
import subprocess
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest
from strata_eval.__main__ import find_binary
from strata_eval.llm import Completion, ToolCall, Usage
from strata_eval.tasks import Task, gold_locations

REPO = Path(__file__).resolve().parents[2]
FIXTURE = REPO / "crates" / "strata-graph-core" / "tests" / "fixtures" / "py_shop"

# The "fix": round_money should round half up, and compute should clamp
# negative amounts. Two functions in one file.
PATCH = """diff --git a/shop/billing/tax.py b/shop/billing/tax.py
--- a/shop/billing/tax.py
+++ b/shop/billing/tax.py
@@ -3,8 +3,12 @@

 class TaxCalculator:
     def compute(self, amount):
-        return round_money(amount * RATE)
+        if amount < 0:
+            amount = 0
+        return round_money(amount * RATE)


 def round_money(x):
-    return round(x, 2)
+    from decimal import ROUND_HALF_UP, Decimal
+    return float(Decimal(str(x)).quantize(Decimal("0.01"), rounding=ROUND_HALF_UP))
"""


@pytest.fixture(scope="session")
def binary() -> str:
    try:
        return find_binary(None)
    except SystemExit:
        pytest.skip("strata-graph binary not built")


@pytest.fixture
def work(tmp_path: Path, binary: str) -> Path:
    """A work directory with one prepared task, as `prepare` would leave it."""
    ws = tmp_path / "work" / "workspaces" / "shop__shop-1"
    shutil.copytree(FIXTURE, ws, ignore=shutil.ignore_patterns(".strata", "expected_edges.txt"))
    files, functions = gold_locations(ws, PATCH)
    task = Task(
        "shop__shop-1", "example/shop", "0" * 40, "Tax totals are off by a cent.", files, functions
    )
    (tmp_path / "work" / "tasks.jsonl").write_text(task.to_json() + "\n", encoding="utf-8")
    gold = tmp_path / "work" / "gold"
    gold.mkdir()
    (gold / "shop__shop-1.diff").write_text(PATCH, encoding="utf-8")
    subprocess.run([binary, "--root", str(ws), "index"], check=True, capture_output=True)
    return tmp_path / "work"


Step = Callable[
    [list[dict[str, Any]], list[dict[str, Any]]], list[tuple[str, dict[str, Any]]] | str
]


class ScriptedLLM:
    """Replays a fixed list of steps. Each step returns tool calls
    [(name, args)] or plain text. Records what it was shown."""

    def __init__(self, steps: list[Step]) -> None:
        self.steps = steps
        self.seen: list[list[dict[str, Any]]] = []
        self.tools_offered: list[str] = []
        self.choices: list[Any] = []

    def complete(
        self, messages: list[dict[str, Any]], tools: list[dict[str, Any]], tool_choice: Any
    ) -> Completion:
        self.seen.append([dict(m) for m in messages])
        self.tools_offered = [t["function"]["name"] for t in tools]
        self.choices.append(tool_choice)
        i = len(self.seen) - 1
        out = self.steps[min(i, len(self.steps) - 1)](messages, tools)
        usage = Usage(input=1000 * (i + 1), cached_input=500 * i, output=100)
        if isinstance(out, str):
            return Completion({"role": "assistant", "content": out}, [], usage)
        calls = [
            ToolCall(f"call_{i}_{j}", name, json.dumps(args)) for j, (name, args) in enumerate(out)
        ]
        msg = {
            "role": "assistant",
            "content": "",
            "tool_calls": [
                {
                    "id": c.id,
                    "type": "function",
                    "function": {"name": c.name, "arguments": c.arguments},
                }
                for c in calls
            ],
        }
        return Completion(msg, calls, usage)


def calls(*items: tuple[str, dict[str, Any]]) -> Step:
    return lambda _m, _t: list(items)
