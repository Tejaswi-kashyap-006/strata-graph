# strata-graph

A code graph for coding agents. strata-graph parses a Python, TypeScript or JavaScript repository into a graph of modules, classes, functions and the calls, imports and inheritance between them. Every edge carries a confidence tier, so an agent can see who calls what, and how sure that answer is, without reading files to find out.

> **Status: week 4 of a 6-week MVP.** Python, TypeScript and JavaScript indexing, tiered call resolution, an incremental SQLite index, a CLI, a Python SDK, an MCP server, and Claude Code / Codex hooks with repo-aware delivery density work today. Next: the public eval against grep-only agents. Reports: [week 1](docs/gates/week-1-hand-check.md), [week 2](docs/gates/week-2-fresh-and-installable.md), [week 3: TypeScript](docs/gates/week-3-typescript.md), [week 3: delivery](docs/gates/week-3-delivery.md), [week 4: adaptive delivery](docs/gates/week-4-adaptive-delivery.md).

## Why

Grep-first agents find the right file but miss structure: who calls a function, what a change breaks, which override actually runs. Studies in 2026 found agents rarely call an optional graph tool, but improve when structure reaches them inside the results they already read. strata-graph is built to be embedded and delivered in-band, and to be measured against grep-only agents in a public eval.

## Python

```sh
pip install maturin
maturin build --release -m crates/strata-graph-py/Cargo.toml -o dist
pip install --find-links dist strata-graph
```

```python
import strata_graph

g = strata_graph.open("path/to/repo")          # builds or updates .strata/index.db
g.callers("billing.tax.round_money", depth=2)  # list of dicts: qname, tier, file, line, at, ...
g.impact("Invoice.total")                      # transitive callers and subclasses
g.path("app.main", "db.save")                  # shortest call path, or None
g.search("invoice tot")                        # prefix name search
g.context("Invoice.total", max_tokens=1500)    # text pack: source, class, callers, callees
g.annotate("billing/invoice.py", density="auto")  # in-band facts for one file, as hooks deliver them
```

Each query first checks for changed files (about 70 ms on django) and re-indexes only those, so answers match the code on disk. Pass `auto_refresh=False` to `open` and call `g.refresh()` yourself if you prefer. Queries default to `min_tier="t0"`, which leaves ambiguous edges out.

## CLI

```sh
cargo build --release
B=./target/release/strata-graph
$B --root path/to/repo index                    # incremental: only changed files are parsed
$B --root path/to/repo callers billing.tax.round_money --depth 2
$B --root path/to/repo impact Invoice.total
$B --root path/to/repo path app.main db.save
$B --root path/to/repo search "invoice tot"
$B --root path/to/repo context Invoice.total    # source, class, callers and callees in ~1500 tokens
$B --root path/to/repo annotate billing/invoice.py --lines 10-60   # what a hook adds after reading those lines
$B --root path/to/repo watch                    # keep the index fresh as files change
```

Add `--json` for machine-readable output. Queries pick up file changes before answering; `--no-refresh` skips that. The index lives in `.strata/index.db` (add `.strata/` to your `.gitignore`).

On django (531k lines, 2 cores) a cold index takes 6.7 s, an update after editing one file 1.2 s, and an update with no changes 0.18 s.

## MCP server

`strata-graph mcp` serves the graph to any MCP client over stdio. It offers seven read-only tools: `strata_context`, `strata_callers`, `strata_callees`, `strata_impact`, `strata_path`, `strata_find` and `strata_stats`. Before each answer it re-indexes changed files, so results follow edits made during the session.

```sh
# Claude Code (project scope writes .mcp.json)
claude mcp add --scope project strata-graph -- strata-graph --root . mcp

# Codex
codex mcp add strata-graph -- strata-graph --root . mcp
```

The server speaks MCP 2024-11-05 through 2025-11-25 with the usual `initialize` handshake, and the stateless 2026-07-28 revision (`server/discover`, version in `_meta`). Logs go to stderr. On django, the first call after start takes about 0.3 s and later calls about 60 ms.

## Agent hooks

Agents rarely call an optional tool, so strata-graph also puts graph facts into the results they already read. After a file read or a code search, a hook adds a few lines about the definitions involved:

```text
[strata-graph] django/utils/text.py:470-529
L460-477 unescape_string_literal (function): called by 4: django.contrib.admin.options.ModelAdmin.get_search_results T1, django.template.base.Variable.__init__ T1, 2 in tests
L481-498 slugify (function): called by 2: django.template.defaultfilters.slugify T1, 1 in tests
L501-505 camel_case_to_spaces (function): called by 1: django.db.models.options.Options.contribute_to_class T1
```

