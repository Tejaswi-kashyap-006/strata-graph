# Week 3, part 1: TypeScript and JavaScript

Status: **TypeScript, TSX, JavaScript and JSX are indexed with the same tiered resolution as Python.** Checked 2026-11-01 on shallow clones of `honojs/hono` (library with classes), `colinhacks/zod` (type-heavy monorepo) and `shadcn-ui/taxonomy` (Next.js app with `@/` aliases).

## What resolves at T1

- Relative imports, `index` files, and `.js` specifiers that point at `.ts` sources
- tsconfig/jsconfig `paths` and `baseUrl`, including `extends` chains and JSONC (comments, trailing commas)
- The repo's own packages imported by name (`import "zod/v4"`), through package.json `exports` (every condition is tried; the first that exists as an indexed source file wins), then `src/` conventions
- Default exports, `export { default as X } from`, `export * from`, `export * as ns from`, and local aliases such as `export { _null as null }`
- `this`, `super`, class fields (annotated or initialised with `new`), constructor parameter properties (`constructor(private repo: Repo)`), typed parameters and variables, declared return types (`Promise<T>`, `T | null` and generic instances reduce to `T`)
- `new Foo()` and JSX components (`<Card />`; intrinsic tags such as `<div>` are not calls)
- `implements` and interface `extends` become INHERITS edges, so impact analysis reaches implementations

## Precision rules added this week (both languages)

| Rule | Why |
| --- | --- |
| A call chain rooted at an import from outside the repo is external (`np.zeros(3).render()`, `lodash.chain(x).render()`) | It never means a repo method with the same name |
| A repo module or class that lacks the member leaves the call unresolved | It is never some other class's method |
| TypeScript: unique-name method matches on untyped receivers are labelled `T0-ambiguous` | Python's equivalent was 26 / 26 correct in hand checks; TypeScript's was about half right on zod (callback parameters, object-literal namespaces) |
| The index is stamped with the engine version and resolver revision | Upgrading strata-graph re-resolves an existing index instead of serving edges from the old rules |

## Hand-checked samples

| Repo | T1 correct | Notes |
| --- | --- | --- |
| hono | 12 / 12 | |
| zod | 22 / 22 | Two samples: before and after workspace-package resolution |
| taxonomy | 12 / 12 | All through `@/` aliases and JSX components |

T0 samples before the TypeScript demotion: hono 5 / 6, zod roughly 4 / 10. Those edges are now labelled ambiguous.

## Coverage and speed (2 cores)

| Repo | TS/JS lines | Cold index | T1 | Ambiguous | External | Unresolved |
| --- | --- | --- | --- | --- | --- | --- |
| hono | 93k | 1.4 s | 7,971 | 745 | 27,450 | 8,536 |
| zod | 103k | 2.0 s | 19,712 | 3,063 | 23,701 | 8,262 |
| taxonomy | 8k | 0.3 s | 321 | 0 | 688 | 170 |

Resolving zod's own package (`"zod/v4"`) raised its T1 calls from 7,559 to 19,712.

## Bugs found while checking

1. JSONC with a comment between a trailing comma and `}` produced invalid JSON, so tsconfig paths were ignored. Fixed with two passes; regression test added.
2. zod's `packages/zod/.gitignore` ignores `**/package.json`, so its package was never discovered. Package discovery no longer consults ignore files.
3. An existing index kept serving edges from the previous resolver after an upgrade. Fixed by the engine stamp; `incremental.rs` has a test.

## Known limits

- Object-literal namespaces (`const z = { ...schemas, ...checks }`) and untyped callback parameters can't be followed without type inference.
- Only the root tsconfig/jsconfig is read; per-package tsconfig `paths` in monorepos are not yet.
- CommonJS `require()` and dynamic `import()` are not resolved.
- Getters (`get foo()`) are indexed as methods.
