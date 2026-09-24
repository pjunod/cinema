"""Every continuation fence in PlayerController.swift names its epoch scopes.

The controller has nine epoch counters and, before A-02, fifty-eight places
that compared some subset of them with a conjunction written out by hand. The
subset was the whole decision and none of them recorded it, so a fence that
omitted a counter deliberately looked exactly like one that forgot it.

`Attempt.stillCurrent(_:scopes:)` names the subset. This audit is what lets the
migration to it finish: the allow-list in `validation/attempt-census.toml`
holds the comparisons that have not been migrated yet, counted, and the census
fails if the source and that list disagree in either direction. A new
`Task {}` that compares an epoch by hand fails it; so does a revert of a
migrated fence; so does a row left behind after its fence is gone.

The synthetic sources below are the proof that it fails for each of those, and
they are written as edits to the real files rather than to a fixture, so the
audit is exercised against the shape of the code it actually guards. Every
edit goes through `replace_once`, which fails the test if its anchor is not in
the file exactly once — an anchor that silently stopped matching would turn a
case into a test of the unedited repository. The per-fence cases find their
anchors from the `[fences]` table rather than from quoted source, so a new
fence is covered by adding its row.
"""

from __future__ import annotations

import re
import tomllib
import unittest

from validation import attempt_census
from validation.attempt_census import ALLOWLIST, ATTEMPT_SOURCE, SOURCE, audit, repository_read

#: The field a hand-written conjunction compares for each scope, for
#: rebuilding the conjunction a migrated fence replaced.
SCOPE_FIELDS = {
    "lifecycle": "lifecycleGeneration",
    "open": "openGeneration",
    "viewerAction": "viewerActionEpoch",
    "initialDecision": "initialDecisionGeneration",
    "createRetry": "createRetryEpoch",
    "preparedAlignment": "preparedAlignmentGeneration",
    "seek": "seekState.generation",
    "pgsSelection": "pgsOverlaySelectionGeneration",
    "pgsItem": "pgsOverlayItemGeneration",
}


def read_with(**overrides):
    """A `read` that substitutes text for the named repository paths."""

    def read(path: str) -> str:
        if path in overrides:
            return overrides[path]
        return repository_read(path)

    return read


def replace_once(case: unittest.TestCase, text: str, old: str, new: str) -> str:
    """`text` with `old` replaced, failing `case` unless `old` occurs once."""
    case.assertEqual(text.count(old), 1, f"anchor not found exactly once: {old!r}")
    return text.replace(old, new, 1)


def recorded_fences() -> dict[str, dict]:
    return dict(tomllib.loads(repository_read(ALLOWLIST))["fences"])


def fence_call(case: unittest.TestCase, source: str, fence: str) -> str:
    """The exact text of `fence`'s one call site in the controller."""
    calls = re.findall(
        rf"attemptStillCurrent\(\s*[^,()]+,\s*fence:\s*\.{fence}\s*\)", source
    )
    case.assertEqual(len(calls), 1, f"{fence}: {calls}")
    return calls[0]


def fence_arm(case: unittest.TestCase, attempt: str, fence: str) -> str:
    """The exact text of `fence`'s arm in `AttemptFence.scopes`."""
    arms = re.findall(rf"case\s+\.{fence}\s*:\s*return\s*\[[^\]]*\]", attempt)
    case.assertEqual(len(arms), 1, f"{fence}: {arms}")
    return arms[0]


