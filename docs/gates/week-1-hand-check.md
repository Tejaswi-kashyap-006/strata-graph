# Gate 1: edges hand-checked on 3 repos

Status: **passed for T1 and T0; ambiguous edges behave as labelled.** Checked 2026-11-01 on shallow clones of `psf/requests`, `pallets/flask` and `encode/httpx` (plus `django/django` for scale only).

## Method

`sample_edges.py` draws a seeded random sample of CALLS edges per tier and prints the call-site line and the target's definition line. Each edge was judged by reading the source, opening the surrounding function where the receiver's type was not obvious from one line.

## Results

| Tier | requests | flask | httpx | Total correct |
| --- | --- | --- | --- | --- |
| T1 | 12 / 12 | 12 / 12 | 12 / 12 | 36 / 36 |
| T0 | 8 / 8 | 10 / 10 | 8 / 8 | 26 / 26 |
| T0-ambiguous (each edge is one of 2–3 candidates) | 1 / 4 | 3 / 6 | 3 / 4 | 7 / 14 |

The first T0 sample on requests was 1 / 6 and ambiguous 0 / 4. That sample found three bugs, each now fixed with a regression line in `tests/fixtures/py_shop/expected_edges.txt`:

1. **Shadowing locals.** A parameter or local named like a repo function (pytest fixtures such as `httpbin`) fell back to the repo function. Locals and parameters now block the name-only fallback.
2. **Builtin method names.** `evt.set()`, `fp.read()`, `d.items()` on untyped receivers matched repo methods with the same name. A stop-list of builtin and stdlib method names now leaves these unresolved.
3. **Namespace subpackages.** `flask/sansio/` has no `__init__.py`, so its modules were named `app` instead of `flask.sansio.app`, which broke base-class resolution. Directories under a package now count as packages (PEP 420).

A fourth change, resolving `Foo(...).method()` to `Foo.method`, moved most of requests' remaining ambiguous edges to T1 (24 → 5).

The T0 and ambiguous samples above were drawn after fixes 1 and 2. The flask T0 sample predates fix 3, which only upgrades edges to T1.

## Resolution coverage (share of non-external calls)

| Repo | Python lines | Index time | T1 | T0 | Ambiguous | Unresolved |
| --- | --- | --- | --- | --- | --- | --- |
| requests | 12k | 0.17 s | 58% | 2% | 0% | 40% |
| flask | 18k | 0.17 s | 39% | 15% | 4% | 41% |
| httpx | 18k | 0.17 s | 65% | 3% | 2% | 30% |
| django | 531k | 4.2 s | 47% | 6% | 2% | 45% |

Timings are on a 2-core container. "Unresolved" includes calls on untyped values that may well be external (third-party objects, stdlib instances), so it overstates true misses. It is the number to drive down in week 2, mainly through return-type hints (`def f() -> Foo`) and `isinstance` narrowing.

## Precision policy this gate confirmed

- T1 and T0 are trustworthy enough to show agents by default.
- Ambiguous edges are right about half the time per edge, as designed. Agent-facing defaults (`impact`, `path`, in-band anchors) use `--min-tier t0` and leave them out.
