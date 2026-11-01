# strata-graph localization eval

Does strata-graph help a coding agent find the code to change? This harness runs the **same agent, model and prompt** on real SWE-bench Verified issues in up to four arms, and measures whether it finds the right files and functions, at what token cost and how consistently.

| Arm | What the agent gets |
| --- | --- |
| A | `search`, `read_file`, `list_dir` (the grep-only baseline) |
| B | A + strata-graph's MCP tools, which it may or may not call |
| C | A + strata-graph facts appended to search and read results (the hooks) |
| D | B and C |

The agent is a small tool-calling loop on the OpenAI API (`strata_eval/agent.py`), not Codex: it is the same in every arm, so any difference comes from strata-graph. Arm C runs the real `strata-graph hook` binary with the same events Claude Code sends; arm B talks to a real `strata-graph mcp` server.

**What it measures:** localization only. The agent never edits or runs code, so no Docker or test suites are needed. Gold locations come from each task's real fix: the existing files it modifies, and the classes, functions and methods enclosing its changed lines (found with Python's `ast`, independent of strata-graph). Workspaces are exported with `git archive`, so they contain no history the fix could be read from.

## Setup (Windows: use WSL2)

Everything has been tested on Linux. A native Windows build has not been tried, so on Windows run it inside WSL2 (Ubuntu).

1. **WSL2** (PowerShell as administrator, then restart): `wsl --install -d Ubuntu`
2. **Tools** (in the Ubuntu terminal):
   ```sh
   sudo apt update && sudo apt install -y build-essential git python3-venv python3-pip curl
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && . "$HOME/.cargo/env"
   ```
3. **Clone inside the Linux file system** (`~/`, not `/mnt/c/...`, which is many times slower):
   ```sh
   cd ~ && git clone https://github.com/Tejaswi-kashyap-006/strata-graph.git && cd strata-graph
   cargo build --release                      # builds target/release/strata-graph (Rust 1.90+)
   python3 -m venv .venv && . .venv/bin/activate
   pip install -e "eval[dev]"
   (cd eval && python -m pytest -q)            # 18 tests, about 2 s, no API calls
   ```
4. **API key** (per terminal session; never put it in a file in the repo):
   ```sh
   export OPENAI_API_KEY=sk-...
   ```

**Disk:** about 15–20 GB free for 125 tasks. Each task gets its own copy of its repo at the base commit plus a strata-graph index (on django: about 75 MB of source and a 55 MB index), and the repos are mirrored once each.

## Run

```sh
cd ~/strata-graph && . .venv/bin/activate
W=~/strata-eval                              # work directory, outside the repo

# 1. Tasks: 125 from SWE-bench Verified, about half multi-file fixes.
#    Clones each repo once, exports each base commit, computes gold, indexes.
python -m strata_eval prepare --work $W --n 125

# 2. Pilot: 5 tasks x arms A and C x 1 seed = 10 runs, to measure cost per run.
python -m strata_eval run --work $W --limit 5 --arms A,C --seeds 1 \
  --model <MODEL> --price-in <USD per 1M input> --price-cached <USD per 1M cached input> \
  --price-out <USD per 1M output> --max-usd 3
python -m strata_eval score --work $W        # look at cost_usd per run

# 3. Full run: 125 tasks x A,C x 2 seeds = 500 runs. Already-finished runs are skipped.
python -m strata_eval run --work $W --arms A,C --seeds 2 \
  --model <MODEL> --price-in ... --price-cached ... --price-out ... --max-usd <BUDGET>
python -m strata_eval score --work $W        # writes $W/report.md and $W/summary.json
python -m strata_eval audit --work $W        # flags runs that quote the real fix
```

- **Prices** are your model's from OpenAI's pricing page, in dollars per million tokens. The harness does not guess them. Cost per run uses them and the token counts the API returns.
- **`--max-usd`** stops starting new runs once that much is spent. Runs already in flight finish, so the total can go over by up to `--concurrency` runs; leave some margin.
- **Reasoning models:** leave out `--temperature`, since some reject it, and use `--reasoning-effort low|medium|high`.
- **Interrupted?** Run the same command again. Finished runs are kept, and runs are grouped so every arm of a task and seed finishes together.
- **`--concurrency`** (default 4): lower it if you hit rate limits. The client retries 429 and 5xx errors itself.
- **Other splits:** `--arms A,B,C,D` with fewer tasks (`--limit`) or seeds gives the four-arm comparison at the same budget.

## Outputs

| File | What it is |
| --- | --- |
| `report.md`, `summary.json` | Per-arm means; paired difference of each arm vs A with a 95% bootstrap interval; flip rate across seeds; tool adoption |
| `results/<run>.json` | One run: answer, finish reason, turns, tool calls, hook characters added, tokens, cost |
| `traces/<run>.jsonl` | The full transcript: the model's text, every tool call and its output |
| `tasks.jsonl`, `excluded.jsonl` | The tasks used, and the ones dropped with the reason |
| `gold/` | The real fixes, kept outside the workspaces for the leak audit |

Send back `report.md`, `summary.json`, `results/`, `excluded.jsonl` and the audit output. Traces help with any question about a specific run.

## Metrics

- `file_hit@k` / `func_hit@k`: at least one gold file or function is in the agent's top k (k = 1, 5).
- `file_all@k` / `func_all@k`: every gold file or function is in the top k.
- Function metrics count only tasks whose fix changes at least one existing function.
- `input_tokens`, `output_tokens`, `turns`, `tool_calls`, `cost_usd` per run; `hook_chars` is what arm C added.
- `used_strata_tools`: share of runs in arms B and D that called a strata-graph tool at least once (adoption).
- Stability: the share of tasks whose `func_hit@5` differs between seeds.

## Limits

- **Contamination:** SWE-bench issues are public, so a model may remember some fixes. Both arms share this, and `audit` flags runs that quote a fix's added lines.
- **Python only:** this release covers Python. TypeScript (SWE-PolyBench) is planned.
- **Not resolve rate:** localization is a necessary step toward a fix, not proof of one. Resolve rate needs each task's tests run in Docker.
