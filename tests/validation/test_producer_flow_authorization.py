"""No flow signal may be issued without the actor authorizing it first.

The actor owns which of two concurrent flow evaluations may signal the
producer. That ownership is worth nothing if a caller can reach the syscall
without asking, so this pins the call site rather than the actor: a unit test
on `apply_producer_flow_intention_at` proves the decision, not that anyone
obeys it.

Everything here is positive as well as negative. A rule that only forbids a
spelling passes vacuously the moment the thing it names is renamed.
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
TRANSCODE = ROOT / "crates/plurxd/src/transcode.rs"
CONTROL = ROOT / "crates/plurxd/src/playback_control.rs"
FLOW_FN = "async fn apply_ahead_window("


def strip_rust(source: str) -> str:
    """Blank line comments and string literals, preserving offsets."""
    out = list(source)
    index = 0
    length = len(source)
    while index < length:
        char = source[index]
        if char == "/" and source.startswith("//", index):
            while index < length and source[index] != "\n":
                out[index] = " "
                index += 1
        elif char == '"':
            out[index] = " "
            index += 1
            while index < length:
                if source[index] == "\\":
                    out[index] = " "
                    if index + 1 < length:
                        out[index + 1] = " "
                    index += 2
                    continue
                if source[index] == '"':
                    out[index] = " "
                    index += 1
                    break
                out[index] = " "
                index += 1
        else:
            index += 1
    return "".join(out)


def function_body(source: str, signature: str) -> str:
    if source.count(signature) != 1:
        raise AssertionError(f"{signature!r} must appear exactly once")
    masked = strip_rust(source)
    start = source.index(signature)
    opening = masked.index("{", masked.index(")", start))
    depth = 0
    for index in range(opening, len(masked)):
        if masked[index] == "{":
            depth += 1
        elif masked[index] == "}":
            depth -= 1
            if depth == 0:
                return source[opening : index + 1]
    raise AssertionError(f"unbalanced braces in {signature!r}")


class ProducerFlowAuthorizationTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.transcode = TRANSCODE.read_text(encoding="utf-8")
        cls.control = CONTROL.read_text(encoding="utf-8")
        cls.flow = function_body(cls.transcode, FLOW_FN)

    def test_the_flow_evaluator_asks_the_actor_before_it_signals(self) -> None:
        masked = strip_rust(self.flow)
        request = masked.find("request_producer_flow_before(")
        self.assertNotEqual(
            request, -1, f"{FLOW_FN} must ask the actor for authorization"
        )
        signal = masked.find(".signal(")
        self.assertNotEqual(signal, -1, f"{FLOW_FN} must still be the caller that signals")
        self.assertLess(
            request,
            signal,
            "the authorization request must precede the syscall; a signal issued "
            "before the actor has ordered the desire is the race this seam exists "
            "to close",
        )

    def test_every_non_issue_outcome_returns_without_signalling(self) -> None:
        masked = strip_rust(self.flow)
        request = masked.index("request_producer_flow_before(")
        signal = masked.index(".signal(")
        between = self.flow[request:signal]
        for outcome in ("Coalesced", "Settled", "Rejected"):
            with self.subTest(outcome=outcome):
                self.assertIn(
                    outcome,
                    between,
                    f"{outcome} must be handled between the request and the "
                    f"syscall; an unhandled outcome falls through and signals",
                )
        self.assertIn(
            "return",
            between,
            "the non-authorizing outcomes must return rather than continue",
        )

    def test_a_signal_that_published_no_barrier_settles_the_claim(self) -> None:
        self.assertIn(
            "settle_producer_flow_signal_before(",
            self.flow,
            "a failed or refused syscall publishes no acknowledgement barrier, "
            "so it must release the actor's outstanding claim explicitly or "
            "every later desire coalesces behind a signal that no longer exists",
        )

    def test_the_actor_is_the_only_thing_that_authorizes_a_signal(self) -> None:
        """Only the flow evaluator may reach the producer signal.

        A second call site would be a second decider, which is the whole
        defect. `AttemptChild::signal` is the supervisor's own method and is
        exempt; every other use has to be this one.
        """
        masked = strip_rust(self.transcode)
        # `.signal(` with an argument SENDS one. `status.signal()` with none
        # READS which signal killed a process — a different method on a
        # different type, and not a decider.
        callers = [m.start() for m in re.finditer(r"\.signal\(\s*[^)\s]", masked)]
        self.assertGreaterEqual(len(callers), 1, "the signal call site vanished")
        flow_start = self.transcode.index(FLOW_FN)
        flow_end = flow_start + len(self.flow)
        tests_start = masked.find("mod tests {")
        for at in callers:
            if flow_start <= at <= flow_end:
                continue
            if tests_start != -1 and at > tests_start:
                continue
            line = self.transcode.count("\n", 0, at) + 1
            line_start = self.transcode.rfind("\n", 0, at) + 1
            line_end = self.transcode.find("\n", at)
            context = self.transcode[max(0, at - 400) : at]
            # The supervisor's own dispatch owns the PID and is the thing the
            # actor authorizes; it is not a competing decider.
            if "AttemptChildCommand::Signal" in context or "fn signal" in context:
                continue
            self.fail(
                f"transcode.rs:{line} signals a producer outside the "
                f"authorized flow path: "
                f"{self.transcode[line_start:line_end].strip()!r}"
            )

    def test_the_authorization_reply_is_the_only_licence(self) -> None:
        """The actor must not hand out a second licence while one is out."""
        body = function_body(self.control, "fn apply_producer_flow_intention_at(")
        self.assertIn("producer_flow_signal_outstanding", body)
        self.assertIn("ProducerFlowIntentionOutcome::Coalesced", body)
        issue = body.index("ProducerFlowIntentionOutcome::Issue")
        coalesce = body.index("ProducerFlowIntentionOutcome::Coalesced")
        self.assertLess(
            coalesce,
            issue,
            "the outstanding-signal check must come before the issue path, or a "
            "second syscall races the first",
        )


if __name__ == "__main__":
    unittest.main()
