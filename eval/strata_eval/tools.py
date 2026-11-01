"""The agent's view of a repository: search, read and list, sandboxed.

The same implementation serves every arm, so differences between arms come
from strata-graph alone. It is pure Python with a fixed file order, so a run
gives the same tool output on any machine.
"""

from __future__ import annotations

import fnmatch
import re
from dataclasses import dataclass
from pathlib import Path

# Never shown to the agent: strata-graph's index (it would leak the graph to
# arm A) and version-control or dependency folders.
HIDDEN = {".strata", ".git", ".hg", ".svn", "node_modules", "__pycache__", ".tox", ".venv"}
MAX_FILE_BYTES = 1_000_000
MAX_SEARCH_HITS = 80
MAX_LINE_CHARS = 200
MAX_READ_LINES = 250
MAX_LIST_ENTRIES = 300


class ToolError(Exception):
    """A tool call the agent made incorrectly; reported back to it as text."""


@dataclass(frozen=True)
class TextFile:
    rel: str
    text: str


class Workspace:
    """A repository checkout the agent may read but not leave."""

    def __init__(self, root: Path) -> None:
        self.root = root.resolve()
        self._corpus: list[TextFile] | None = None

    def resolve(self, path: str) -> Path:
        """`path` (relative to the root, or absolute inside it) as an absolute
        path. Raises ToolError for anything outside the workspace or hidden."""
        raw = Path(path.strip() or ".")
        full = (raw if raw.is_absolute() else self.root / raw).resolve()
        if full != self.root and self.root not in full.parents:
            raise ToolError(f"{path}: outside the repository")
        if any(part in HIDDEN for part in full.relative_to(self.root).parts):
            raise ToolError(f"{path}: not available")
        return full

    def rel(self, full: Path) -> str:
        return full.relative_to(self.root).as_posix() or "."

    def corpus(self) -> list[TextFile]:
        """Every readable text file, sorted by path, loaded once."""
        if self._corpus is None:
            files: list[TextFile] = []
            for p in sorted(self._walk(self.root)):
                try:
                    if p.stat().st_size > MAX_FILE_BYTES:
                        continue
                    data = p.read_bytes()
                except OSError:
                    continue
                if b"\0" in data[:4096]:
                    continue
                files.append(TextFile(self.rel(p), data.decode("utf-8", errors="replace")))
            self._corpus = files
        return self._corpus

    def _walk(self, d: Path) -> list[Path]:
        out: list[Path] = []
        try:
            entries = list(d.iterdir())
        except OSError:
            return out
        for e in entries:
            if e.name in HIDDEN or e.is_symlink():
                continue
            if e.is_dir():
                out.extend(self._walk(e))
            elif e.is_file():
                out.append(e)
        return out

    # ------------------------------------------------------------- tools ---

    def list_dir(self, path: str = ".") -> str:
        d = self.resolve(path)
        if not d.is_dir():
            raise ToolError(f"{path}: not a directory")
        names = sorted(
            (e.name + "/" if e.is_dir() else e.name)
            for e in d.iterdir()
            if e.name not in HIDDEN and not e.is_symlink()
        )
        shown = names[:MAX_LIST_ENTRIES]
        more = f"\n... {len(names) - len(shown)} more entries" if len(names) > len(shown) else ""
        return "\n".join(shown) + more if shown else "(empty)"

    def read_file(
        self, path: str, start_line: int = 1, end_line: int | None = None
    ) -> tuple[str, int, int]:
        """Numbered lines `start_line..end_line` (at most MAX_READ_LINES).
        Returns the text and the range actually shown."""
        f = self.resolve(path)
        if not f.is_file():
            raise ToolError(f"{path}: not a file")
        if f.stat().st_size > MAX_FILE_BYTES:
            raise ToolError(f"{path}: larger than {MAX_FILE_BYTES} bytes")
        lines = f.read_text(encoding="utf-8", errors="replace").split("\n")
        if lines and lines[-1] == "":
            lines.pop()
        total = len(lines)
        start = max(1, start_line)
        if start > max(total, 1):
            raise ToolError(f"{path} has {total} lines; start_line {start_line} is past the end")
        end = total if end_line is None else min(end_line, total)
        end = min(end, start + MAX_READ_LINES - 1)
        if end < start:
            raise ToolError("end_line is before start_line")
        body = "\n".join(f"{i:>6}\t{lines[i - 1]}" for i in range(start, end + 1))
        note = (
            f"\n... lines {end + 1}-{total} not shown; call read_file with start_line={end + 1}"
            if end < total
            else ""
        )
        return f"{self.rel(f)} (lines {start}-{end} of {total})\n{body}{note}", start, end

    def search(self, pattern: str, path: str = ".", glob: str | None = None) -> str:
        """Lines matching a regular expression (an invalid one is searched as
        plain text), as `path:line: text`, in path order."""
        base = self.resolve(path)
        prefix = self.rel(base)
        try:
            rx = re.compile(pattern)
        except re.error:
            rx = re.compile(re.escape(pattern))
        hits: list[str] = []
        total = 0
        files_hit = 0
        for tf in self.corpus():
            if prefix != "." and tf.rel != prefix and not tf.rel.startswith(prefix + "/"):
                continue
            if (
                glob
                and not fnmatch.fnmatch(tf.rel.rsplit("/", 1)[-1], glob)
                and not fnmatch.fnmatch(tf.rel, glob)
            ):
                continue
            if not rx.search(tf.text):
                continue
            files_hit += 1
            for n, line in enumerate(tf.text.split("\n"), start=1):
                if rx.search(line):
                    total += 1
                    if len(hits) < MAX_SEARCH_HITS:
                        hits.append(f"{tf.rel}:{n}: {line.strip()[:MAX_LINE_CHARS]}")
        if not hits:
            return "no matches"
        more = (
            f"\n... {total - len(hits)} more matches in {files_hit} files; "
            "narrow the pattern, path or glob"
            if total > len(hits)
            else ""
        )
        return "\n".join(hits) + more
