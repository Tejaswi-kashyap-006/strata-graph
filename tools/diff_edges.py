"""Sample CALLS edges present in a new index but not an old one, for hand-checking.

Usage: python tools/diff_edges.py <repo-root> <old graph.json> <n> [tier]
The new index is read from <repo-root>/.strata/graph.json.
"""
import json
import random
import sys
from pathlib import Path

root, old_path, n = Path(sys.argv[1]), Path(sys.argv[2]), int(sys.argv[3])
tier = sys.argv[4] if len(sys.argv) > 4 else "T1"


def keyed(g):
    nodes = g["nodes"]
    return {
        (nodes[e["src"]]["qname"], nodes[e["dst"]]["qname"]): (e, nodes[e["dst"]])
        for e in g["edges"]
        if e["kind"] == "CALLS"
    }


old = keyed(json.loads(old_path.read_text()))
new = keyed(json.loads((root / ".strata" / "graph.json").read_text()))
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
