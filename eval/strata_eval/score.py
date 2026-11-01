"""Scoring: localization accuracy, cost, stability, and tool adoption per arm,
with paired confidence intervals against arm A."""

from __future__ import annotations

import random
import statistics
from collections import defaultdict
from dataclasses import dataclass
from typing import Any

from .agent import RunResult
from .tasks import Task

KS = (1, 5)
BOOTSTRAP_SAMPLES = 10_000


def norm_path(p: str) -> str:
    """Compare paths the way a reviewer would: `/` separators, no `./` or
    leading slash, and anything up to a `workspaces/<id>/` prefix dropped."""
    p = p.strip().replace("\\", "/")
    if "/workspaces/" in p:
        p = p.split("/workspaces/", 1)[1].split("/", 1)[-1]
    while p.startswith("./"):
        p = p[2:]
    return p.lstrip("/")


def norm_func(f: str) -> str:
    if "::" not in f:
        return ""
    path, name = f.split("::", 1)
    name = name.strip().removesuffix("()")
    return f"{norm_path(path)}::{name}"


def at_k(predicted: list[str], gold: list[str], k: int) -> tuple[float, float]:
    """(hit, all) at k: whether any / every gold item is in the top k."""
    top = set(predicted[:k])
    hits = [g in top for g in gold]
    return float(any(hits)), float(all(hits))


@dataclass
class RunScore:
    run: RunResult
    metrics: dict[str, float]


def score_run(r: RunResult, task: Task) -> RunScore:
    files = list(dict.fromkeys(norm_path(f) for f in r.files))
    funcs = list(dict.fromkeys(x for x in (norm_func(f) for f in r.functions) if x))
    gold_files = [norm_path(f) for f in task.gold_files]
    gold_funcs = [norm_func(f) for f in task.gold_functions]
    m: dict[str, float] = {}
    for k in KS:
        m[f"file_hit@{k}"], m[f"file_all@{k}"] = at_k(files, gold_files, k)
        if gold_funcs:
            m[f"func_hit@{k}"], m[f"func_all@{k}"] = at_k(funcs, gold_funcs, k)
    m["input_tokens"] = r.usage.get("input", 0)
    m["output_tokens"] = r.usage.get("output", 0)
    m["turns"] = r.turns
    m["tool_calls"] = sum(r.tool_calls.values())
    m["cost_usd"] = r.cost_usd
    m["hook_chars"] = r.hook_chars
    m["used_strata_tools"] = float(any(k.startswith("strata_") for k in r.tool_calls))
    m["submitted"] = float(r.finish in ("submitted", "forced"))
    return RunScore(r, m)


def bootstrap_ci(diffs: list[float], seed: int = 0) -> tuple[float, float, float]:
    """Mean and 95% percentile-bootstrap interval of paired differences."""
    if not diffs:
        return 0.0, 0.0, 0.0
    rng = random.Random(seed)
    n = len(diffs)
    means = sorted(sum(rng.choice(diffs) for _ in range(n)) / n for _ in range(BOOTSTRAP_SAMPLES))
    return (
        sum(diffs) / n,
        means[int(0.025 * BOOTSTRAP_SAMPLES)],
        means[int(0.975 * BOOTSTRAP_SAMPLES) - 1],
    )


