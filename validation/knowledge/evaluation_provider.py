"""Dependency-injected Anthropic Messages backend for the bounded evaluator.

This module never imports an SDK, resolves credentials, opens files, or executes
tool calls. The caller supplies an authenticated client and a closed registry of
client-side function schemas; only schemas named for the current run are sent.
"""
from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass
import json
import re
from typing import Any, Callable


class EvaluationProviderError(RuntimeError):
    """Redacted, bounded backend failure suitable for an evaluation receipt."""

    def __init__(self, error_class: str):
        self.error_class = error_class
        super().__init__(f"Anthropic Messages request failed ({error_class}).")


@dataclass(frozen=True)
class Usage:
    """Provider usage; cached input is included in input_tokens and is a subset."""

    input_tokens: int | None = None
    output_tokens: int | None = None
    cached_input_tokens: int | None = None
    reasoning_tokens: int | None = None
    cache_read_tokens: int | None = None
    cache_creation_tokens: int | None = None

    @property
    def total_tokens(self) -> int | None:
        if self.input_tokens is None or self.output_tokens is None:
            return None
        return self.input_tokens + self.output_tokens


@dataclass(frozen=True)
class ToolCall:
    id: str
    name: str
    arguments_json: str


@dataclass(frozen=True)
class Response:
    text: str
    calls: tuple[ToolCall, ...] = ()
    usage: Usage = Usage()
    request_id: str | None = None
    cache_state: str | None = None
    model: str | None = None
    stop_reason: str | None = None


_SETTING_KEYS = frozenset({
    "thinking", "output_config", "temperature", "top_p", "top_k", "stop_sequences",
})
_SAFE_ERROR_CLASSES = frozenset({
    "APITimeoutError", "APIConnectionError", "RateLimitError", "APIStatusError",
    "BadRequestError", "AuthenticationError", "PermissionDeniedError", "NotFoundError",
})
_CLASS_NAME = re.compile(r"^[A-Za-z][A-Za-z0-9_]{0,63}$")


def _provider_error(exc: Exception) -> EvaluationProviderError:
    """Preserve only a bounded SDK exception class, never its message or headers."""
    name = type(exc).__name__
    if name not in _SAFE_ERROR_CLASSES or not _CLASS_NAME.fullmatch(name):
        name = "ProviderError"
    return EvaluationProviderError(name)


def _field(value: Any, name: str, default: Any = None) -> Any:
    if isinstance(value, Mapping):
        return value.get(name, default)
    return getattr(value, name, default)


def _count(value: Any) -> int | None:
    if type(value) is int and value >= 0:
        return value
    return None


def _json_copy(value: Any) -> Any:
    """Freeze request data into JSON values without importing provider code."""
    try:
        return json.loads(json.dumps(value, ensure_ascii=False, allow_nan=False))
    except (TypeError, ValueError, OverflowError) as exc:
        raise EvaluationProviderError("InvalidRequestData") from None


def _checked_settings(settings: Mapping[str, Any]) -> dict[str, Any]:
    if not isinstance(settings, Mapping) or settings.keys() - _SETTING_KEYS:
        raise EvaluationProviderError("InvalidSettings")
    copied = _json_copy(dict(settings))
    if not isinstance(copied, dict):
        raise EvaluationProviderError("InvalidSettings")
    # Message/Response cannot preserve signed reasoning blocks. Refuse before
    # counting or spending instead of silently corrupting a tool round trip.
    if "thinking" in copied and copied["thinking"] != {"type": "disabled"}:
        raise EvaluationProviderError("UnsupportedThinkingMode")
    return copied


def _checked_registry(tool_registry: Mapping[str, Mapping[str, Any]]) -> dict[str, dict[str, Any]]:
    if not isinstance(tool_registry, Mapping):
        raise EvaluationProviderError("InvalidToolRegistry")
    result: dict[str, dict[str, Any]] = {}
    for name, raw in tool_registry.items():
        if not isinstance(name, str) or not name or not isinstance(raw, Mapping):
            raise EvaluationProviderError("InvalidToolRegistry")
        # Only Anthropic's client-side function schema is accepted. This excludes
        # server tools and any executable handler from the model request.
        if set(raw) != {"name", "description", "input_schema"} or raw.get("name") != name:
            raise EvaluationProviderError("InvalidToolRegistry")
        if not isinstance(raw.get("description"), str) or not isinstance(raw.get("input_schema"), Mapping):
            raise EvaluationProviderError("InvalidToolRegistry")
        schema = _json_copy(dict(raw))
        if not isinstance(schema["input_schema"], dict) or schema["input_schema"].get("type") != "object":
            raise EvaluationProviderError("InvalidToolRegistry")
        result[name] = schema
    return result


def _selected_tools(registry: Mapping[str, dict[str, Any]], names: Sequence[str]) -> list[dict[str, Any]]:
    if not isinstance(names, (tuple, list)) or any(not isinstance(name, str) for name in names):
        raise EvaluationProviderError("InvalidToolSelection")
    if len(set(names)) != len(names):
        raise EvaluationProviderError("InvalidToolSelection")
    if any(not isinstance(name, str) or name not in registry for name in names):
        raise EvaluationProviderError("UnknownTool")
    return [_json_copy(registry[name]) for name in names]


