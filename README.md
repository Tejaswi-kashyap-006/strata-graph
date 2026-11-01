# strata-graph

A code graph for coding agents. strata-graph parses a repository into a graph of modules, classes, functions and the calls, imports and inheritance between them. Every edge carries a confidence tier, so an agent can see who calls what, and how sure that answer is, without reading files to find out.

> **Status: week 1 of a 6-week MVP.** Python indexing, tiered call resolution and a CLI work today. Next: TypeScript, SQLite storage, incremental updates, a Python SDK, an MCP server, and Claude Code / Codex hooks. See [the week-1 gate report](docs/gates/week-1-hand-check.md).

## Why

Grep-first agents find the right file but miss structure: who calls a function, what a change breaks, which override actually runs. Studies in 2026 found agents rarely call an optional graph tool, but improve when structure reaches them inside the results they already read. strata-graph is built to be embedded and delivered in-band, and to be measured against grep-only agents in a public eval.

## Try it

```sh
cargo build --release
./target/release/strata-graph --root path/to/repo index
./target/release/strata-graph --root path/to/repo callers billing.tax.round_money --depth 2
./target/release/strata-graph --root path/to/repo impact Invoice.total
./target/release/strata-graph --root path/to/repo path app.main db.save
```

Add `--json` to any command for machine-readable output. The index is written to `.strata/graph.json` in the repo (add `.strata/` to your `.gitignore`).

## Confidence tiers

| Tier | Meaning |
| --- | --- |
| `T2` | Structural fact (containment) or, later, confirmed by a compiler index (SCIP) |
| `T1` | Resolved by Python's lookup rules: scopes, imports and re-exports, `self` / `cls` / `super()` through resolved bases, and type hints (constructor assignments, annotations, typed parameters) |
| `T0` | The only definition with that name in the repo |
| `T0-ambiguous` | One of 2–3 definitions with that name; right about half the time |

Calls that match more than three definitions, hit a local variable, or use a builtin method name on an untyped value (`d.items()`, `f.read()`) are left unresolved rather than guessed.

## Layout

| Path | What it is |
| --- | --- |
| `crates/strata-graph-core` | Parsing, resolution, graph and queries (the library) |
| `crates/strata-graph-core/queries/python` | tree-sitter query files; adding a language is mostly new query files |
| `crates/strata-graph-core/tests/fixtures` | Fixture repos with hand-checked `expected_edges.txt` |
| `crates/strata-graph` | The `strata-graph` CLI |
| `tools/sample_edges.py` | Samples edges per tier for hand-checking on real repos |

## Testing

`cargo test` runs unit tests and the golden-edge tests. Each fixture's `expected_edges.txt` was written by reading the fixture code, never by copying tool output. Lines starting with `!` guard false positives found on real repos.

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
