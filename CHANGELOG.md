# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## Unreleased

### Added
- MCP server (`strata-graph mcp`, stdio) with seven read-only tools. Supports MCP 2024-11-05 to 2025-11-25 and the stateless 2026-07-28 revision. Answers refresh changed files first.
- Context packs: one symbol's source, class, bases and subclasses, callers and callees in a token budget (`context` in the CLI, `Graph.context` in Python, `strata_context` over MCP).
- Multi-hop results print as a tree: each hit sits under the hit that reached it (`Hit.from`, `tree_order`).
- Standards enforced in CI: rustdoc with warnings as errors, `cargo audit`, ruff, `mypy --strict`.
- `CONTRIBUTING.md` with the review rules for resolution changes.
- Type stubs for the native Python module.

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
