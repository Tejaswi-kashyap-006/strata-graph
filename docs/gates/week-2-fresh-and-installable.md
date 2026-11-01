# Gate 2: pip install works, graph stays fresh on edit

Status: **passed on Linux.** Checked 2026-11-01. The CI matrix (Linux, macOS, Windows × Python 3.9 and 3.13) runs the same checks once GitHub Actions is enabled on the account.

## pip install

`maturin build` produces one abi3 wheel for every CPython ≥ 3.9. Installed with `pip install` into a fresh virtualenv, it imports and passes all 5 tests in `crates/strata-graph-py/tests` (open and build, queries, errors, auto-refresh after an edit, manual refresh).

The local wheel is tagged `manylinux_2_34`. Wheels for PyPI need to be built in a manylinux container (maturin-action) for older glibc; that belongs to the release workflow in week 4.

## Fresh on edit

`tests/incremental.rs` edits a temp copy of the fixture and checks the index after each step:

| Step | Re-parsed | What must hold |
| --- | --- | --- |
| First build | all 13 files | — |
| No change | 0 | Same graph back from the store |
| Add a caller in `views.py` | 1 | New T1 edge to `round_money` |
| Rename `round_money` in `tax.py` | 1 | Old node gone; `compute` → `round_cash` is T1; the untouched `views.late` no longer points anywhere |
| Rewrite identical bytes | 0 | Detected by hash |
| Delete `scripts/report.py` | 0 (1 removed) | Its nodes gone |

The rename step is the one that matters. Only `tax.py` is re-parsed, but every update re-resolves the whole graph from cached facts, so edges from files that weren't re-parsed follow the change.

`strata-graph watch` and the Python SDK's `auto_refresh` both use the same update.

## Timings on django (531k lines, 2 cores)

| Operation | Time |
| --- | --- |
| Cold index | 6.7 s |
| Update, nothing changed | 0.18 s |
| Update after editing one file | 1.2 s |
| Staleness check (runs before each SDK query) | 68 ms |
| `callers` / `impact` query, including the check | ≈ 60 ms |

The one-file update is dominated by decoding cached facts and rewriting the graph tables, not parsing. If agents need sub-second updates on repos this size, the next step is a binary facts encoding and writing only changed rows.

## Resolution after week 2

Return annotations, unannotated-return inference, pytest fixtures, constructor and factory receivers, and library base classes. Share of non-external calls left unresolved:

| Repo | Week 1 | Week 2 |
| --- | --- | --- |
| requests | 40% | 38% |
| flask | 41% | 30% |
| httpx | 30% | 29% |
| django | 45% | 30% |

Part of the drop comes from reclassification rather than new edges: calls on methods inherited from library base classes now count as external, which shrinks the denominator. The T1 counts are the direct measure of new edges: flask rose from 1,088 to 1,499 and django from 73,702 to 74,757.

New T1 edges were hand-checked by diffing the week-1 and week-2 indexes and sampling only edges that were added: 15 / 15 correct on flask, 10 / 10 on httpx. No T1 edge was removed in any repo.

## Known limits

- **Django ORM managers.** `Model.objects` is created by a metaclass, so `Model.objects.filter(...)` doesn't resolve; `QuerySet.filter` shows few callers. Fixing this needs framework-specific rules or the compiler tier (SCIP), not more syntax.
- **Calls on values from untyped library code** stay unresolved by design rather than guessed.
