# Contributing to strata-graph

strata-graph is only useful if agents can trust its edges, so changes are judged on precision first, then coverage, then speed.

## Before you push

Run everything CI runs:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo test --workspace --exclude strata-graph-py
cargo audit

ruff check . && ruff format --check .
mypy --strict tools crates/strata-graph-py/python/strata_graph

maturin build --release -m crates/strata-graph-py/Cargo.toml -o dist
pip install --force-reinstall --no-deps dist/*.whl
pytest crates/strata-graph-py/tests
```

## Rust

- Rust 2021, MSRV 1.82 (`rust-version` in the workspace `Cargo.toml`).
- `#![forbid(unsafe_code)]` in the core and CLI crates. The PyO3 crate is exempt because its generated bindings use `unsafe`.
- `#![warn(missing_docs)]` in the core crate: every public item has a doc comment.
- No `unwrap()`, `expect()` or `panic!` on paths that handle repo content or user input. Return errors with `anyhow` context. The one deliberate exception is compiling the built-in tree-sitter queries, which are constants covered by tests.
- Keep modules to one concern. If a file grows past roughly 800 lines, split it.

## Python

- Python 3.10 or newer (3.9 is end-of-life). Ruff with the config in `ruff.toml`; `mypy --strict` for the SDK and tools.
- The SDK ships type stubs for the native module (`_native.pyi`); keep them in step with `crates/strata-graph-py/src/lib.rs`.

## Changing resolution rules

1. **Add expectations first.** Put the case in a fixture under `crates/strata-graph-core/tests/fixtures/` and the expected edge in its `expected_edges.txt`, written by reading the fixture code, never copied from tool output. Add a `!` line for every false positive you are guarding against.
2. **Never lint or format fixtures.** They contain deliberately odd code that the tests depend on; `ruff.toml` excludes them.
3. **Check on real repos.** Index at least one real repo per affected language. Hand-check a sample with `tools/sample_edges.py`, and use `tools/diff_edges.py` against the previous index to sample only the edges your change added. Record the results in `docs/gates/`.
4. **Bump `RESOLVER_REVISION`** in `crates/strata-graph-core/src/lib.rs`, so existing indexes are re-resolved on upgrade.
5. **Prefer leaving a call unresolved to guessing.** A wrong edge costs an agent more than a missing one.

## Commits

- One logical change per commit, with a message that says what changed and why. Include measured numbers when a change affects resolution or speed.
- Keep `CHANGELOG.md` up to date under "Unreleased".
