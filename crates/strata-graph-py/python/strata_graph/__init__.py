"""strata-graph: a code graph for coding agents.

>>> import strata_graph
>>> g = strata_graph.open("path/to/repo")       # builds or updates .strata/index.db
>>> for hit in g.callers("billing.tax.round_money", depth=2):
...     print(hit["tier"], hit["qname"], hit["at"])

Every edge carries a confidence tier: "T1" (resolved through scopes, imports,
self/super, type hints or pytest fixtures), "T0" (the only definition with
that name), or "T0-ambiguous" (one of 2-3 same-named definitions). Queries
default to ``min_tier="t0"``, which leaves ambiguous edges out.

The graph picks up file changes before each query (``auto_refresh=True``);
only changed files are re-parsed.
"""

from __future__ import annotations

import json
from typing import Any, Dict, List, Optional

from ._native import NativeGraph, StrataError, __version__
from ._native import open as _open

__all__ = ["Graph", "StrataError", "open", "__version__"]

Hit = Dict[str, Any]
"""A node reached by a traversal: qname, kind, file, line, depth, via, tier, at."""

NodeInfo = Dict[str, Any]
"""A symbol: id, kind, qname, name, file, start_line, end_line, parent."""


class Graph:
    """An open index for one repository. Create it with :func:`open`."""

    def __init__(self, native: NativeGraph) -> None:
        self._g = native

    def __repr__(self) -> str:
        return f"strata_graph.Graph(root={self.root!r})"

    @property
    def root(self) -> str:
        """Absolute path of the indexed repository."""
        return self._g.root

    def refresh(self) -> Dict[str, Any]:
        """Re-index changed files now and return the update report."""
        return json.loads(self._g.refresh())

    def stale_files(self) -> int:
        """Files added, removed or modified since the last update."""
        return self._g.stale_files()

    def stats(self) -> Dict[str, Any]:
        """Node and edge counts by kind and tier, plus call-resolution counts."""
        return json.loads(self._g.stats())

    def find(self, query: str) -> List[NodeInfo]:
        """Symbols by exact qualified name, qualified suffix, or simple name."""
        return json.loads(self._g.find(query))

    def search(self, query: str, limit: int = 20) -> List[NodeInfo]:
        """Full-text name search; each word matches as a prefix."""
        return json.loads(self._g.search(query, limit))

    def node(self, symbol: str) -> NodeInfo:
        """The single symbol `symbol` names; raises StrataError if it is ambiguous."""
        return json.loads(self._g.node(symbol))

    def callers(self, symbol: str, depth: int = 1, min_tier: str = "t0") -> List[Hit]:
        """Who calls `symbol`, up to `depth` hops back."""
        return json.loads(self._g.callers(symbol, depth, min_tier))

    def callees(self, symbol: str, depth: int = 1, min_tier: str = "t0") -> List[Hit]:
        """What `symbol` calls, up to `depth` hops forward."""
        return json.loads(self._g.callees(symbol, depth, min_tier))

    def impact(self, symbol: str, depth: int = 3, min_tier: str = "t0") -> List[Hit]:
        """Transitive callers and subclasses: what may break if `symbol` changes."""
        return json.loads(self._g.impact(symbol, depth, min_tier))

    def path(
        self, source: str, target: str, max_depth: int = 8, min_tier: str = "t0"
    ) -> Optional[List[str]]:
        """Shortest call path from `source` to `target` as qualified names, or None."""
        out = self._g.path(source, target, max_depth, min_tier)
        return None if out is None else json.loads(out)


def open(path: str = ".", auto_refresh: bool = True) -> Graph:  # noqa: A001
    """Open the index for the repository at `path`, building or updating it first.

    With ``auto_refresh`` on, each query first checks for changed files and
    re-indexes them, so answers always match the code on disk.
    """
    return Graph(_open(path, auto_refresh))