def summarize(results: list[RunResult], tasks: list[Task]) -> dict[str, Any]:
    by_id = {t.instance_id: t for t in tasks}
    scored = [score_run(r, by_id[r.instance_id]) for r in results if r.instance_id in by_id]
    arms = sorted({s.run.arm for s in scored})
    out: dict[str, Any] = {
        "runs": len(scored),
        "tasks": len({s.run.instance_id for s in scored}),
        "arms": {},
    }

    # Per arm: mean of every metric over runs where it is defined.
    for arm in arms:
        rows = [s for s in scored if s.run.arm == arm]
        keys = sorted({k for s in rows for k in s.metrics})
        out["arms"][arm] = {
            "runs": len(rows),
            "errors": sum(s.run.finish == "error" for s in rows),
            "means": {
                k: statistics.fmean(s.metrics[k] for s in rows if k in s.metrics) for k in keys
            },
        }

    # Per (arm, task): mean over seeds, for paired comparison and stability.
    cell: dict[tuple[str, str], list[RunScore]] = defaultdict(list)
    for s in scored:
        cell[(s.run.arm, s.run.instance_id)].append(s)

    def task_mean(arm: str, iid: str, key: str) -> float | None:
        vals = [s.metrics[key] for s in cell.get((arm, iid), []) if key in s.metrics]
        return statistics.fmean(vals) if vals else None

    compare = [
        "file_hit@1",
        "file_hit@5",
        "func_hit@1",
        "func_hit@5",
        "func_all@5",
        "input_tokens",
        "turns",
        "cost_usd",
    ]
    out["vs_A"] = {}
    if "A" in arms:
        for arm in arms:
            if arm == "A":
                continue
            out["vs_A"][arm] = {}
            for key in compare:
                diffs = []
                for iid in by_id:
                    a, b = task_mean("A", iid, key), task_mean(arm, iid, key)
                    if a is not None and b is not None:
                        diffs.append(b - a)
                mean, lo, hi = bootstrap_ci(diffs)
                out["vs_A"][arm][key] = {"diff": mean, "ci95": [lo, hi], "paired_tasks": len(diffs)}

    # Stability across seeds: share of tasks whose func_hit@5 differs between
    # seeds, and the spread of the arm's mean across seeds.
    out["stability"] = {}
    for arm in arms:
        flips = 0
        multi = 0
        for iid in by_id:
            vals = {s.metrics.get("func_hit@5") for s in cell.get((arm, iid), [])} - {None}
            if len([s for s in cell.get((arm, iid), [])]) > 1 and vals:
                multi += 1
                flips += len(vals) > 1
        per_seed: dict[int, list[float]] = defaultdict(list)
        for s in scored:
            if s.run.arm == arm and "func_hit@5" in s.metrics:
                per_seed[s.run.seed].append(s.metrics["func_hit@5"])
        seed_means = [statistics.fmean(v) for v in per_seed.values() if v]
        out["stability"][arm] = {
            "tasks_with_repeats": multi,
            "func_hit@5_flip_rate": flips / multi if multi else None,
            "func_hit@5_seed_sd": statistics.stdev(seed_means) if len(seed_means) > 1 else None,
        }
    return out


def report(summary: dict[str, Any]) -> str:
    """The summary as Markdown."""
    lines = [
        "# strata-graph localization eval",
        "",
        f"{summary['runs']} runs over {summary['tasks']} tasks.",
        "",
        "Definitions: `hit@k` = at least one gold location in the agent's top k; "
        "`all@k` = every gold location in the top k. Files come from the fix's patch "
        "(existing files only); functions are the "
        "classes, functions and methods enclosing the changed lines, found with Python's `ast`.",
        "",
    ]
    arms = summary["arms"]
    keys = [
        "file_hit@1",
        "file_hit@5",
        "file_all@5",
        "func_hit@1",
        "func_hit@5",
        "func_all@5",
        "input_tokens",
        "output_tokens",
        "turns",
        "tool_calls",
        "cost_usd",
        "hook_chars",
        "used_strata_tools",
        "submitted",
    ]
    lines.append("| metric | " + " | ".join(arms) + " |")
    lines.append("| --- |" + " --- |" * len(arms))
    for k in keys:
        vals = []
        for a in arms:
            v = arms[a]["means"].get(k)
            vals.append("—" if v is None else (f"{v:.3f}" if v < 100 else f"{v:,.0f}"))
        lines.append(f"| {k} | " + " | ".join(vals) + " |")
    lines.append(
        "| runs (errors) | "
        + " | ".join(f"{arms[a]['runs']} ({arms[a]['errors']})" for a in arms)
        + " |"
    )
    if summary["vs_A"]:
        lines += [
            "",
            "## Paired difference vs arm A (per-task means over seeds; 95% bootstrap CI)",
            "",
        ]
        lines.append("| arm | metric | difference | 95% CI | tasks |")
        lines.append("| --- | --- | --- | --- | --- |")
        for arm, rows in summary["vs_A"].items():
            for k, v in rows.items():
                lo, hi = v["ci95"]
                lines.append(
                    f"| {arm} | {k} | {v['diff']:+.3f} | [{lo:+.3f}, {hi:+.3f}] "
                    f"| {v['paired_tasks']} |"
                )
    lines += [
        "",
        "## Stability across seeds",
        "",
        "| arm | tasks repeated | func_hit@5 flip rate | sd of seed means |",
        "| --- | --- | --- | --- |",
    ]
    for arm, v in summary["stability"].items():
        fr = "—" if v["func_hit@5_flip_rate"] is None else f"{v['func_hit@5_flip_rate']:.3f}"
        sd = "—" if v["func_hit@5_seed_sd"] is None else f"{v['func_hit@5_seed_sd']:.3f}"
        lines.append(f"| {arm} | {v['tasks_with_repeats']} | {fr} | {sd} |")
    return "\n".join(lines) + "\n"
