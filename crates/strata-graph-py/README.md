# strata-graph (Python)

Python bindings for [strata-graph](https://github.com/Tejaswi-kashyap-006/strata-graph), a code graph for coding agents.

```python
import strata_graph

g = strata_graph.open("path/to/repo")   # builds or updates .strata/index.db
g.callers("billing.tax.round_money", depth=2)
g.impact("Invoice.total")
g.path("app.main", "db.save")
```

Every edge carries a confidence tier (`T1`, `T0`, `T0-ambiguous`); queries default to `min_tier="t0"`. Only changed files are re-parsed when the code changes.
