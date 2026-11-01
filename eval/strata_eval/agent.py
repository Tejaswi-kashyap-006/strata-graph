"""One localization run: a tool-calling loop over a sandboxed workspace."""

from __future__ import annotations

import json
import time
from collections import Counter
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

from .arms import Arm, Hook, McpClient
from .llm import LLM, Usage
from .tasks import Task
from .tools import ToolError, Workspace

MAX_RESULT_CHARS = 24_000
MAX_SUBMITTED = 10

SYSTEM = """You are a software engineer localizing a bug. The repository is checked out \
read-only at the commit before the fix. Find the files and functions that must change to \
resolve the issue. You cannot edit or run code.

Explore with the tools. When you are confident, call submit once, with up to 10 file paths \
and up to 10 functions, most likely first. Paths are relative to the repository root. Name \
functions as path::Qualified.name, for example src/pkg/mod.py::ClassName.method or \
src/pkg/mod.py::function."""

BASE_TOOLS: list[dict[str, Any]] = [
    {
        "type": "function",
        "function": {
            "name": "search",
            "description": (
                "Search file contents with a regular expression. Returns path:line: text."
            ),
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Python regular expression"},
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search (default: whole repo)",
                    },
                    "glob": {
                        "type": "string",
                        "description": "Only files matching this glob, e.g. *.py",
                    },
                },
                "required": ["pattern"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "read_file",
            "description": "Read numbered lines of a file (at most 250 per call).",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "start_line": {"type": "integer", "minimum": 1},
                    "end_line": {"type": "integer", "minimum": 1},
                },
                "required": ["path"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "list_dir",
            "description": "List a directory's entries.",
            "parameters": {"type": "object", "properties": {"path": {"type": "string"}}},
        },
    },
    {
        "type": "function",
        "function": {
            "name": "submit",
            "description": "Submit the final answer. Call once.",
            "parameters": {
                "type": "object",
                "properties": {
                    "files": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Ranked file paths",
                    },
                    "functions": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Ranked functions as path::Qualified.name",
                    },
                },
                "required": ["files", "functions"],
            },
        },
    },
]


@dataclass
class RunResult:
    """Everything scored or audited about one run."""

    run_id: str
    instance_id: str
    arm: str
    seed: int
    model: str
    files: list[str] = field(default_factory=list)
    functions: list[str] = field(default_factory=list)
    finish: str = "none"  # submitted | forced | no_answer | error
    error: str = ""
    turns: int = 0
    tool_calls: dict[str, int] = field(default_factory=dict)
    hook_chars: int = 0
    usage: dict[str, int] = field(default_factory=dict)
    cost_usd: float = 0.0
    seconds: float = 0.0


def _clip(text: str) -> str:
    if len(text) <= MAX_RESULT_CHARS:
        return text
    return text[:MAX_RESULT_CHARS] + f"\n... output cut at {MAX_RESULT_CHARS} characters"


def _strings(value: Any) -> list[str]:
    if not isinstance(value, list):
        return []
    return [str(v).strip() for v in value if str(v).strip()][:MAX_SUBMITTED]