```sh
strata-graph install-hooks --agent claude   # adds hooks to .claude/settings.json (--local for settings.local.json)
strata-graph install-hooks --agent codex    # adds hooks to .codex/hooks.json; also set codex_hooks = true in ~/.codex/config.toml
```

- Claude Code: facts follow `Read`, `Grep`, and `Bash` commands that read or search code.
- Codex: hooks see shell commands only, which is how Codex reads code. `rg`, `grep`, `git grep`, `cat`, `nl`, `head`, `tail` and `sed -n` are understood, including pipelines (`nl -ba f.py | sed -n '1,80p'`) and `cd dir && ...`.
- At session start the hook brings the index up to date and explains the tiers once. A tool call never builds a missing index; it adds nothing until one exists.
- Each fact is sent once per session (again after `/clear` or compaction). An annotation is at most 2,000 characters (`--max-chars`); when facts don't fit, the ones with the most relations are kept.
- Code outside tests is named first; test callers and subclasses are counted (`2 in tests`) and named only when nothing else uses a definition. Calls to widely used helpers (hubs: the top 1% by callers, and at least 20) are counted, not named.
- Classes say what constructs them; TypeScript interfaces say what implements them.
- The hook never edits files and never fails a tool call. If anything goes wrong it adds nothing and logs to stderr.

**Density.** `--density` (or `density=` in Python) picks how much is said: `full` (callers and callees), `capped` (at most two callees each), `inverse` (only what depends on a definition), or `auto`. `auto` applies the CodeAnchor heuristic to the repo's average resolved call out-degree: under 5 → full, over 8 → inverse, between → capped. On every repo measured so far the average is 1.7–2.2, so `auto` currently means `full`; the week-5 eval will tune the thresholds. `strata-graph stats` shows a repo's shape and the density `auto` picks.

On django (531k lines) an annotation takes a median of 250 ms (p95 316 ms); on hono, zod and flask under 30 ms.

## Languages

| Language | Files | Import resolution |
| --- | --- | --- |
| Python | `.py` | Packages and `__init__.py`, namespace subpackages, relative imports, re-exports, `import *` |
| TypeScript / JavaScript | `.ts` `.tsx` `.mts` `.cts` `.js` `.jsx` `.mjs` `.cjs` | Relative paths, `index` files, tsconfig/jsconfig `paths` and `baseUrl`, the repo's own packages via package.json `exports`, default and namespace imports, re-exports |

`node_modules/` is always skipped; everything else follows `.gitignore`.

## Confidence tiers

| Tier | Meaning |
| --- | --- |
| `T2` | Structural fact (containment) or, later, confirmed by a compiler index (SCIP) |
| `T1` | Resolved by the language's lookup rules: scopes, imports and re-exports, `self` / `this` / `super` through resolved bases, type hints (annotations, typed parameters, return types, constructor and factory calls, inferred returns), pytest fixtures, JSX components |
| `T0` | The only definition with that name in the repo (Python only; in TypeScript these are labelled ambiguous because hand checks found them unreliable) |
| `T0-ambiguous` | One of 2–3 definitions with that name; right about half the time |

Calls that match more than three definitions, hit an untyped local, use a builtin method name on an untyped value (`d.items()`, `f.read()`), start from a third-party import (`np.zeros(3).render()`), or reach a method inherited from a library base class are left unresolved or marked external rather than guessed.

When strata-graph is upgraded, an existing index is re-resolved automatically; parsed files are reused.

## Eval

`eval/` holds the localization eval: the same OpenAI-backed agent on SWE-bench Verified issues, without strata-graph (arm A), with its MCP tools (B), with its hooks (C) or with both (D). It measures File@k and Func@k, tokens, cost and consistency across seeds, with paired confidence intervals. See [eval/README.md](eval/README.md) for setup, a pilot run and a 500-run plan.

## Layout

| Path | What it is |
| --- | --- |
| `crates/strata-graph-core` | Parsing, resolution, storage and queries (the library) |
| `crates/strata-graph-core/queries/{python,typescript}` | tree-sitter query files; adding a language is mostly new query files |
| `crates/strata-graph-core/tests/fixtures` | Fixture repos with hand-checked `expected_edges.txt` |
| `crates/strata-graph` | The `strata-graph` CLI |
| `crates/strata-graph-py` | The `strata_graph` Python package (PyO3 + maturin) |
| `tools/` | Edge sampling and index-diff scripts used for hand-checking |

## Testing

`cargo test --workspace --exclude strata-graph-py` runs unit tests, the golden-edge tests, the incremental-index tests (including concurrent writers) and the MCP and hook tests (which run the binary with real protocol messages and hook events). Each fixture's `expected_edges.txt` was written by reading the fixture code, never by copying tool output. Lines starting with `!` guard false positives found on real repos.

The Python tests run against an installed wheel: `pytest crates/strata-graph-py/tests`.

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
