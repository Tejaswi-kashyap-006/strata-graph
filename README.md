# strata-graph

A code graph for coding agents. strata-graph parses a Python, TypeScript or JavaScript repository into a graph of modules, classes, functions and the calls, imports and inheritance between them. Every edge carries a confidence tier, so an agent can see who calls what, and how sure that answer is, without reading files to find out.

> **Status: week 3 of a 6-week MVP.** Python, TypeScript and JavaScript indexing, tiered call resolution, an incremental SQLite index, a CLI, a Python SDK, an MCP server and Claude Code / Codex hooks work today. Next: adaptive delivery and the public eval against grep-only agents. Reports: [week 1](docs/gates/week-1-hand-check.md), [week 2](docs/gates/week-2-fresh-and-installable.md), [week 3: TypeScript](docs/gates/week-3-typescript.md), [week 3: delivery](docs/gates/week-3-delivery.md).

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
L481-498 slugify (function): called by 2: django.template.defaultfilters.slugify T1, utils_tests.test_text.TestUtilsText.test_slugify T1
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
- The hook never edits files and never fails a tool call. If anything goes wrong it adds nothing and logs to stderr.

On django an annotation takes about 0.2 s, on hono about 20 ms.

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
