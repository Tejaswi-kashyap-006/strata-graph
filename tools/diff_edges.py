"""Sample CALLS edges present in a new index but not an old one, for hand-checking.

Usage: python tools/diff_edges.py <repo-root> <old index.db> <n> [tier]
The new index is read from <repo-root>/.strata/index.db.
"""

import random
import sys
from pathlib import Path
from typing import Any

from _index import load_graph, source_line

Keyed = dict[tuple[str, str], tuple[dict[str, Any], dict[str, Any]]]


def keyed(g: dict[str, list[dict[str, Any]]]) -> Keyed:
    nodes = g["nodes"]
    return {
        (nodes[e["src"]]["qname"], nodes[e["dst"]]["qname"]): (e, nodes[e["dst"]])
        for e in g["edges"]
        if e["kind"] == "CALLS"
    }


def main() -> None:
    root, old_path, n = Path(sys.argv[1]), Path(sys.argv[2]), int(sys.argv[3])
    tier = sys.argv[4] if len(sys.argv) > 4 else "T1"
    old = keyed(load_graph(old_path))
    new = keyed(load_graph(root / ".strata" / "index.db"))
    added = [
        (k, v)
        for k, v in new.items()
        if v[0]["tier"] == tier and (k not in old or old[k][0]["tier"] != tier)
    ]
    removed = [k for k in old if k not in new]
    print(f"{len(added)} new {tier} CALLS edges, {len(removed)} edges removed")
    for i, ((src, dst), (e, d)) in enumerate(random.Random(11).sample(added, min(n, len(added)))):
        print(f"[#{i}] {src}  ->  {dst} ({d['kind']})")
        print(f"    call  {e['file']}:{e['line']}: {source_line(root, e['file'], e['line'])[:150]}")
        print(
            f"    def   {d['file']}:{d['start_line']}: "
            f"{source_line(root, d['file'], d['start_line'])[:110]}"
        )


if __name__ == "__main__":
    main()
