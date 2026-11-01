"""Benchmark tasks: selection, clean workspaces, and gold locations.

A task is one SWE-bench instance: a repository at the commit before a fix,
the issue text, and the files and functions the real fix changed. Gold
locations come from the fix's patch alone, parsed with Python's own `ast`,
so they do not depend on strata-graph.
"""

from __future__ import annotations

import ast
import io
import json
import random
import re
import subprocess
import tarfile
from collections.abc import Iterable
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

HUNK = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


@dataclass(frozen=True)
class Task:
    """One localization task."""

    instance_id: str
    repo: str
    base_commit: str
    problem: str
    gold_files: list[str]
    gold_functions: list[str] = field(default_factory=list)

    def to_json(self) -> str:
        return json.dumps(asdict(self), sort_keys=True)

    @staticmethod
    def from_json(line: str) -> Task:
        return Task(**json.loads(line))


@dataclass
class FilePatch:
    """The hunks of a unified diff that apply to one file."""

    old_path: str | None
    new_path: str | None
    hunks: list[tuple[int, int, list[str]]] = field(default_factory=list)


def parse_patch(patch: str) -> list[FilePatch]:
    """Split a unified diff (as in SWE-bench `patch`) into per-file hunks.

    Each hunk is its old-file start line, its old-file line count, and its
    body lines, each starting with ' ', '-' or '+'. `old_path` is None for new
    files and `new_path` is
    None for deleted ones.
    """
    files: list[FilePatch] = []
    current: FilePatch | None = None
    body: list[str] | None = None
    lines = patch.splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        if line.startswith("--- ") and i + 1 < len(lines) and lines[i + 1].startswith("+++ "):
            old = _strip_prefix(line[4:])
            new = _strip_prefix(lines[i + 1][4:])
            current = FilePatch(old_path=old, new_path=new)
            files.append(current)
            body = None
            i += 2
            continue
        m = HUNK.match(line)
        if m and current is not None:
            body = []
            count = int(m.group(2)) if m.group(2) is not None else 1
            current.hunks.append((int(m.group(1)), count, body))
        elif body is not None and line[:1] in (" ", "-", "+"):
            body.append(line)
        elif body is not None and line == "":
            # Some diffs drop the space on blank context lines.
            body.append(" ")
        i += 1
    return files


def _strip_prefix(path: str) -> str | None:
    path = path.split("\t")[0].strip()
    if path == "/dev/null":
        return None
    return path[2:] if path[:2] in ("a/", "b/") else path


def apply_patch(old: list[str], fp: FilePatch) -> tuple[list[str], set[int], set[int]]:
    """Apply one file's hunks to its old lines.

    Returns the new lines, the 1-based old line numbers removed, and the
    1-based new line numbers added. Raises ValueError if context does not
    match, so a gold location is never computed from a misapplied patch.
    """
    new: list[str] = []
    removed: set[int] = set()
    added: set[int] = set()
    pos = 0  # index into old
    for start, count, body in fp.hunks:
        # A hunk with no old lines (diff -U0) is numbered by the line it
        # follows; any other hunk by its first old line.
        target = start if count == 0 else max(start - 1, 0)
        if target < pos:
            raise ValueError("overlapping hunks")
        new.extend(old[pos:target])
        pos = target
        for line in body:
            tag, text = line[:1], line[1:]
            if tag == " ":
                if pos >= len(old) or old[pos] != text:
                    raise ValueError(f"context mismatch at old line {pos + 1}")
                new.append(text)
                pos += 1
            elif tag == "-":
                if pos >= len(old) or old[pos] != text:
                    raise ValueError(f"removed line mismatch at old line {pos + 1}")
                removed.add(pos + 1)
                pos += 1
            else:
                new.append(text)
                added.add(len(new))
    new.extend(old[pos:])
    return new, removed, added


def def_ranges(source: str) -> list[tuple[str, int, int]]:
    """(qualname, first line, last line) of every class, top-level function and
    method in Python `source`. Functions nested in functions are folded into
    the outer one: a fix inside them is credited to the function an agent can
    name. Decorators count as part of the definition."""
    tree = ast.parse(source)
    out: list[tuple[str, int, int]] = []

    def visit(node: ast.AST, prefix: str) -> None:
        for child in ast.iter_child_nodes(node):
            if isinstance(child, ast.ClassDef | ast.FunctionDef | ast.AsyncFunctionDef):
                start = min([child.lineno] + [d.lineno for d in child.decorator_list])
                end = child.end_lineno or child.lineno
                name = f"{prefix}{child.name}"
                out.append((name, start, end))
                if isinstance(child, ast.ClassDef):
                    visit(child, f"{name}.")
            elif isinstance(child, ast.stmt | ast.excepthandler | ast.match_case):
                # Definitions under `if TYPE_CHECKING:`, `try/except ImportError`,
                # `with` and the like keep their enclosing name.
                visit(child, prefix)

    visit(tree, "")
    return out


