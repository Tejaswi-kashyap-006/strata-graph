"""Model access: a small interface, the OpenAI implementation, and costs."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Protocol


@dataclass(frozen=True)
class ToolCall:
    id: str
    name: str
    arguments: str


@dataclass
class Usage:
    input: int = 0
    cached_input: int = 0
    output: int = 0

    def add(self, other: Usage) -> None:
        self.input += other.input
        self.cached_input += other.cached_input
        self.output += other.output


@dataclass
class Completion:
    """One model reply: the assistant message to append to the conversation,
    its tool calls, and the tokens it used."""

    message: dict[str, Any]
    tool_calls: list[ToolCall] = field(default_factory=list)
    usage: Usage = field(default_factory=Usage)


class LLM(Protocol):
    def complete(
        self, messages: list[dict[str, Any]], tools: list[dict[str, Any]], tool_choice: Any
    ) -> Completion: ...


@dataclass(frozen=True)
class Prices:
    """US dollars per million tokens, as the user supplies them for their model."""

    input: float
    cached_input: float
    output: float

    def cost(self, u: Usage) -> float:
        fresh = max(u.input - u.cached_input, 0)
        return (
            fresh * self.input + u.cached_input * self.cached_input + u.output * self.output
        ) / 1e6


class OpenAIChat:
    """OpenAI Chat Completions with function calling. The key is read from
    OPENAI_API_KEY by the OpenAI client; it is never logged."""

    def __init__(
        self,
        model: str,
        temperature: float | None = None,
        seed: int | None = None,
        reasoning_effort: str | None = None,
        max_output_tokens: int | None = None,
    ) -> None:
        from openai import OpenAI  # imported here so tests and scoring need no API package

        self.client = OpenAI(max_retries=8, timeout=600)
        self.model = model
        self.temperature = temperature
        self.seed = seed
        self.reasoning_effort = reasoning_effort
        self.max_output_tokens = max_output_tokens

    def complete(
        self, messages: list[dict[str, Any]], tools: list[dict[str, Any]], tool_choice: Any
    ) -> Completion:
        kwargs: dict[str, Any] = {
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "tool_choice": tool_choice,
        }
        if self.temperature is not None:
            kwargs["temperature"] = self.temperature
        if self.seed is not None:
            kwargs["seed"] = self.seed
        if self.reasoning_effort:
            kwargs["reasoning_effort"] = self.reasoning_effort
        if self.max_output_tokens:
            kwargs["max_completion_tokens"] = self.max_output_tokens
        r = self.client.chat.completions.create(**kwargs)
        m = r.choices[0].message
        calls = [
            ToolCall(tc.id, tc.function.name, tc.function.arguments)
            for tc in (m.tool_calls or [])
            if getattr(tc, "function", None) is not None
        ]
        message: dict[str, Any] = {"role": "assistant", "content": m.content or ""}
        if calls:
            message["tool_calls"] = [
                {
                    "id": c.id,
                    "type": "function",
                    "function": {"name": c.name, "arguments": c.arguments},
                }
                for c in calls
            ]
        usage = Usage()
        if r.usage is not None:
            details = getattr(r.usage, "prompt_tokens_details", None)
            usage = Usage(
                input=r.usage.prompt_tokens,
                cached_input=(getattr(details, "cached_tokens", 0) or 0) if details else 0,
                output=r.usage.completion_tokens,
            )
        return Completion(message=message, tool_calls=calls, usage=usage)
