"""Print a seeded random sample of CALLS edges with call-site and target source lines.

Usage: python tools/sample_edges.py <repo-root> <n_T1> <n_T0> <n_ambiguous>
Run `strata-graph --root <repo-root> index` first. Set SEED to vary the sample.
"""

import os
import random
import sys
from pathlib import Path

from _index import load_graph, source_line


def main() -> None:
    root = Path(sys.argv[1])
    per_tier = {"T1": int(sys.argv[2]), "T0": int(sys.argv[3]), "T0-ambiguous": int(sys.argv[4])}
    g = load_graph(root / ".strata" / "index.db")
    nodes = g["nodes"]
    rng = random.Random(int(os.environ.get("SEED", "7")))
    calls = [e for e in g["edges"] if e["kind"] == "CALLS"]
    for tier, k in per_tier.items():
        pool = [e for e in calls if e["tier"] == tier]
        for i, e in enumerate(rng.sample(pool, min(k, len(pool)))):
            s, d = nodes[e["src"]], nodes[e["dst"]]
            print(f"[{tier} #{i}] {s['qname']}  ->  {d['qname']} ({d['kind']})")
            call_src = source_line(root, e["file"], e["line"])[:150]
            print(f"    call  {e['file']}:{e['line']}: {call_src}")
            print(
                f"    def   {d['file']}:{d['start_line']}: "
                f"{source_line(root, d['file'], d['start_line'])[:110]}"
            )


if __name__ == "__main__":
    main()