class AttemptScopeCensusCase(unittest.TestCase):
    def test_the_repository_agrees_with_its_own_census(self) -> None:
        self.assertEqual(audit(repository_read), ())

    def test_every_migrated_fence_names_at_least_one_scope(self) -> None:
        """A fence with an empty scope set decides nothing."""
        declared = attempt_census.declared_fences(repository_read(ATTEMPT_SOURCE))
        self.assertTrue(declared, "no fence has been migrated to Attempt at all")
        self.assertEqual(set(declared), set(recorded_fences()))
        for name, scopes in declared.items():
            self.assertTrue(scopes, name)

    def test_swapping_any_fences_scope_set_is_reported(self) -> None:
        """The review's experiment on #464, per fence: rewrite one fence's set
        to a scope it never compared, and the census must name that fence."""
        attempt = repository_read(ATTEMPT_SOURCE)
        for fence, row in recorded_fences().items():
            with self.subTest(fence=fence):
                unrelated = next(s for s in SCOPE_FIELDS if s not in row["scopes"])
                arm = fence_arm(self, attempt, fence)
                swapped = replace_once(
                    self, attempt, arm, f"case .{fence}: return [.{unrelated}]"
                )
                errors = audit(read_with(**{ATTEMPT_SOURCE: swapped}))

                self.assertEqual(len(errors), 1, errors)
                self.assertIn(f"AttemptFence.{fence}", errors[0])
                self.assertIn("never widens, narrows or swaps", errors[0])

    def test_dropping_one_scope_from_any_fence_is_reported(self) -> None:
        """Narrowing is the quiet version of the swap: a fence that stops
        reading `.viewerAction` survives the Pause it exists to see."""
        attempt = repository_read(ATTEMPT_SOURCE)
        for fence, row in recorded_fences().items():
            for dropped in row["scopes"]:
                with self.subTest(fence=fence, dropped=dropped):
                    kept = ", ".join(f".{s}" for s in row["scopes"] if s != dropped)
                    arm = fence_arm(self, attempt, fence)
                    narrowed = replace_once(
                        self, attempt, arm, f"case .{fence}: return [{kept}]"
                    )
                    errors = audit(read_with(**{ATTEMPT_SOURCE: narrowed}))

                    self.assertTrue(errors)
                    self.assertTrue(any(f"AttemptFence.{fence}" in e for e in errors), errors)

    def test_all_four_fences_swapped_at_once_are_each_reported(self) -> None:
        """The exact rewrite the review ran on mba, which the census used to
        pass: every fence moved onto one unrelated scope in the same edit."""
        attempt = repository_read(ATTEMPT_SOURCE)
        rewrites = {
            "stallRecovery": "pgsSelection",
            "seekPresentationDeadline": "seek",
            "blackFrameDecodeFailure": "initialDecision",
            "itemFailureLadder": "pgsItem",
        }
        self.assertEqual(set(rewrites), set(recorded_fences()))
        for fence, scope in rewrites.items():
            attempt = replace_once(
                self, attempt, fence_arm(self, attempt, fence), f"case .{fence}: return [.{scope}]"
            )
        errors = audit(read_with(**{ATTEMPT_SOURCE: attempt}))

        self.assertEqual(len(errors), 4, errors)
        for fence in rewrites:
            self.assertTrue(any(f"AttemptFence.{fence} " in e for e in errors), (fence, errors))

    def test_a_fence_used_from_another_continuation_is_reported(self) -> None:
        """Borrowing another fence's case is swapping a set by another name."""
        source = repository_read(SOURCE)
        fences = sorted(recorded_fences())
        for index, fence in enumerate(fences):
            other = fences[(index + 1) % len(fences)]
            with self.subTest(fence=fence, other=other):
                call = fence_call(self, source, fence)
                borrowed = replace_once(
                    self, source, call, call.replace(f".{fence}", f".{other}")
                )
                errors = audit(read_with(**{SOURCE: borrowed}))

                self.assertEqual(len(errors), 2, errors)
                joined = "\n".join(errors)
                self.assertIn(f"AttemptFence.{fence}, but", joined)
                self.assertIn(f"AttemptFence.{other} 1 time(s)", joined)

    def test_a_direct_still_current_call_is_reported(self) -> None:
        """A literal scope set at the call site is the shape #464 started
        from, and it is exactly what nothing could pin."""
        source = repository_read(SOURCE)
        for fence, row in recorded_fences().items():
            with self.subTest(fence=fence):
                call = fence_call(self, source, fence)
                captured = call.split("(", 1)[1].split(",", 1)[0].strip()
                scopes = ", ".join(f".{s}" for s in row["scopes"])
                direct = replace_once(
                    self,
                    source,
                    call,
                    f"{captured}.stillCurrent(self.snapshotAttempt(), scopes: [{scopes}])",
                )
                errors = audit(read_with(**{SOURCE: direct}))

                self.assertEqual(len(errors), 2, errors)
                joined = "\n".join(errors)
                self.assertIn(f"`{row['function']}()` calls stillCurrent directly", joined)
                self.assertIn(f"uses AttemptFence.{fence}, but", joined)

    def test_a_fence_without_a_census_row_and_a_row_without_a_fence_are_reported(self) -> None:
        allowlist = repository_read(ALLOWLIST)
        row = next(line for line in allowlist.splitlines() if line.startswith("stallRecovery = "))
        unrecorded = replace_once(self, allowlist, row + "\n", "")
        errors = audit(read_with(**{ALLOWLIST: unrecorded}))
        self.assertEqual(len(errors), 2, errors)
        self.assertIn("declares AttemptFence.stallRecovery", errors[0])

        orphaned = allowlist + (
            'aFenceThatDoesNotExist = { function = "load", scopes = ["open"] }\n'
        )
        errors = audit(read_with(**{ALLOWLIST: orphaned}))
        self.assertEqual(len(errors), 2, errors)
        self.assertIn("aFenceThatDoesNotExist", errors[0])

    def test_a_new_hand_written_fence_is_reported(self) -> None:
        """The case the census exists for: a later `Task {}` that compares an
        epoch by hand instead of naming what it depends on."""
        source = replace_once(
            self,
            repository_read(SOURCE),
            "    private func isCurrentLifecycle(_ generation: Int) -> Bool {",
            "    private func speculativeWork(_ generation: Int) -> Bool {\n"
            "        openGeneration == generation\n"
            "    }\n\n"
            "    private func isCurrentLifecycle(_ generation: Int) -> Bool {",
        )
        errors = audit(read_with(**{SOURCE: source}))

        self.assertEqual(len(errors), 1, errors)
        self.assertIn("openGeneration == generation", errors[0])
        self.assertIn("speculativeWork()", errors[0])
        self.assertIn("Attempt.stillCurrent(_:scopes:)", errors[0])

    def test_reverting_any_migrated_fence_is_reported(self) -> None:
        """Restoring a hand-written conjunction in place of any migrated
        fence puts one comparison per scope back into a function the
        allow-list does not excuse, and drops the fence's recorded call; the
        census reports every one of them."""
        source = repository_read(SOURCE)
        for fence, row in recorded_fences().items():
            with self.subTest(fence=fence):
                conjunction = ", ".join(
                    f"self.{SCOPE_FIELDS[scope]} == captured_{scope}" for scope in row["scopes"]
                )
                reverted = replace_once(self, source, fence_call(self, source, fence), conjunction)
                errors = audit(read_with(**{SOURCE: reverted}))

                self.assertEqual(len(errors), len(row["scopes"]) + 1, errors)
                joined = "\n".join(errors)
                self.assertIn(f"{row['function']}()", joined)
                for scope in row["scopes"]:
                    self.assertIn(f"self.{SCOPE_FIELDS[scope]} == captured_{scope}", joined)

    def test_a_second_copy_of_an_excused_fence_is_reported(self) -> None:
        """The allow-list excuses a count, not a licence to repeat it."""
        source = replace_once(
            self,
            repository_read(SOURCE),
            "    private func isSuperseded(_ generation: Int) -> Bool "
            "{ generation != openGeneration }",
            "    private func isSuperseded(_ generation: Int) -> Bool "
            "{ generation != openGeneration }\n"
            "    private func isSupersededTwice(_ generation: Int) -> Bool "
            "{ generation != openGeneration }",
        )
        source = replace_once(self, source, "isSupersededTwice", "isSuperseded")
        errors = audit(read_with(**{SOURCE: source}))

        self.assertEqual(len(errors), 1, errors)
        self.assertIn("appears 2 time(s)", errors[0])
        self.assertIn("allows 1", errors[0])

    def test_an_allow_list_row_that_outlived_its_fence_is_reported(self) -> None:
        allowlist = replace_once(
            self,
            repository_read(ALLOWLIST),
            "[allowed]\n",
            '[allowed]\n"aFunctionThatDoesNotExist::openGeneration == generation" = 1\n',
        )
        errors = audit(read_with(**{ALLOWLIST: allowlist}))

        self.assertEqual(len(errors), 1, errors)
        self.assertIn("aFunctionThatDoesNotExist", errors[0])
        self.assertIn("Lower the row", errors[0])

    def test_a_scope_the_attempt_type_does_not_declare_is_reported(self) -> None:
        attempt = repository_read(ATTEMPT_SOURCE)
        for fence, row in recorded_fences().items():
            with self.subTest(fence=fence):
                typo = [f".{s}" for s in row["scopes"]]
                typo[-1] = typo[-1] + "x"
                misspelt = replace_once(
                    self,
                    attempt,
                    fence_arm(self, attempt, fence),
                    f"case .{fence}: return [{', '.join(typo)}]",
                )
                errors = audit(read_with(**{ATTEMPT_SOURCE: misspelt}))

                self.assertEqual(len(errors), 2, errors)
                self.assertIn(f"AttemptFence.{fence} names scope `{typo[-1]}`", errors[0])

    def test_dropping_a_scope_from_the_attempt_type_is_reported(self) -> None:
        """The nine counters and the nine scopes are the same nine. Removing a
        scope without removing its counter would let a fence silently stop
        being able to name what it depends on."""
        attempt = replace_once(
            self, repository_read(ATTEMPT_SOURCE), "        case preparedAlignment\n", ""
        )
        errors = audit(read_with(**{ATTEMPT_SOURCE: attempt}))

        self.assertTrue(errors)
        self.assertIn("preparedAlignment", errors[0])
        self.assertIn("Attempt.Scope", errors[0])

    def test_prose_about_a_fence_is_not_a_fence(self) -> None:
        """The migration's own comments quote the conjunctions they replaced,
        and the census must not count a sentence as a comparison."""
        source = replace_once(
            self,
            repository_read(SOURCE),
            "    private func isCurrentLifecycle(_ generation: Int) -> Bool {",
            "    // Once upon a time this read `openGeneration == generation`.\n"
            "    /// And `self.viewerActionEpoch == actionEpoch` beside it.\n"
            "    /// Now `attempt.stillCurrent(now, scopes: [.open])` via attemptStillCurrent(a, fence: .stallRecovery).\n"
            "    private func isCurrentLifecycle(_ generation: Int) -> Bool {",
        )
        self.assertEqual(audit(read_with(**{SOURCE: source})), ())


if __name__ == "__main__":
    unittest.main()
