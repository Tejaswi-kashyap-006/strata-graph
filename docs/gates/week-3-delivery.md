# Week 3, part 2: delivery (MCP server and agent hooks)

Goal: agents get graph facts without reading extra files. They get them on request through MCP, and without asking through hooks, inside the results they already read.

## What was built

| Piece | How it was checked |
| --- | --- |
| `strata-graph mcp`: 7 read-only tools, MCP 2024-11-05 to 2025-11-25 and the stateless 2026-07-28 revision | 4 end-to-end tests that spawn the server: handshake, version negotiation and errors, stateless calls, answers after a file edit. Also run with the official MCP Python SDK client (2.3.0), which negotiated 2025-11-25 and listed and called the tools. |
| Context packs (CLI, Python SDK, MCP) | Unit tests for content and budget; Python SDK test |
| `strata-graph hook` for Claude Code (Read, Grep, Bash) and Codex (shell commands) | Payload shapes taken from the official Agent SDK type definitions (`claude-agent-sdk` 0.2.176, `@anthropic-ai/claude-agent-sdk` 0.3.316) and the Codex hooks documentation. 4 end-to-end tests run the binary on those events; unit tests cover the shell reader and the settings merge. |
| `strata-graph install-hooks` | End-to-end test: merges into an existing settings file, is idempotent, refuses to overwrite invalid JSON |

## Measurements (2-core container)

| | django (531k lines) | hono (93k lines) |
| --- | --- | --- |
| Hook annotation after a Read / search | 215–276 ms | 19–23 ms |
| SessionStart, index fresh | 0.26–0.30 s (0.57 s with a cold page cache) | — |
| MCP tool call, warm | about 60 ms | — |
| Annotation size | about 700–770 characters (≈190 tokens) | about 300–400 characters |

All edges in the sampled annotations were T1. Before shipping, the hono sample turned up noise: interfaces and type aliases were listed as having "no resolved callers". That line now names only functions and methods, and a test fails if a class appears there.

## Bugs found and fixed on the way

- `watch` ignored TypeScript/JavaScript and config changes (it only looked at `.py`).
- An index update failed with "database is locked" when another update held the lock for more than 5 s (rusqlite's default wait). A test now holds the lock for 6.5 s. It failed before the fix and passes after.
- The declared MSRV (1.82) was wrong: the dependencies need 1.90. It's corrected, and a CI job now builds on 1.90.

## Not yet verified

- **Inside a live agent session.** The hooks were tested with the exact payloads the official SDK types define, not by running Claude Code or Codex against a repo. That run is the first step of week 4.
- **One slow first call.** The first hook or MCP call right after a release build took 3.5–4.6 s twice. It didn't reproduce, even with a cold page cache (0.57 s). Week-4 latency runs will show whether it is real.
- **CI.** It has not run: GitHub Actions is locked on the account for billing, so every check was run locally before each push. The MSRV job can only run in CI.
- **Interfaces.** TypeScript interfaces share the `Class` node kind. A separate kind is planned, so that delivery and `impact` can treat them differently.
