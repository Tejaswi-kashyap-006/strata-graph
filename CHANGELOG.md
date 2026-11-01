# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## Unreleased

### Added
- Localization eval harness (`eval/`): SWE-bench Verified tasks with gold files and functions from the real fix, a sandboxed agent on the OpenAI API, arms A–D through the shipped hook and MCP server, a resumable runner with a spending cap, paired bootstrap scoring and a leak audit.
- Delivery density: `full`, `capped`, `inverse` and `auto` (picked from the repo's call out-degree). Calls to hubs are counted, not named; test callers and subclasses are counted after real code. `strata-graph annotate`, `Graph.annotate` and `Graph.shape` in Python, `--density` on the hook, and a `delivery` section in `stats`.
- TypeScript interfaces are their own node kind (`interface`), so delivery says "implemented by" and classes say "constructed by".
- The index is stamped with fingerprints of the parser and resolver sources: an index written by a different build is re-parsed or re-resolved automatically, with no revision number to bump by hand. `needs_update()` reports it.
- MCP server (`strata-graph mcp`, stdio) with seven read-only tools. Supports MCP 2024-11-05 to 2025-11-25 and the stateless 2026-07-28 revision. Answers refresh changed files first.
- Context packs: one symbol's source, class, bases and subclasses, callers and callees in a token budget (`context` in the CLI, `Graph.context` in Python, `strata_context` over MCP).
- Agent hooks (`strata-graph hook`, `strata-graph install-hooks --agent claude|codex`): graph facts appended to file reads and code searches in Claude Code (Read, Grep, Bash) and Codex (shell commands). Facts are sent once per session, trimmed to a budget, and never block a tool call.
- `annotate` module in the core crate: per-line facts for a file range or a searched name, for any embedder.
- Multi-hop results print as a tree: each hit sits under the hit that reached it (`Hit.from`, `tree_order`).
- Standards enforced in CI: rustdoc with warnings as errors, `cargo audit`, ruff, `mypy --strict`.
- `CONTRIBUTING.md` with the review rules for resolution changes.
- Type stubs for the native Python module.

### Fixed
- Queries (hooks, MCP, CLI, Python) served a graph resolved by an older engine until a file changed. They now re-resolve first.
- `install-hooks` rewrote an agent settings file with its keys sorted; it now keeps their order.
- `watch` ignored TypeScript/JavaScript files and tsconfig/package.json changes; it now reacts to every indexed language and to import settings.
- A second process updating the index while a long update ran (over 5 s, as on large repos) failed with "database is locked". Writers now wait up to 30 s and take the write lock up front.

### Changed
- Python 3.10 is now the minimum (3.9 reached end-of-life in October 2025).
- MSRV corrected to Rust 1.90. The earlier 1.82 was never tested, and the dependencies (tree-sitter-language, rusqlite) already need 1.90. A CI job now builds on 1.90.
- `resolve` split into `resolve::modules` (module names and import specifiers) and `resolve::text`. tsconfig and package.json loading moved to `config`.
- The core crate forbids `unsafe` code and documents every public item.

## 0.1.0-alpha.0 (2026-11-01, not yet published)

### Added
- Python indexing with tiered call resolution (T1, T0, T0-ambiguous): scopes, imports and re-exports, `self`/`super` through bases, annotations, return types, inferred returns, pytest fixtures.
- TypeScript and JavaScript (TS, TSX, JSX): relative imports, `index` files, tsconfig `paths`/`baseUrl`, workspace packages via package.json `exports`, default and namespace imports, `this`/`super`/`new`, JSX components.
- Incremental SQLite index (`.strata/index.db`) with per-file fact caching, FTS5 name search, and automatic re-resolution when the engine or import settings change.
- CLI: `index`, `watch`, `stats`, `find`, `search`, `callers`, `callees`, `impact`, `path`.
- Python SDK (`strata_graph`) with auto-refresh before queries.
- Gate reports in `docs/gates/`.
