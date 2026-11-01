"""The four eval arms and their strata-graph adapters.

Both adapters drive the shipped binary exactly as agents do: arm C sends the
same hook events Claude Code sends to `strata-graph hook`, and arm B speaks
MCP to a `strata-graph mcp` server. Nothing here re-implements strata-graph.
"""

from __future__ import annotations

import json
import subprocess
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Any

ARMS = {
    "A": "baseline: search, read and list only",
    "B": "+ strata-graph MCP tools the agent may call",
    "C": "+ strata-graph facts appended to search and read results (hooks)",
    "D": "B and C together",
}


@dataclass(frozen=True)
class Arm:
    name: str

    @property
    def tools(self) -> bool:
        """Offers strata-graph's MCP tools."""
        return self.name in ("B", "D")

    @property
    def hooks(self) -> bool:
        """Appends hook output to tool results."""
        return self.name in ("C", "D")


def parse_arms(spec: str) -> list[Arm]:
    arms = [Arm(a.strip().upper()) for a in spec.split(",") if a.strip()]
    bad = [a.name for a in arms if a.name not in ARMS]
    if bad or not arms:
        raise ValueError(f"unknown arm(s) {bad}; choose from {', '.join(ARMS)}")
    return arms


class Hook:
    """Runs `strata-graph hook` for one agent session."""

    def __init__(
        self, binary: str, root: Path, session_id: str, max_chars: int, density: str
    ) -> None:
        self.binary = binary
        self.root = root
        self.session_id = session_id
        self.args = [binary, "hook", "--max-chars", str(max_chars), "--density", density]

    def _send(self, event: dict[str, Any]) -> str:
        event = {
            "session_id": self.session_id,
            "transcript_path": "",
            "cwd": str(self.root),
            **event,
        }
        out = subprocess.run(
            self.args,
            input=json.dumps(event),
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
        if not out.stdout.strip():
            return ""
        reply = json.loads(out.stdout)
        return str(reply.get("hookSpecificOutput", {}).get("additionalContext", ""))

    def session_start(self) -> str:
        return self._send({"hook_event_name": "SessionStart", "source": "startup"})

    def after_read(self, abs_path: Path, start: int, end: int) -> str:
        return self._send(
            {
                "hook_event_name": "PostToolUse",
                "tool_name": "Read",
                "tool_input": {
                    "file_path": str(abs_path),
                    "offset": start,
                    "limit": end - start + 1,
                },
            }
        )

    def after_search(self, pattern: str) -> str:
        return self._send(
            {
                "hook_event_name": "PostToolUse",
                "tool_name": "Grep",
                "tool_input": {"pattern": pattern},
            }
        )


class McpClient:
    """A minimal MCP stdio client for one `strata-graph mcp` server."""

    def __init__(self, binary: str, root: Path) -> None:
        self.proc = subprocess.Popen(
            [binary, "--root", str(root), "mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )
        self._id = 0
        self._lock = threading.Lock()
        init = self._request(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "strata-eval", "version": "0"},
            },
        )
        if "result" not in init:
            raise RuntimeError(f"MCP initialize failed: {init}")
        self.instructions: str = str(init["result"].get("instructions", ""))
        self._notify("notifications/initialized")
        listed = self._request("tools/list", {})
        self.tools: list[dict[str, Any]] = listed["result"]["tools"]

    def _write(self, msg: dict[str, Any]) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def _notify(self, method: str) -> None:
        self._write({"jsonrpc": "2.0", "method": method})

    def _request(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        with self._lock:
            self._id += 1
            self._write({"jsonrpc": "2.0", "id": self._id, "method": method, "params": params})
            assert self.proc.stdout is not None
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("MCP server closed its output")
            reply: dict[str, Any] = json.loads(line)
            return reply

    def openai_tools(self) -> list[dict[str, Any]]:
        """The server's tools as OpenAI function definitions."""
        return [
            {
                "type": "function",
                "function": {
                    "name": t["name"],
                    "description": t.get("description", ""),
                    "parameters": t.get("inputSchema", {"type": "object", "properties": {}}),
                },
            }
            for t in self.tools
        ]

    def call(self, name: str, arguments: dict[str, Any]) -> str:
        reply = self._request("tools/call", {"name": name, "arguments": arguments})
        if "error" in reply:
            return f"error: {reply['error'].get('message', reply['error'])}"
        content = reply["result"].get("content", [])
        return "\n".join(c.get("text", "") for c in content if c.get("type") == "text")

    def close(self) -> None:
        if self.proc.poll() is None:
            self.proc.kill()
        self.proc.wait(timeout=10)
