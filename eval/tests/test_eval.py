"""End to end: every arm against the real strata-graph binary, with a
scripted model; then scoring, budget, resume and the leak audit."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from conftest import ScriptedLLM, calls
from strata_eval.agent import Run
from strata_eval.arms import Arm, parse_arms
from strata_eval.audit import audit
from strata_eval.llm import Prices
from strata_eval.run import load_results, load_tasks, plan, run_all
from strata_eval.score import norm_func, norm_path, score_run, summarize

ANSWER = {
    "files": ["./shop/billing/tax.py", "shop/api.py"],
    "functions": [
        "shop/billing/tax.py::round_money",
        "shop/billing/tax.py::TaxCalculator.compute()",
    ],
}


def script() -> ScriptedLLM:
    return ScriptedLLM(
        [
            calls(("search", {"pattern": r"def round_money"})),
            calls(("read_file", {"path": "shop/billing/tax.py"})),
            calls(("submit", ANSWER)),
        ]
    )


def run_arm(
    work: Path, binary: str, arm: str, llm: ScriptedLLM
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    task = load_tasks(work)[0]
    run = Run(
        llm, task, work / "workspaces" / task.instance_id, Arm(arm), 0, "fake", binary, max_turns=8
    )
    result, trace = run.execute(Prices(1.0, 0.5, 2.0))
    return result.__dict__, trace


def test_arm_a_sees_no_strata_graph(work: Path, binary: str) -> None:
    llm = script()
    result, _ = run_arm(work, binary, "A", llm)
    assert result["finish"] == "submitted" and result["hook_chars"] == 0
    assert not any("[strata-graph]" in str(m.get("content", "")) for msgs in llm.seen for m in msgs)
    assert not any(t.startswith("strata_") for t in llm.tools_offered)
    # Cost: 1000+2000+3000 input of which 0+500+1000 cached, 300 output.
    assert abs(result["cost_usd"] - (4500 * 1.0 + 1500 * 0.5 + 300 * 2.0) / 1e6) < 1e-9


def test_arm_c_appends_real_hook_output(work: Path, binary: str) -> None:
    llm = script()
    result, trace = run_arm(work, binary, "C", llm)
    assert result["finish"] == "submitted" and result["hook_chars"] > 0
    system = [m["content"] for m in llm.seen[0] if m["role"] == "system"]
    assert any(s.startswith("[strata-graph] This repo has a code graph") for s in system)
    search_out = next(t["output"] for t in trace if t.get("tool") == "search")
    assert "[strata-graph] definitions named round_money" in search_out
    read_out = next(t["output"] for t in trace if t.get("tool") == "read_file")
    assert "TaxCalculator.compute (method): called by 2" in read_out


def test_arm_b_offers_and_calls_mcp_tools(work: Path, binary: str) -> None:
    llm = ScriptedLLM(
        [
            calls(("strata_callers", {"symbol": "round_money"})),
            calls(("submit", ANSWER)),
        ]
    )
    result, trace = run_arm(work, binary, "B", llm)
    assert "strata_callers" in llm.tools_offered and llm.tools_offered[-1] == "submit"
    assert result["tool_calls"] == {"strata_callers": 1, "submit": 1}
    out = next(t["output"] for t in trace if t.get("tool") == "strata_callers")
    assert out.startswith("callers of shop.billing.tax.round_money")
    assert result["hook_chars"] == 0
    assert any(
        "Instructions from the strata-graph tool server" in m["content"]
        for m in llm.seen[0]
        if m["role"] == "system"
    )


def test_turn_limit_forces_a_submit_and_errors_are_recorded(work: Path, binary: str) -> None:
    llm = ScriptedLLM(
        [
            calls(("list_dir", {"path": "."})),
            calls(("list_dir", {"path": "."})),
            calls(("submit", ANSWER)),
        ]
    )
    task = load_tasks(work)[0]
    result, _ = Run(
        llm, task, work / "workspaces" / task.instance_id, Arm("A"), 0, "fake", binary, max_turns=3
    ).execute()
    assert result.finish == "forced"
    assert llm.choices[-1] == {"type": "function", "function": {"name": "submit"}}

    bad = ScriptedLLM(
        [calls(("read_file", {"path": "../../etc/passwd"})), calls(("submit", ANSWER))]
    )
    result, trace = Run(
        bad, task, work / "workspaces" / task.instance_id, Arm("A"), 0, "fake", binary, 5
    ).execute()
    assert result.finish == "submitted"
    assert next(t["output"] for t in trace if t.get("tool") == "read_file").startswith("error:")

    def boom(_m: object, _t: object) -> str:
        raise RuntimeError("API down")

    result, _ = Run(
        ScriptedLLM([boom]),
        task,
        work / "workspaces" / task.instance_id,
        Arm("A"),
        0,
        "fake",
        binary,
        5,
    ).execute()
    assert result.finish == "error" and "API down" in result.error


def test_scoring_normalizes_and_counts(work: Path, binary: str) -> None:
    task = load_tasks(work)[0]
    result, _ = Run(
        script(), task, work / "workspaces" / task.instance_id, Arm("A"), 0, "fake", binary, 8
    ).execute()
    m = score_run(result, task).metrics
    assert m["file_hit@1"] == 1.0 and m["func_hit@1"] == 1.0 and m["func_all@5"] == 1.0
    assert norm_path(".\\shop\\x.py") == "shop/x.py"
    assert norm_path("/home/u/work/workspaces/django__django-1/django/db/x.py") == "django/db/x.py"
    assert norm_func("a.py::f()") == "a.py::f" and norm_func("no separator") == ""


def test_runner_resumes_respects_budget_and_scores(work: Path, binary: str) -> None:
    tasks = load_tasks(work)
    jobs = plan(tasks, parse_arms("A,C"), seeds=2)
    assert [(a.name, s) for _, a, s in jobs] == [("A", 0), ("C", 0), ("A", 1), ("C", 1)]
    logs: list[str] = []
    prices = Prices(1.0, 0.5, 2.0)
    # Each run costs ~$0.0058; a $0.001 cap allows exactly the first one.
    run_all(work, jobs, lambda s: script(), "fake", binary, prices, 0.001, 1, 8, log=logs.append)
    assert len(load_results(work)) == 1
    assert any("budget reached" in line for line in logs)
    # Raising the cap resumes without redoing finished runs.
    run_all(work, jobs, lambda s: script(), "fake", binary, prices, 10.0, 2, 8, log=logs.append)
    results = load_results(work)
    assert len(results) == 4 and any("1 run(s) already done" in line for line in logs)
    summary = summarize(results, tasks)
    assert summary["arms"]["C"]["means"]["func_hit@5"] == 1.0
    assert summary["vs_A"]["C"]["func_hit@5"]["diff"] == 0.0
    assert summary["arms"]["C"]["means"]["hook_chars"] > 0
    assert summary["stability"]["A"]["func_hit@5_flip_rate"] == 0.0
    assert all((work / "traces" / f"{r.run_id}.jsonl").exists() for r in results)


def test_audit_flags_quoted_fixes(work: Path, binary: str) -> None:
    task = load_tasks(work)[0]
    leaky = ScriptedLLM(
        [
            # Whitespace differs from the patch; still a verbatim quote.
            lambda _m, _t: (
                "Change it to:  return float(Decimal(str(x))"
                '.quantize(Decimal("0.01"),  rounding=ROUND_HALF_UP))'
            ),
            calls(("submit", ANSWER)),
        ]
    )
    for llm, seed in ((script(), 0), (leaky, 1)):
        result, trace = Run(
            llm, task, work / "workspaces" / task.instance_id, Arm("A"), seed, "fake", binary, 8
        ).execute()
        (work / "traces").mkdir(exist_ok=True)
        (work / "traces" / f"{result.run_id}.jsonl").write_text(
            "".join(json.dumps(t) + "\n" for t in trace), encoding="utf-8"
        )
    flagged = audit(work)
    assert flagged == ["shop__shop-1__A__s1: quotes 1 added line(s)"]
