"""Sample CALLS edges present in a new index but not an old one, for hand-checking.

Usage: python tools/diff_edges.py <repo-root> <old index.db> <n> [tier]
The new index is read from <repo-root>/.strata/index.db.
"""
import json
import random
import sys
from pathlib import Path

root, old_path, n = Path(sys.argv[1]), Path(sys.argv[2]), int(sys.argv[3])
tier = sys.argv[4] if len(sys.argv) > 4 else "T1"


def load_graph(db_path):
    """Nodes (indexed by id) and edges from a strata-graph index.db."""
    import sqlite3

    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    cols = ["id", "kind", "qname", "name", "file", "start_line", "end_line", "parent"]
    nodes = [dict(zip(cols, r)) for r in con.execute(f"SELECT {', '.join(cols)} FROM nodes ORDER BY id")]
    ecols = ["src", "dst", "kind", "tier", "file", "line"]
    edges = [dict(zip(ecols, r)) for r in con.execute(f"SELECT {', '.join(ecols)} FROM edges")]
    con.close()
    return {"nodes": nodes, "edges": edges}


def keyed(g):
    nodes = g["nodes"]
    return {
        (nodes[e["src"]]["qname"], nodes[e["dst"]]["qname"]): (e, nodes[e["dst"]])
        for e in g["edges"]
        if e["kind"] == "CALLS"
    }


old = keyed(load_graph(old_path))
new = keyed(load_graph(root / ".strata" / "index.db"))
added = [(k, v) for k, v in new.items() if v[0]["tier"] == tier and (k not in old or old[k][0]["tier"] != tier)]
removed = [k for k, v in old.items() if k not in new]
print(f"{len(added)} new {tier} CALLS edges, {len(removed)} edges removed")


def line(file, num):
    try:
        return (root / file).read_text(errors="replace").splitlines()[num - 1].strip()
    except Exception:
        return "?"


for i, ((src, dst), (e, d)) in enumerate(random.Random(11).sample(added, min(n, len(added)))):
    print(f"[#{i}] {src}  ->  {dst} ({d['kind']})")
    print(f"    call  {e['file']}:{e['line']}: {line(e['file'], e['line'])[:150]}")
    print(f"    def   {d['file']}:{d['start_line']}: {line(d['file'], d['start_line'])[:110]}")
