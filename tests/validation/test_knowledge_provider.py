"""Pure mocked contract checks for the optional Anthropic evaluation adapter."""
from types import SimpleNamespace
import unittest

from validation.knowledge.evaluation_provider import (
    AnthropicMessagesBackend,
    EvaluationProviderError,
    make_input_counter,
)


SAFE_TOOLS = {
    "read": {
        "name": "read",
        "description": "Read a bounded excerpt from the exported source corpus.",
        "input_schema": {"type": "object", "properties": {"path": {"type": "string"}},
                         "required": ["path"], "additionalProperties": False},
    },
    "query": {
        "name": "query",
        "description": "Query the bounded knowledge export.",
        "input_schema": {"type": "object", "properties": {"target": {"type": "string"}},
                         "required": ["target"], "additionalProperties": False},
    },
}


class FakeMessages:
    def __init__(self, *, counted=17, response=None, error=None):
        self.counted = counted
        self.response = response
        self.error = error
        self.count_calls = []
        self.create_calls = []

    def count_tokens(self, **kwargs):
        self.count_calls.append(kwargs)
        if self.error:
            raise self.error
        return SimpleNamespace(input_tokens=self.counted)

    def create(self, **kwargs):
        self.create_calls.append(kwargs)
        if self.error:
            raise self.error
        return self.response


class FakeClient:
    def __init__(self, messages):
        self.messages = messages
        self.option_calls = []

    def with_options(self, **kwargs):
        self.option_calls.append(kwargs)
        return self


def message(role, text, tool_call_id=None):
    return SimpleNamespace(role=role, text=text, tool_call_id=tool_call_id)


def response(*, content=(), usage=None, model="claude-haiku-4-5-20251001", stop_reason="end_turn"):
    return SimpleNamespace(content=list(content), usage=usage, model=model,
                           stop_reason=stop_reason, _request_id="req-safe-123")


