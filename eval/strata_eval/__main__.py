"""Command line: `python -m strata_eval prepare|run|score|audit ...`."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sys
from pathlib import Path

from .arms import ARMS, parse_arms
from .llm import OpenAIChat, Prices
from .run import load_results, load_rows, load_tasks, plan, prepare, run_all
from .score import report, summarize


def find_binary(given: str | None) -> str:
    candidates = [given, os.environ.get("STRATA_GRAPH_BIN"), shutil.which("strata-graph")]
    repo_build = Path(__file__).resolve().parents[2] / "target" / "release" / "strata-graph"
    candidates += [str(repo_build), str(repo_build) + ".exe"]
    for c in candidates:
        if c and Path(c).exists():
            return c
    sys.exit("strata-graph binary not found: build it (cargo build --release) or pass --binary")


def main(argv: list[str] | None = None) -> None:
    p = argparse.ArgumentParser(prog="strata_eval", description="strata-graph localization eval")
    sub = p.add_subparsers(dest="cmd", required=True)

    pr = sub.add_parser("prepare", help="select tasks, export repos, compute gold locations, index")
    pr.add_argument("--work", type=Path, required=True)
    pr.add_argument("--n", type=int, default=125, help="number of tasks")
    pr.add_argument("--seed", type=int, default=0)
    pr.add_argument("--multi-share", type=float, default=0.5, help="share of multi-file fixes")
    pr.add_argument("--dataset", default="princeton-nlp/SWE-bench_Verified")
    pr.add_argument(
        "--dataset-file", type=Path, help="local .jsonl/.json/.parquet instead of the hub"
    )
    pr.add_argument("--binary")

    ru = sub.add_parser("run", help="run the agent over tasks x arms x seeds")
    ru.add_argument("--work", type=Path, required=True)
    ru.add_argument("--arms", default="A,C", help=f"comma list of {','.join(ARMS)}")
    ru.add_argument("--seeds", type=int, default=2)
    ru.add_argument("--limit", type=int, help="only the first N tasks (for a pilot)")
    ru.add_argument("--model", required=True)
    ru.add_argument("--price-in", type=float, required=True, help="USD per 1M input tokens")
    ru.add_argument(
        "--price-cached", type=float, help="USD per 1M cached input tokens (default: --price-in)"
    )
    ru.add_argument("--price-out", type=float, required=True, help="USD per 1M output tokens")
    ru.add_argument(
        "--max-usd", type=float, required=True, help="stop starting runs once this is spent"
    )
    ru.add_argument("--concurrency", type=int, default=4)
    ru.add_argument("--max-turns", type=int, default=25)
    ru.add_argument("--temperature", type=float, help="omit for models that reject it")
    ru.add_argument("--reasoning-effort", help="for reasoning models, e.g. low|medium|high")
    ru.add_argument("--api-seed", action="store_true", help="also pass the seed number to the API")
    ru.add_argument("--density", default="auto")
    ru.add_argument("--binary")

    sc = sub.add_parser("score", help="write report.md and summary.json")
    sc.add_argument("--work", type=Path, required=True)

    au = sub.add_parser("audit", help="flag runs whose transcript quotes the gold fix")
    au.add_argument("--work", type=Path, required=True)

    a = p.parse_args(argv)
    if a.cmd == "prepare":
        binary = find_binary(a.binary)
        rows = load_rows(a.dataset, a.dataset_file)
        tasks = prepare(a.work, rows, a.n, a.seed, a.multi_share, binary)
        multi = sum(len(t.gold_files) > 1 for t in tasks)
        print(f"prepared {len(tasks)} tasks ({multi} multi-file) in {a.work}")
    elif a.cmd == "run":
        if "OPENAI_API_KEY" not in os.environ:
            sys.exit("set OPENAI_API_KEY first")
        binary = find_binary(a.binary)
        tasks = load_tasks(a.work)[: a.limit] if a.limit else load_tasks(a.work)
        jobs = plan(tasks, parse_arms(a.arms), a.seeds)
        prices = Prices(
            a.price_in, a.price_cached if a.price_cached is not None else a.price_in, a.price_out
        )

        def make_llm(seed: int) -> OpenAIChat:
            return OpenAIChat(
                a.model, a.temperature, seed if a.api_seed else None, a.reasoning_effort
            )

        print(
            f"{len(jobs)} runs planned: {len(tasks)} tasks x arms {a.arms} x {a.seeds} seed(s); "
            f"cap ${a.max_usd:.2f}"
        )
        run_all(
            a.work,
            jobs,
            make_llm,
            a.model,
            binary,
            prices,
            a.max_usd,
            a.concurrency,
            a.max_turns,
            density=a.density,
        )
    elif a.cmd == "score":
        summary = summarize(load_results(a.work), load_tasks(a.work))
        (a.work / "summary.json").write_text(json.dumps(summary, indent=1), encoding="utf-8")
        text = report(summary)
        (a.work / "report.md").write_text(text, encoding="utf-8")
        print(text)
    elif a.cmd == "audit":
        from .audit import audit

        flagged = audit(a.work)
        print(
            f"{len(flagged)} run(s) flagged"
            + ("".join(f"\n  {f}" for f in flagged) if flagged else "")
        )


if __name__ == "__main__":
    main()