def _system_and_messages(messages: Sequence[Any]) -> tuple[str | None, list[dict[str, Any]]]:
    system: list[str] = []
    output: list[dict[str, Any]] = []
    for message in messages:
        role = _field(message, "role")
        text = _field(message, "text")
        if not isinstance(text, str):
            raise EvaluationProviderError("InvalidTranscript")
        if role == "system":
            system.append(text)
        elif role == "tool":
            call_id = _field(message, "tool_call_id")
            if not isinstance(call_id, str) or not call_id:
                raise EvaluationProviderError("InvalidTranscript")
            output.append({"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": call_id, "content": text,
            }]})
        elif role == "user":
            output.append({"role": "user", "content": text})
        elif role == "assistant":
            output.append({"role": "assistant", "content": _assistant_content(text)})
        else:
            raise EvaluationProviderError("InvalidTranscript")
    if not output:
        raise EvaluationProviderError("InvalidTranscript")
    return ("\n\n".join(system) if system else None), output


def _assistant_content(text: str) -> str | list[dict[str, Any]]:
    """Decode the evaluator's canonical assistant envelope into native blocks."""
    try:
        envelope = json.loads(text)
    except (TypeError, ValueError):
        return text
    if not isinstance(envelope, dict) or "tool_calls" not in envelope:
        return text
    if set(envelope) != {"text", "tool_calls"} or not isinstance(envelope["text"], str):
        raise EvaluationProviderError("InvalidTranscript")
    calls = envelope["tool_calls"]
    if not isinstance(calls, list):
        raise EvaluationProviderError("InvalidTranscript")
    blocks: list[dict[str, Any]] = []
    if envelope["text"]:
        blocks.append({"type": "text", "text": envelope["text"]})
    seen: set[str] = set()
    for call in calls:
        if not isinstance(call, dict) or set(call) != {"id", "name", "arguments_json"}:
            raise EvaluationProviderError("InvalidTranscript")
        call_id, name, arguments_json = call["id"], call["name"], call["arguments_json"]
        if (not isinstance(call_id, str) or not call_id or call_id in seen
                or not isinstance(name, str) or not name or not isinstance(arguments_json, str)):
            raise EvaluationProviderError("InvalidTranscript")
        try:
            arguments = json.loads(arguments_json)
        except (TypeError, ValueError):
            raise EvaluationProviderError("InvalidTranscript") from None
        if not isinstance(arguments, dict):
            raise EvaluationProviderError("InvalidTranscript")
        seen.add(call_id)
        blocks.append({"type": "tool_use", "id": call_id, "name": name, "input": arguments})
    return blocks


def _request_kwargs(messages: Sequence[Any], tool_names: Sequence[str], *, model: str,
                    settings: Mapping[str, Any], registry: Mapping[str, dict[str, Any]]) -> dict[str, Any]:
    if not isinstance(model, str) or not model.strip():
        raise EvaluationProviderError("InvalidModel")
    system, api_messages = _system_and_messages(messages)
    kwargs: dict[str, Any] = {
        "model": model,
        "messages": api_messages,
        "tools": _selected_tools(registry, tool_names),
    }
    if system is not None:
        kwargs["system"] = system
    if "thinking" in settings:
        kwargs["thinking"] = settings["thinking"]
    return kwargs


def _no_retry_messages(client: Any, timeout_seconds: int) -> Any:
    if type(timeout_seconds) is not int or timeout_seconds <= 0:
        raise EvaluationProviderError("InvalidTimeout")
    options = getattr(client, "with_options", None)
    if not callable(options):
        raise EvaluationProviderError("NoRetryClientUnavailable")
    try:
        return options(max_retries=0, timeout=timeout_seconds).messages
    except Exception as exc:
        raise _provider_error(exc) from None


def _usage(response: Any) -> Usage:
    raw = _field(response, "usage")
    if raw is None:
        return Usage()
    input_uncached = _count(_field(raw, "input_tokens"))
    output = _count(_field(raw, "output_tokens"))
    cache_read = _count(_field(raw, "cache_read_input_tokens"))
    cache_creation = _count(_field(raw, "cache_creation_input_tokens"))
    reasoning_raw = _field(_field(raw, "output_tokens_details"), "thinking_tokens")
    reasoning = _count(reasoning_raw) if reasoning_raw is not None else None
    # Missing or malformed counters remain unavailable. Anthropic's input_tokens
    # excludes both cache buckets, so total input is their sum exactly once.
    total_input = None
    cached = None
    if all(value is not None for value in (input_uncached, cache_read, cache_creation)):
        total_input = input_uncached + cache_read + cache_creation
        cached = cache_read + cache_creation
    if reasoning is not None and output is not None and reasoning > output:
        reasoning = None
    return Usage(
        input_tokens=total_input,
        output_tokens=output,
        cached_input_tokens=cached,
        reasoning_tokens=reasoning,
        cache_read_tokens=cache_read,
        cache_creation_tokens=cache_creation,
    )


