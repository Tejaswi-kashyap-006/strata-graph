# Week 4: adaptive delivery

Goal (spec): `annotate(file, density="auto")`, with delivery density chosen from the repo's shape, plus the week-3 carry-overs: a live agent run, a separate interface node kind, and the latency question.

## Live agent runs (Claude Code 2.1.316, Haiku, fixture repo)

| Run | Tools allowed | What the agent saw | Answer | Cost |
| --- | --- | --- | --- | --- |
| 1 | Read only, told not to open other files | SessionStart note, then facts after reading `shop/billing/tax.py` | Named both callers of `TaxCalculator.compute` and their tier. It said it had not checked the call sites itself, and that dynamic calls are not tracked. | $0.008, 2 turns |
| 2 | Read, Grep | Facts after one Grep: where `round_money` is defined, its call site, and what constructs `TaxCalculator` | Correct on all three, without opening a file | $0.009, 2 turns |

Both runs used the hooks installed by `strata-graph install-hooks --agent claude`, unmodified. **Codex was not run:** this workspace has no OpenAI credentials.

## Density

Spec rule (CodeAnchor heuristic): average call out-degree under 5 → both directions; over 8 → callers only; in between → both, with a cap. All four modes are implemented and can be forced. `auto` applies the rule to the **average number of distinct resolved callees per function that makes at least one resolved call**.

| Repo | Avg out-degree | Hubs (min callers) | Calls landing on hubs | `auto` picks |
| --- | --- | --- | --- | --- |
| django | 2.20 | 92 (88) | 42% | full |
| flask | 1.80 | 3 (57) | 25% | full |
| httpx | 2.01 | 4 (95) | 29% | full |
| requests | 1.93 | 3 (57) | 20% | full |
| hono | 1.73 | 8 (20) | 19% | full |
| zod | 2.06 | 17 (45) | 23% | full |
| taxonomy | 1.77 | 1 (75) | 38% | full |

**Finding:** on our graph the rule picks `full` everywhere. The paper's thresholds assume a denser graph, most likely one that also counts unresolved and library calls. Raw call sites per function range from 2.7 (flask) to 37 (hono), inflated by module-level test code, so that measure doesn't transfer either. Following the spec ("the eval tunes these thresholds per language"), the modes are forceable and the week-5 eval will choose. Until then, `auto` means `full`.

What does reduce noise now, in every mode:

- **Hubs:** calls to the top 1% most-called definitions (at least 20 callers) are counted, not named: "calls 2: MobileNav T1, 1 widely used helper".
- **Tests:** test callers and test subclasses are counted after real code: "called by 4: …get_search_results T1, …Variable.__init__ T1, 2 in tests". They are named only when nothing else uses a definition. On django, `text.py` lines 470–529 went from 732 to about 480 characters.
- **Wording:** classes say "constructed by" and interfaces say "implemented by". The agent in run 1 had to work out that "called by" on a class meant construction.

## Interface node kind

TypeScript interfaces are now `interface` nodes. The resolver treats classes and interfaces alike through one helper, so resolution should not change. To check this, every index was rebuilt from scratch with the previous commit and with the new code, and the two edge lists compared:

| | Python (django, flask, httpx, requests) | TypeScript (hono, zod, taxonomy) |
| --- | --- | --- |
| Edges compared | 141,171 | 17,833 |
| Edges different | 0 | 0 |
| Nodes relabelled | 0 | 1,005, all to `interface` |

A new fixture case covers a class implementing an interface and a call made through the interface type (2 new hand-checked edges, 34 TypeScript expectations in total).

## Bugs found this week

- **Stale engine, part 1.** An index written by an older build was reused whenever the hand-bumped `RESOLVER_REVISION` hadn't changed. On hono, an index written earlier in week 3 lacked 118 T1 calls and 3 inheritance edges that the current resolver finds, and still held 5 T1 calls it no longer makes. The fix: a build script now fingerprints the parser and resolver sources into the index stamp, so there is nothing to bump. A test covers parser changes (every file is re-parsed) and resolver changes (re-resolved from cached facts).
- **Stale engine, part 2.** Queries only re-checked files, never the engine, so after an upgrade hooks, MCP, the CLI and Python kept serving the old graph. `needs_update()` checks both. On django, the first call after a resolver change now takes 1.2–1.4 s, re-resolving from cached facts, and later calls are back to about 220 ms. This also explains the 3.5–4.6 s "slow first calls" from week 3: they were real re-resolutions after development builds.
- **Settings key order.** `install-hooks` sorted the keys of a user's settings file. It now preserves their order; a test checks it.

## Latency (30 random file reads per repo, fresh session, 2-core container)

| Repo | Median | p95 | Reads with facts | Median reply |
| --- | --- | --- | --- | --- |
| django (531k lines) | 217 ms | 264 ms | 20/30 | 1,170 chars |
| flask | 9 ms | 12 ms | 24/30 | 820 chars |
| hono | 15 ms | 22 ms | 18/30 | 289 chars |
| zod | 19 ms | 24 ms | 17/30 | 333 chars |

On django, the time is about 110 ms for the freshness check (walk and stat 3,045 files) and about 180 ms to load 47k nodes and 133k edges from SQLite. The freshness check stays, because it is what keeps facts right after edits. Loading only one file's neighbourhood instead of the whole graph would remove most of the load time; that is planned for week 6.

## Open items

- **Decision needed (spec, due week 4):** which model per agent for the eval, and the budget for 3 seeds × 4 arms × 3 agents.
- Codex live run: needs an OpenAI API key.
- Density thresholds: to be tuned by the week-5 eval.
- CI: still blocked by the GitHub Actions billing lock. Every check was run locally before the push.
