"""The OpenAI client wrapper against a local stand-in for the Chat Completions
endpoint: the real SDK builds the requests and parses the replies."""

from __future__ import annotations

import json
import threading
from collections.abc import Iterator
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Any

import pytest

openai = pytest.importorskip("openai")

from strata_eval.llm import OpenAIChat  # noqa: E402


@pytest.fixture
def server(monkeypatch: pytest.MonkeyPatch) -> Iterator[list[dict[str, Any]]]:
    requests: list[dict[str, Any]] = []
    replies = [
        {
            "id": "c1",
            "object": "chat.completion",
            "created": 0,
            "model": "m",
            "choices": [
                {
                    "index": 0,
                    "finish_reason": "tool_calls",
                    "message": {
                        "role": "assistant",
                        "content": None,
                        "tool_calls": [
                            {
                                "id": "t1",
                                "type": "function",
                                "function": {"name": "search", "arguments": '{"pattern": "x"}'},
                            }
                        ],
                    },
                }
            ],
            "usage": {
                "prompt_tokens": 1200,
                "completion_tokens": 80,
                "total_tokens": 1280,
                "prompt_tokens_details": {"cached_tokens": 1024},
            },
        },
        {
            "id": "c2",
            "object": "chat.completion",
            "created": 0,
            "model": "m",
            "choices": [
                {
                    "index": 0,
                    "finish_reason": "stop",
                    "message": {"role": "assistant", "content": "done"},
                }
            ],
            "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12},
        },
    ]

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self) -> None:
            body = self.rfile.read(int(self.headers["Content-Length"]))
            requests.append(
                {
                    "path": self.path,
                    "body": json.loads(body),
                    "auth": self.headers.get("Authorization"),
                }
            )
            out = json.dumps(replies[min(len(requests) - 1, len(replies) - 1)]).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(out)))
            self.end_headers()
            self.wfile.write(out)

        def log_message(self, *args: Any) -> None:
            pass

    httpd = HTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    monkeypatch.setenv("OPENAI_API_KEY", "sk-test-not-real")
    monkeypatch.setenv("OPENAI_BASE_URL", f"http://127.0.0.1:{httpd.server_port}/v1")
    yield requests
    httpd.shutdown()


def test_requests_and_replies_round_trip(server: list[dict[str, Any]]) -> None:
    llm = OpenAIChat(
        "my-model", temperature=None, seed=3, reasoning_effort="low", max_output_tokens=4000
    )
    tools = [
        {
            "type": "function",
            "function": {"name": "search", "parameters": {"type": "object", "properties": {}}},
        }
    ]
    first = llm.complete([{"role": "user", "content": "hi"}], tools, "auto")
    assert [c.name for c in first.tool_calls] == ["search"]
    assert first.message["tool_calls"][0]["function"]["arguments"] == '{"pattern": "x"}'
    assert (first.usage.input, first.usage.cached_input, first.usage.output) == (1200, 1024, 80)

    # The assistant message and a tool reply go back in the next request as-is.
    messages = [
        {"role": "user", "content": "hi"},
        first.message,
        {"role": "tool", "tool_call_id": "t1", "content": "no matches"},
    ]
    second = llm.complete(messages, tools, {"type": "function", "function": {"name": "search"}})
    assert second.tool_calls == [] and second.message == {"role": "assistant", "content": "done"}
    assert second.usage.cached_input == 0

    body = server[0]["body"]
    assert server[0]["path"] == "/v1/chat/completions"
    assert body["model"] == "my-model" and body["seed"] == 3
    assert body["reasoning_effort"] == "low" and body["max_completion_tokens"] == 4000
    assert "temperature" not in body
    assert server[1]["body"]["messages"][1]["tool_calls"][0]["id"] == "t1"
    assert server[1]["body"]["tool_choice"] == {"type": "function", "function": {"name": "search"}}