def _response(client_response: Any, allowed_tool_names: frozenset[str]) -> Response:
    content = _field(client_response, "content")
    if not isinstance(content, (list, tuple)):
        raise EvaluationProviderError("InvalidResponse")
    text_parts: list[str] = []
    calls: list[ToolCall] = []
    for block in content:
        kind = _field(block, "type")
        if kind == "text":
            value = _field(block, "text")
            if isinstance(value, str):
                text_parts.append(value)
        elif kind in {"thinking", "redacted_thinking"}:
            raise EvaluationProviderError("UnsupportedThinkingResponse")
        elif kind == "tool_use":
            call_id, name, arguments = (_field(block, "id"), _field(block, "name"), _field(block, "input"))
            if not isinstance(call_id, str) or not call_id or not isinstance(name, str) or not name:
                raise EvaluationProviderError("InvalidResponse")
            if name not in allowed_tool_names:
                raise EvaluationProviderError("UnexpectedToolCall")
            if any(call.id == call_id for call in calls):
                raise EvaluationProviderError("InvalidResponse")
            if not isinstance(arguments, Mapping):
                raise EvaluationProviderError("InvalidResponse")
            try:
                canonical_arguments = json.dumps(dict(arguments), ensure_ascii=False,
                                                 allow_nan=False, sort_keys=True,
                                                 separators=(",", ":"))
            except (TypeError, ValueError, OverflowError):
                raise EvaluationProviderError("InvalidResponse") from None
            calls.append(ToolCall(call_id, name, canonical_arguments))
    reported_model = _field(client_response, "model")
    if not isinstance(reported_model, str) or not reported_model:
        reported_model = None
    request_id = _field(client_response, "_request_id")
    if not isinstance(request_id, str) or not request_id:
        request_id = None
    stop_reason = _field(client_response, "stop_reason")
    if not isinstance(stop_reason, str) or not stop_reason:
        stop_reason = None
    raw_usage = _field(client_response, "usage")
    cache_read = _count(_field(raw_usage, "cache_read_input_tokens"))
    cache_creation = _count(_field(raw_usage, "cache_creation_input_tokens"))
    cache_state = None
    if cache_read is not None and cache_creation is not None:
        if cache_read and cache_creation:
            cache_state = "read_and_created"
        elif cache_read:
            cache_state = "read"
        elif cache_creation:
            cache_state = "created"
        else:
            cache_state = "none_reported"
    return Response(
        text="\n\n".join(text_parts), calls=tuple(calls), usage=_usage(client_response),
        request_id=request_id, cache_state=cache_state,
        model=reported_model, stop_reason=stop_reason,
    )


class AnthropicMessagesBackend:
    """One stateless request at a time, using an already-authenticated client."""

    def __init__(self, client: Any, *, model: str, settings: Mapping[str, Any],
                 tool_registry: Mapping[str, Mapping[str, Any]]):
        if not isinstance(model, str) or not model.strip():
            raise EvaluationProviderError("InvalidModel")
        self._client = client
        self._model = model
        self._settings = _checked_settings(settings)
        self._tool_registry = _checked_registry(tool_registry)

    def respond(self, messages: tuple[Any, ...], tool_names: tuple[str, ...], *,
                max_output_tokens: int, timeout_seconds: int) -> Response:
        if type(max_output_tokens) is not int or max_output_tokens <= 0:
            raise EvaluationProviderError("InvalidOutputLimit")
        request = _request_kwargs(messages, tool_names, model=self._model,
                                  settings=self._settings, registry=self._tool_registry)
        request["max_tokens"] = max_output_tokens
        request.update(self._settings)
        try:
            result = _no_retry_messages(self._client, timeout_seconds).create(**request)
        except EvaluationProviderError:
            raise
        except Exception as exc:
            raise _provider_error(exc) from None
        return _response(result, frozenset(tool_names))


def make_input_counter(client: Any, *, model: str, settings: Mapping[str, Any],
                       tool_registry: Mapping[str, Mapping[str, Any]], timeout_seconds: int,
                       ) -> Callable[[tuple[Any, ...], tuple[str, ...]], int]:
    """Create the runner's exact-input counter; runner reserves max output separately."""
    if not isinstance(model, str) or not model.strip():
        raise EvaluationProviderError("InvalidModel")
    checked_settings = _checked_settings(settings)
    checked_registry = _checked_registry(tool_registry)

    def count_input(messages: tuple[Any, ...], tool_names: tuple[str, ...]) -> int:
        request = _request_kwargs(messages, tool_names, model=model,
                                  settings=checked_settings, registry=checked_registry)
        # Token counting accepts input-affecting request fields; output settings
        # and max_tokens are accounted for by the runner's separate reservation.
        try:
            response = _no_retry_messages(client, timeout_seconds).count_tokens(**request)
        except EvaluationProviderError:
            raise
        except Exception as exc:
            raise _provider_error(exc) from None
        count = _count(_field(response, "input_tokens"))
        if count is None:
            raise EvaluationProviderError("InvalidTokenCount")
        return count

    return count_input
