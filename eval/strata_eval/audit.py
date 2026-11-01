"""Leak audit: flag runs whose transcript quotes the gold fix.

The workspace holds only the base commit, so the fix cannot be read from it.
What remains is the model having memorized the benchmark. A run is flagged
when its assistant messages repeat a distinctive added line of the fix
verbatim, or name the fix's commit. Flagged runs are reported, never
silently dropped.
"""

from __future__ import annotations

import json
from pathlib import Path

from .tasks import parse_patch

MIN_LINE_CHARS = 30


def squash(text: str) -> str:
    """Collapse whitespace, so a reformatted quote still matches."""
    return " ".join(text.split())


def added_lines(patch: str) -> set[str]:
    """Distinctive lines the fix adds: long enough not to be boilerplate."""
    out: set[str] = set()
    for fp in parse_patch(patch):
        for _, _, body in fp.hunks:
            for line in body:
                text = line[1:].strip()
                if line.startswith("+") and len(text) >= MIN_LINE_CHARS:
                    out.add(squash(text))
    return out


def audit(work: Path) -> list[str]:
    flagged: list[str] = []
    patches = work / "gold"
    for trace_file in sorted((work / "traces").glob("*.jsonl")):
        run_id = trace_file.stem
        # Run ids are <instance_id>__<arm>__s<seed>; instance ids contain "__" too.
        iid = run_id.rsplit("__", 2)[0]
        patch_file = patches / f"{iid}.diff"
        if not patch_file.exists():
            continue
        lines = added_lines(patch_file.read_text(encoding="utf-8"))
        meta = patches / f"{iid}.json"
        commit = (
            json.loads(meta.read_text(encoding="utf-8")).get("fix_commit", "")
            if meta.exists()
            else ""
        )
        said = squash(
            " ".join(
                json.loads(t).get("assistant", "") or ""
                for t in trace_file.read_text(encoding="utf-8").splitlines()
                if t.strip()
            )
        )
        quoted = [ln for ln in lines if ln in said]
        if quoted or (commit and commit[:10] in said):
            what = f"quotes {len(quoted)} added line(s)" if quoted else "names the fix commit"
            flagged.append(f"{run_id}: {what}")
    return flagged
