# strata-graph

A code graph for coding agents. strata-graph parses a Python, TypeScript or JavaScript repository into a graph of modules, classes, functions and the calls, imports and inheritance between them. Every edge carries a confidence tier, so an agent can see who calls what, and how sure that answer is, without reading files to find out.

> **Status: week 3 of a 6-week MVP.** Python, TypeScript and JavaScript indexing, tiered call resolution, an incremental SQLite index, a CLI and a Python SDK work today. Next: an MCP server and Claude Code / Codex hooks. Reports: [week 1](docs/gates/week-1-hand-check.md), [week 2](docs/gates/week-2-fresh-and-installable.md), [week 3: TypeScript](docs/gates/week-3-typescript.md).

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
$B --root path/to/repo watch                    # keep the index fresh as files change
```

Add `--json` for machine-readable output. Queries pick up file changes before answering; `--no-refresh` skips that. The index lives in `.strata/index.db` (add `.strata/` to your `.gitignore`).

On django (531k lines, 2 cores) a cold index takes 6.7 s, an update after editing one file 1.2 s, and an update with no changes 0.18 s.

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

`cargo test --workspace --exclude strata-graph-py` runs unit tests, the golden-edge tests and the incremental-index test. Each fixture's `expected_edges.txt` was written by reading the fixture code, never by copying tool output. Lines starting with `!` guard false positives found on real repos.

The Python tests run against an installed wheel: `pytest crates/strata-graph-py/tests`.

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
