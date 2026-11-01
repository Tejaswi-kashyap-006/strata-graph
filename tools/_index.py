"""Read a strata-graph index (.strata/index.db) for the hand-check tools."""

import sqlite3
from pathlib import Path
from typing import Any

NODE_COLS = ["id", "kind", "qname", "name", "file", "start_line", "end_line", "parent"]
EDGE_COLS = ["src", "dst", "kind", "tier", "file", "line"]


def load_graph(db_path: Path) -> dict[str, list[dict[str, Any]]]:
    """Nodes (list position == id) and edges from an index, opened read-only."""
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        nodes = [
            dict(zip(NODE_COLS, row, strict=True))
            for row in con.execute(f"SELECT {', '.join(NODE_COLS)} FROM nodes ORDER BY id")
        ]
        edges = [
            dict(zip(EDGE_COLS, row, strict=True))
            for row in con.execute(f"SELECT {', '.join(EDGE_COLS)} FROM edges")
        ]
    finally:
        con.close()
    return {"nodes": nodes, "edges": edges}


def source_line(root: Path, file: str, num: int) -> str:
    """Line `num` (1-based) of `file` under `root`, stripped; "?" if unreadable."""
    try:
        return (root / file).read_text(errors="replace").splitlines()[num - 1].strip()
    except (OSError, IndexError):
        return "?"
