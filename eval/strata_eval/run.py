"""Prepare tasks and run the eval: resumable, parallel, with a spending cap."""

from __future__ import annotations

import json
import subprocess
import threading
import time
from collections.abc import Callable, Iterable
from concurrent.futures import FIRST_COMPLETED, Future, ThreadPoolExecutor, wait
from dataclasses import asdict
from pathlib import Path
from typing import Any

from .agent import Run, RunResult
from .arms import Arm
from .llm import LLM, Prices
from .tasks import Task, export_tree, gold_locations, select


def load_rows(dataset: str | None, dataset_file: Path | None) -> list[dict[str, Any]]:
    """SWE-bench rows from the Hugging Face hub or a local .jsonl/.json/.parquet file."""
    if dataset_file is not None:
        if dataset_file.suffix == ".jsonl":
            return [
                json.loads(line)
                for line in dataset_file.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
        if dataset_file.suffix == ".json":
            data = json.loads(dataset_file.read_text(encoding="utf-8"))
            return list(data)
        from datasets import load_dataset

        return [
            dict(r) for r in load_dataset("parquet", data_files=str(dataset_file), split="train")
        ]
    from datasets import load_dataset

    return [
        dict(r) for r in load_dataset(dataset or "princeton-nlp/SWE-bench_Verified", split="test")
    ]


def prepare(
    work: Path,
    rows: list[dict[str, Any]],
    n: int,
    seed: int,
    multi_share: float,
    binary: str,
    log: Callable[[str], None] = print,
) -> list[Task]:
    """Select tasks, export each repo at its base commit, compute gold
    locations, and index the workspace with strata-graph. Tasks that cannot
    be prepared are written to `excluded.jsonl` with the reason."""
    work.mkdir(parents=True, exist_ok=True)
    picked = select(rows, n, seed, multi_share)
    tasks: list[Task] = []
    excluded: list[dict[str, str]] = []
    for i, row in enumerate(picked, start=1):
        iid = row["instance_id"]
        ws = work / "workspaces" / iid
        try:
            if not (ws / ".export-done").exists():
                export_tree(work / "cache", row["repo"], row["base_commit"], ws)
                (ws / ".export-done").write_text("ok\n", encoding="utf-8")
            files, functions = gold_locations(ws, row["patch"])
            if not files:
                raise ValueError("the fix modifies no existing file")
            # Kept outside the workspace, for the leak audit only.
            gold = work / "gold"
            gold.mkdir(parents=True, exist_ok=True)
            (gold / f"{iid}.diff").write_text(row["patch"], encoding="utf-8")
            (gold / f"{iid}.json").write_text(
                json.dumps({"fix_commit": row.get("commit", "") or ""}), encoding="utf-8"
            )
            t0 = time.monotonic()
            subprocess.run([binary, "--root", str(ws), "index"], check=True, capture_output=True)
            log(
                f"[{i}/{len(picked)}] {iid}: {len(files)} file(s), {len(functions)} function(s), "
                f"indexed in {time.monotonic() - t0:.1f}s"
            )
            tasks.append(
                Task(
                    iid, row["repo"], row["base_commit"], row["problem_statement"], files, functions
                )
            )
        except Exception as e:
            log(f"[{i}/{len(picked)}] {iid}: excluded ({type(e).__name__}: {e})")
            excluded.append({"instance_id": iid, "reason": f"{type(e).__name__}: {e}"})
    (work / "tasks.jsonl").write_text("".join(t.to_json() + "\n" for t in tasks), encoding="utf-8")
    (work / "excluded.jsonl").write_text(
        "".join(json.dumps(x) + "\n" for x in excluded), encoding="utf-8"
    )
    return tasks


def load_tasks(work: Path) -> list[Task]:
    return [
        Task.from_json(line)
        for line in (work / "tasks.jsonl").read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def plan(tasks: Iterable[Task], arms: list[Arm], seeds: int) -> list[tuple[Task, Arm, int]]:
    """Every (task, arm, seed), grouped so all arms of one task and seed run
    together: a run stopped by the budget still leaves paired results."""
    return [(t, a, s) for s in range(seeds) for t in tasks for a in arms]


def load_results(work: Path) -> list[RunResult]:
    out = []
    for p in sorted((work / "results").glob("*.json")):
        out.append(RunResult(**json.loads(p.read_text(encoding="utf-8"))))
    return out


def run_all(
    work: Path,
    jobs: list[tuple[Task, Arm, int]],
    make_llm: Callable[[int], LLM],
    model: str,
    binary: str,
    prices: Prices | None,
    max_usd: float | None,
    concurrency: int,
    max_turns: int,
    hook_max_chars: int = 2000,
    density: str = "auto",
    log: Callable[[str], None] = print,
) -> list[RunResult]:
    """Run every job without a saved result. Stops starting new runs once
    the recorded spend reaches `max_usd` (runs already in flight finish, so
    the total can exceed it by up to `concurrency` runs)."""
    results_dir = work / "results"
    traces_dir = work / "traces"
    results_dir.mkdir(parents=True, exist_ok=True)
    traces_dir.mkdir(parents=True, exist_ok=True)
    done = {r.run_id for r in load_results(work)}
    spent = sum(r.cost_usd for r in load_results(work))
    lock = threading.Lock()
    todo = [j for j in jobs if f"{j[0].instance_id}__{j[1].name}__s{j[2]}" not in done]
    log(
        f"{len(jobs) - len(todo)} run(s) already done, {len(todo)} to go; spent so far ${spent:.2f}"
    )
    finished: list[RunResult] = []

    def one(task: Task, arm: Arm, seed: int) -> RunResult:
        nonlocal spent
        run = Run(
            make_llm(seed),
            task,
            work / "workspaces" / task.instance_id,
            arm,
            seed,
            model,
            binary,
            max_turns,
            hook_max_chars,
            density,
        )
        result, trace = run.execute(prices)
        with lock:
            (traces_dir / f"{result.run_id}.jsonl").write_text(
                "".join(json.dumps(t) + "\n" for t in trace), encoding="utf-8"
            )
            (results_dir / f"{result.run_id}.json").write_text(
                json.dumps(asdict(result), indent=1), encoding="utf-8"
            )
            spent += result.cost_usd
            log(
                f"{result.run_id}: {result.finish}, {result.turns} turns, "
                f"${result.cost_usd:.4f} (total ${spent:.2f})"
                + (f" ERROR {result.error}" if result.error else "")
            )
        return result

    pending: set[Future[RunResult]] = set()
    queue = list(todo)
    with ThreadPoolExecutor(max_workers=max(1, concurrency)) as pool:
        while queue or pending:
            while queue and len(pending) < concurrency:
                with lock:
                    over = max_usd is not None and spent >= max_usd
                if over:
                    log(
                        f"budget reached (${spent:.2f} >= ${max_usd:.2f}); "
                        f"not starting {len(queue)} run(s)"
                    )
                    queue.clear()
                    break
                task, arm, seed = queue.pop(0)
                pending.add(pool.submit(one, task, arm, seed))
            if not pending:
                break
            done_now, pending = wait(pending, return_when=FIRST_COMPLETED)
            finished.extend(f.result() for f in done_now)
    return finished
