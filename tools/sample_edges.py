"""Print a seeded random sample of CALLS edges with call-site and target source lines.

Usage: python tools/sample_edges.py <repo-root> <n_T1> <n_T0> <n_ambiguous>
Run `strata-graph --root <repo-root> index` first."""
import json
import random
import sys
from pathlib import Path

root = Path(sys.argv[1])
per_tier = {"T1": int(sys.argv[2]), "T0": int(sys.argv[3]), "T0-ambiguous": int(sys.argv[4])}
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


g = load_graph(root / ".strata" / "index.db")
nodes = g["nodes"]
rng = random.Random(int(__import__("os").environ.get("SEED", "7")))


def line(file, n):
    try:
        return (root / file).read_text(errors="replace").splitlines()[n - 1].strip()
    except Exception:
        return "?"


calls = [e for e in g["edges"] if e["kind"] == "CALLS"]
for tier, k in per_tier.items():
    pool = [e for e in calls if e["tier"] == tier]
    for i, e in enumerate(rng.sample(pool, min(k, len(pool)))):
        s, d = nodes[e["src"]], nodes[e["dst"]]
        print(f"[{tier} #{i}] {s['qname']}  ->  {d['qname']} ({d['kind']})")
        print(f"    call  {e['file']}:{e['line']}: {line(e['file'], e['line'])[:150]}")
        print(f"    def   {d['file']}:{d['start_line']}: {line(d['file'], d['start_line'])[:110]}")