def _innermost(ranges: list[tuple[str, int, int]], line: int) -> str | None:
    best: tuple[str, int, int] | None = None
    for r in ranges:
        if r[1] <= line <= r[2] and (best is None or r[2] - r[1] < best[2] - best[1]):
            best = r
    return best[0] if best else None


def gold_locations(root: Path, patch: str) -> tuple[list[str], list[str]]:
    """Files and functions the fix in `patch` changes, given the repo at its
    base commit in `root`.

    Files: every existing file the patch modifies or deletes (new files
    cannot be found by searching the old tree). Functions, for Python files:
    the innermost class, function or method enclosing a removed line (in the
    old file) or an added line (in the patched file), kept only if that name
    already existed at the base commit.
    """
    files: list[str] = []
    functions: list[str] = []
    for fp in parse_patch(patch):
        if fp.old_path is None:
            continue
        files.append(fp.old_path)
        if not fp.old_path.endswith(".py") or fp.new_path is None:
            continue
        old_text = (root / fp.old_path).read_text(encoding="utf-8")
        old_lines = old_text.split("\n")
        new_lines, removed, added = apply_patch(old_lines, fp)
        try:
            old_ranges = def_ranges(old_text)
            new_ranges = def_ranges("\n".join(new_lines))
        except SyntaxError:
            continue
        existing = {name for name, _, _ in old_ranges}
        names: list[str] = []
        for line in sorted(removed):
            n = _innermost(old_ranges, line)
            if n:
                names.append(n)
        for line in sorted(added):
            n = _innermost(new_ranges, line)
            if n:
                names.append(n)
        for n in dict.fromkeys(names):
            if n in existing:
                functions.append(f"{fp.old_path}::{n}")
    return list(dict.fromkeys(files)), list(dict.fromkeys(functions))


def select(
    rows: Iterable[dict[str, Any]], n: int, seed: int, multi_share: float
) -> list[dict[str, Any]]:
    """Pick `n` instances, about `multi_share` of them fixes touching 2+ files
    (the spec weights the slice toward multi-file changes), the rest at
    random. Deterministic for a given seed."""
    rows = sorted(rows, key=lambda r: r["instance_id"])
    multi = [r for r in rows if len([f for f in parse_patch(r["patch"]) if f.old_path]) >= 2]
    single = [r for r in rows if r not in multi]
    rng = random.Random(seed)
    rng.shuffle(multi)
    rng.shuffle(single)
    want_multi = min(len(multi), round(n * multi_share))
    picked = multi[:want_multi] + single[: n - want_multi]
    return sorted(picked, key=lambda r: r["instance_id"])


def export_tree(cache: Path, repo: str, commit: str, dest: Path) -> None:
    """Write the files of `repo` at `commit` into `dest`, with no `.git`, so
    no later history (including the fix) can be read from the workspace."""
    mirror = cache / (repo.replace("/", "__") + ".git")
    if not mirror.exists():
        mirror.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            ["git", "clone", "--mirror", "--quiet", f"https://github.com/{repo}.git", str(mirror)],
            check=True,
        )
    archive = subprocess.run(
        ["git", f"--git-dir={mirror}", "archive", "--format=tar", commit],
        check=True,
        capture_output=True,
    ).stdout
    dest.mkdir(parents=True, exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        safe_extract(tar, dest)


def safe_extract(tar: tarfile.TarFile, dest: Path) -> None:
    """Extract regular files and directories only, never outside `dest`.
    (`tarfile`'s own `filter="data"` is missing from early 3.10/3.11 releases.)"""
    root = dest.resolve()
    for member in tar.getmembers():
        target = (root / member.name).resolve()
        if root not in target.parents and target != root:
            raise ValueError(f"archive member escapes the workspace: {member.name}")
        if member.isdir():
            target.mkdir(parents=True, exist_ok=True)
        elif member.isfile():
            target.parent.mkdir(parents=True, exist_ok=True)
            src = tar.extractfile(member)
            if src is not None:
                target.write_bytes(src.read())
        # Symlinks, devices and the like are skipped: the agent reads files only.