class Run:
    """State of one agent run. Call `execute`."""

    def __init__(
        self,
        llm: LLM,
        task: Task,
        workspace: Path,
        arm: Arm,
        seed: int,
        model: str,
        binary: str,
        max_turns: int,
        hook_max_chars: int = 2000,
        density: str = "auto",
    ) -> None:
        self.llm = llm
        self.task = task
        self.ws = Workspace(workspace)
        self.arm = arm
        self.max_turns = max_turns
        self.run_id = f"{task.instance_id}__{arm.name}__s{seed}"
        self.result = RunResult(self.run_id, task.instance_id, arm.name, seed, model)
        self.trace: list[dict[str, Any]] = []
        self.usage = Usage()
        session = self.run_id.replace(".", "-")
        self.hook = (
            Hook(binary, self.ws.root, session, hook_max_chars, density) if arm.hooks else None
        )
        self.mcp_binary = binary
        self.tool_counts: Counter[str] = Counter()

    def _tool(self, name: str, args: dict[str, Any], mcp: McpClient | None) -> str:
        if name == "search":
            pattern = str(args.get("pattern", ""))
            if not pattern:
                raise ToolError("search needs a pattern")
            out = self.ws.search(pattern, str(args.get("path") or "."), args.get("glob") or None)
            if self.hook:
                extra = self.hook.after_search(pattern)
                if extra:
                    self.result.hook_chars += len(extra)
                    out += "\n\n" + extra
            return out
        if name == "read_file":
            path = str(args.get("path", ""))
            end = args.get("end_line")
            text, start, last = self.ws.read_file(
                path, int(args.get("start_line") or 1), int(end) if end else None
            )
            if self.hook:
                extra = self.hook.after_read(self.ws.resolve(path), start, last)
                if extra:
                    self.result.hook_chars += len(extra)
                    text += "\n\n" + extra
            return text
        if name == "list_dir":
            return self.ws.list_dir(str(args.get("path") or "."))
        if mcp is not None and name.startswith("strata_"):
            return mcp.call(name, args)
        raise ToolError(f"unknown tool {name}")

    def execute(self, prices: Any = None) -> tuple[RunResult, list[dict[str, Any]]]:
        """Run to completion. Never raises: failures are recorded in the result."""
        t0 = time.monotonic()
        mcp: McpClient | None = None
        try:
            tools = list(BASE_TOOLS)
            system = SYSTEM
            if self.arm.tools:
                mcp = McpClient(self.mcp_binary, self.ws.root)
                tools = [*tools[:-1], *mcp.openai_tools(), tools[-1]]
                if mcp.instructions:
                    system += (
                        "\n\nInstructions from the strata-graph tool server:\n" + mcp.instructions
                    )
            messages: list[dict[str, Any]] = [{"role": "system", "content": system}]
            if self.hook:
                start_ctx = self.hook.session_start()
                if start_ctx:
                    self.result.hook_chars += len(start_ctx)
                    messages.append({"role": "system", "content": start_ctx})
            messages.append(
                {
                    "role": "user",
                    "content": (
                        f"Issue:\n{self.task.problem}\n\n"
                        "Find the code that must change to fix this issue."
                    ),
                }
            )
            self._loop(messages, tools, mcp)
        except Exception as e:  # recorded, not raised: one bad run must not stop a batch
            self.result.finish = "error"
            self.result.error = f"{type(e).__name__}: {e}"[:2000]
        finally:
            if mcp is not None:
                mcp.close()
        self.result.tool_calls = dict(self.tool_counts)
        self.result.usage = asdict(self.usage)
        if prices is not None:
            self.result.cost_usd = round(prices.cost(self.usage), 6)
        self.result.seconds = round(time.monotonic() - t0, 2)
        return self.result, self.trace

    def _loop(
        self, messages: list[dict[str, Any]], tools: list[dict[str, Any]], mcp: McpClient | None
    ) -> None:
        idle = 0
        for turn in range(1, self.max_turns + 1):
            last = turn == self.max_turns
            choice: Any = {"type": "function", "function": {"name": "submit"}} if last else "auto"
            if last:
                messages.append(
                    {
                        "role": "user",
                        "content": "Turn limit reached. Call submit now with your best answer.",
                    }
                )
            reply = self.llm.complete(messages, tools, choice)
            self.result.turns = turn
            self.usage.add(reply.usage)
            messages.append(reply.message)
            self.trace.append(
                {
                    "turn": turn,
                    "assistant": reply.message.get("content", ""),
                    "usage": asdict(reply.usage),
                }
            )
            if not reply.tool_calls:
                idle += 1
                if idle >= 2:
                    self.result.finish = "no_answer"
                    return
                messages.append(
                    {"role": "user", "content": "Use the tools to explore, then call submit."}
                )
                continue
            idle = 0
            submitted = False
            for call in reply.tool_calls:
                self.tool_counts[call.name] += 1
                try:
                    args = json.loads(call.arguments or "{}")
                    if not isinstance(args, dict):
                        raise ToolError("arguments must be a JSON object")
                    if call.name == "submit":
                        self.result.files = _strings(args.get("files"))
                        self.result.functions = _strings(args.get("functions"))
                        self.result.finish = "forced" if last else "submitted"
                        submitted = True
                        output = "Submitted."
                    else:
                        output = self._tool(call.name, args, mcp)
                except (ToolError, ValueError) as e:
                    output = f"error: {e}"
                output = _clip(output)
                messages.append({"role": "tool", "tool_call_id": call.id, "content": output})
                self.trace.append(
                    {"turn": turn, "tool": call.name, "arguments": call.arguments, "output": output}
                )
            if submitted:
                return
        self.result.finish = self.result.finish if self.result.finish != "none" else "no_answer"