class EvaluationProviderTests(unittest.TestCase):
    def test_counter_counts_system_and_only_the_selected_client_tools(self):
        messages_api = FakeMessages(counted=41)
        client = FakeClient(messages_api)
        count = make_input_counter(client, model="claude-haiku-4-5-20251001",
                                   settings={"thinking": {"type": "disabled"}},
                                   tool_registry=SAFE_TOOLS, timeout_seconds=30)

        self.assertEqual(41, count((message("system", "rules"), message("user", "question")), ("read",)))
        request = messages_api.count_calls[0]
        self.assertEqual("claude-haiku-4-5-20251001", request["model"])
        self.assertEqual("rules", request["system"])
        self.assertEqual(["read"], [tool["name"] for tool in request["tools"]])
        self.assertEqual({"type": "disabled"}, request["thinking"])
        self.assertEqual([{"max_retries": 0, "timeout": 30}], client.option_calls)

    def test_thinking_modes_refuse_before_counting_or_spending(self):
        for thinking in ({"type": "enabled", "budget_tokens": 1024}, {"type": "adaptive"}, None):
            with self.subTest(thinking=thinking):
                api = FakeMessages(response=response())
                client = FakeClient(api)
                with self.assertRaises(EvaluationProviderError) as caught:
                    AnthropicMessagesBackend(client, model="claude-haiku-4-5-20251001",
                                             settings={"thinking": thinking}, tool_registry=SAFE_TOOLS)
                self.assertEqual("UnsupportedThinkingMode", caught.exception.error_class)
                with self.assertRaises(EvaluationProviderError):
                    make_input_counter(client, model="claude-haiku-4-5-20251001",
                                       settings={"thinking": thinking}, tool_registry=SAFE_TOOLS,
                                       timeout_seconds=10)
                self.assertEqual([], api.create_calls)
                self.assertEqual([], api.count_calls)

    def test_unexpected_signed_thinking_cannot_become_unsigned_tool_history(self):
        for kind in ("thinking", "redacted_thinking"):
            with self.subTest(kind=kind):
                api = FakeMessages(response=response(content=[
                    SimpleNamespace(type=kind, thinking="opaque", signature="signed"),
                    SimpleNamespace(type="tool_use", id="toolu-1", name="read", input={"path": "x.py"}),
                ]))
                backend = AnthropicMessagesBackend(FakeClient(api), model="claude-haiku-4-5-20251001",
                                                   settings={"thinking": {"type": "disabled"}},
                                                   tool_registry=SAFE_TOOLS)
                with self.assertRaises(EvaluationProviderError) as caught:
                    backend.respond((message("user", "inspect"),), ("read",),
                                    max_output_tokens=100, timeout_seconds=10)
                self.assertEqual("UnsupportedThinkingResponse", caught.exception.error_class)
                self.assertEqual(1, len(api.create_calls))

    def test_tool_only_response_is_normalized_without_executing_it(self):
        reply = response(
            content=[SimpleNamespace(type="tool_use", id="toolu-1", name="read",
                                     input={"path": "src/example.py"})],
            usage=SimpleNamespace(input_tokens=10, cache_read_input_tokens=2,
                                  cache_creation_input_tokens=3, output_tokens=4,
                                  output_tokens_details=None),
            stop_reason="tool_use",
        )
        messages_api = FakeMessages(response=reply)
        client = FakeClient(messages_api)
        backend = AnthropicMessagesBackend(client, model="claude-haiku-4-5-20251001",
                                           settings={"temperature": 0}, tool_registry=SAFE_TOOLS)

        result = backend.respond((message("system", "rules"), message("user", "read it")),
                                 ("read",), max_output_tokens=128, timeout_seconds=30)

        self.assertEqual("", result.text)
        self.assertEqual("toolu-1", result.calls[0].id)
        self.assertEqual("read", result.calls[0].name)
        self.assertEqual('{"path":"src/example.py"}', result.calls[0].arguments_json)
        self.assertEqual("tool_use", result.stop_reason)
        self.assertEqual("claude-haiku-4-5-20251001", result.model)
        sent = messages_api.create_calls[0]
        self.assertEqual(["read"], [tool["name"] for tool in sent["tools"]])
        self.assertEqual(128, sent["max_tokens"])
        self.assertEqual(1, len(messages_api.create_calls))

    def test_cached_input_is_counted_once_and_cache_buckets_remain_separate(self):
        reply = response(content=[SimpleNamespace(type="text", text="done")],
                         usage=SimpleNamespace(input_tokens=10, cache_read_input_tokens=2,
                                               cache_creation_input_tokens=3, output_tokens=4,
                                               output_tokens_details=SimpleNamespace(thinking_tokens=1)))
        client = FakeClient(FakeMessages(response=reply))
        backend = AnthropicMessagesBackend(client, model="claude-haiku-4-5-20251001",
                                           settings={}, tool_registry=SAFE_TOOLS)

        result = backend.respond((message("user", "go"),), (),
                                 max_output_tokens=20, timeout_seconds=15)

        self.assertEqual(15, result.usage.input_tokens)
        self.assertEqual(5, result.usage.cached_input_tokens)
        self.assertEqual(2, result.usage.cache_read_tokens)
        self.assertEqual(3, result.usage.cache_creation_tokens)
        self.assertEqual(4, result.usage.output_tokens)
        self.assertEqual(1, result.usage.reasoning_tokens)
        self.assertEqual(19, result.usage.total_tokens)
        self.assertEqual("read_and_created", result.cache_state)

    def test_malformed_usage_stays_unavailable(self):
        reply = response(content=[SimpleNamespace(type="text", text="done")],
                         usage=SimpleNamespace(input_tokens=True, cache_read_input_tokens=0,
                                               cache_creation_input_tokens=0, output_tokens=-1,
                                               output_tokens_details=None))
        backend = AnthropicMessagesBackend(FakeClient(FakeMessages(response=reply)),
                                           model="claude-haiku-4-5-20251001",
                                           settings={}, tool_registry=SAFE_TOOLS)

        result = backend.respond((message("user", "go"),), (),
                                 max_output_tokens=20, timeout_seconds=15)

        self.assertIsNone(result.usage.input_tokens)
        self.assertIsNone(result.usage.output_tokens)
        self.assertIsNone(result.usage.total_tokens)
        self.assertIsNone(result.usage.reasoning_tokens)

    def test_missing_reasoning_usage_is_not_reported_as_zero(self):
        reply = response(content=[SimpleNamespace(type="text", text="done")],
                         usage=SimpleNamespace(input_tokens=10, cache_read_input_tokens=0,
                                               cache_creation_input_tokens=0, output_tokens=4))
        backend = AnthropicMessagesBackend(FakeClient(FakeMessages(response=reply)),
                                           model="claude-haiku-4-5-20251001",
                                           settings={}, tool_registry=SAFE_TOOLS)

        result = backend.respond((message("user", "go"),), (),
                                 max_output_tokens=20, timeout_seconds=15)

        self.assertIsNone(result.usage.reasoning_tokens)

    def test_unknown_tool_fails_closed_before_provider_call(self):
        messages_api = FakeMessages(response=response())
        backend = AnthropicMessagesBackend(FakeClient(messages_api),
                                           model="claude-haiku-4-5-20251001",
                                           settings={}, tool_registry=SAFE_TOOLS)

        with self.assertRaises(EvaluationProviderError):
            backend.respond((message("user", "go"),), ("terminal",),
                            max_output_tokens=20, timeout_seconds=15)
        self.assertEqual([], messages_api.create_calls)

    def test_provider_failure_is_redacted_and_not_retried_or_substituted(self):
        messages_api = FakeMessages(error=RuntimeError("api-key=secret; Authorization: Bearer hidden"))
        client = FakeClient(messages_api)
        backend = AnthropicMessagesBackend(client, model="claude-haiku-4-5-20251001",
                                           settings={}, tool_registry=SAFE_TOOLS)

        with self.assertRaises(EvaluationProviderError) as caught:
            backend.respond((message("user", "go"),), (),
                            max_output_tokens=20, timeout_seconds=15)
        self.assertEqual("ProviderError", caught.exception.error_class)
        self.assertNotIn("secret", str(caught.exception))
        self.assertNotIn("Bearer", str(caught.exception))
        self.assertEqual(1, len(messages_api.create_calls))
        self.assertEqual("claude-haiku-4-5-20251001", messages_api.create_calls[0]["model"])
        self.assertEqual([{"max_retries": 0, "timeout": 15}], client.option_calls)

    def test_tool_results_are_sent_as_anthropic_user_tool_result_blocks(self):
        messages_api = FakeMessages(counted=12)
        count = make_input_counter(FakeClient(messages_api), model="claude-haiku-4-5-20251001",
                                   settings={}, tool_registry=SAFE_TOOLS, timeout_seconds=10)

        count((message("user", "read"),
               message("assistant", '{"text":"","tool_calls":[{"id":"toolu-1",'
                       '"name":"read","arguments_json":"{\\"path\\":\\"src/a.py\\"}"}]}'),
               message("tool", '{"excerpt":"safe"}', "toolu-1")), ("read",))

        mapped = messages_api.count_calls[0]["messages"]
        self.assertEqual("assistant", mapped[1]["role"])
        self.assertEqual("tool_use", mapped[1]["content"][0]["type"])
        self.assertEqual("user", mapped[2]["role"])
        self.assertEqual("tool_result", mapped[2]["content"][0]["type"])
        self.assertEqual("toolu-1", mapped[2]["content"][0]["tool_use_id"])


if __name__ == "__main__":
    unittest.main()
