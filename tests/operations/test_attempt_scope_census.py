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
audit is exercised against the shape of the code it actually guards.
"""

from __future__ import annotations

import unittest

from validation import attempt_census
from validation.attempt_census import ALLOWLIST, ATTEMPT_SOURCE, SOURCE, audit, repository_read


def read_with(**overrides):
    """A `read` that substitutes text for the named repository paths."""

    def read(path: str) -> str:
        if path in overrides:
            return overrides[path]
        return repository_read(path)

    return read


class AttemptScopeCensusCase(unittest.TestCase):
    def test_the_repository_agrees_with_its_own_census(self) -> None:
        self.assertEqual(audit(repository_read), ())

    def test_every_migrated_fence_names_at_least_one_scope(self) -> None:
        """A `stillCurrent` call with an empty scope set decides nothing."""
        sets = attempt_census.named_scope_sets(repository_read(SOURCE))
        self.assertTrue(sets, "no fence has been migrated to Attempt.stillCurrent at all")
        for named in sets:
            self.assertTrue(named)

    def test_a_new_hand_written_fence_is_reported(self) -> None:
        """The case the census exists for: a later `Task {}` that compares an
        epoch by hand instead of naming what it depends on."""
        source = repository_read(SOURCE).replace(
            "    private func isCurrentLifecycle(_ generation: Int) -> Bool {",
            "    private func speculativeWork(_ generation: Int) -> Bool {\n"
            "        openGeneration == generation\n"
            "    }\n\n"
            "    private func isCurrentLifecycle(_ generation: Int) -> Bool {",
            1,
        )
        errors = audit(read_with(**{SOURCE: source}))

        self.assertEqual(len(errors), 1, errors)
        self.assertIn("openGeneration == generation", errors[0])
        self.assertIn("speculativeWork()", errors[0])
        self.assertIn("Attempt.stillCurrent(_:scopes:)", errors[0])

    def test_reverting_a_migrated_fence_is_reported(self) -> None:
        """Restoring the conjunction the stall-recovery fence used to carry
        puts two comparisons back into a function the allow-list now says has
        none, and the census says so for each."""
        source = repository_read(SOURCE).replace(
            "        guard stallAttempt.stillCurrent(snapshotAttempt(), "
            "scopes: [.open, .viewerAction]),",
            "        guard openGeneration == generation,\n"
            "              viewerActionEpoch == actionEpoch,",
            1,
        )
        errors = audit(read_with(**{SOURCE: source}))

        self.assertEqual(len(errors), 2, errors)
        joined = "\n".join(errors)
        self.assertIn("retrySameDeliveryAfterStall()", joined)
        self.assertIn("openGeneration == generation", joined)
        self.assertIn("viewerActionEpoch == actionEpoch", joined)

    def test_a_second_copy_of_an_excused_fence_is_reported(self) -> None:
        """The allow-list excuses a count, not a licence to repeat it."""
        source = repository_read(SOURCE).replace(
            "    private func isSuperseded(_ generation: Int) -> Bool "
            "{ generation != openGeneration }",
            "    private func isSuperseded(_ generation: Int) -> Bool "
            "{ generation != openGeneration }\n"
            "    private func isSupersededTwice(_ generation: Int) -> Bool "
            "{ generation != openGeneration }",
            1,
        )
        source = source.replace("isSupersededTwice", "isSuperseded", 1)
        errors = audit(read_with(**{SOURCE: source}))

        self.assertEqual(len(errors), 1, errors)
        self.assertIn("appears 2 time(s)", errors[0])
        self.assertIn("allows 1", errors[0])

    def test_an_allow_list_row_that_outlived_its_fence_is_reported(self) -> None:
        allowlist = repository_read(ALLOWLIST) + (
            '"aFunctionThatDoesNotExist::openGeneration == generation" = 1\n'
        )
        errors = audit(read_with(**{ALLOWLIST: allowlist}))

        self.assertEqual(len(errors), 1, errors)
        self.assertIn("aFunctionThatDoesNotExist", errors[0])
        self.assertIn("Lower the row", errors[0])

    def test_a_scope_the_attempt_type_does_not_declare_is_reported(self) -> None:
        source = repository_read(SOURCE).replace(
            "scopes: [.open, .viewerAction, .seek]",
            "scopes: [.open, .viewerAction, .sesk]",
            1,
        )
        errors = audit(read_with(**{SOURCE: source}))

        self.assertEqual(len(errors), 1, errors)
        self.assertIn("`.sesk`", errors[0])

    def test_dropping_a_scope_from_the_attempt_type_is_reported(self) -> None:
        """The nine counters and the nine scopes are the same nine. Removing a
        scope without removing its counter would let a fence silently stop
        being able to name what it depends on."""
        attempt = repository_read(ATTEMPT_SOURCE).replace(
            "        case preparedAlignment\n", "", 1
        )
        errors = audit(read_with(**{ATTEMPT_SOURCE: attempt}))

        self.assertTrue(errors)
        self.assertIn("preparedAlignment", errors[0])
        self.assertIn("Attempt.Scope", errors[0])

    def test_prose_about_a_fence_is_not_a_fence(self) -> None:
        """The migration's own comments quote the conjunctions they replaced,
        and the census must not count a sentence as a comparison."""
        source = repository_read(SOURCE).replace(
            "    private func isCurrentLifecycle(_ generation: Int) -> Bool {",
            "    // Once upon a time this read `openGeneration == generation`.\n"
            "    /// And `self.viewerActionEpoch == actionEpoch` beside it.\n"
            "    private func isCurrentLifecycle(_ generation: Int) -> Bool {",
            1,
        )
        self.assertEqual(audit(read_with(**{SOURCE: source})), ())


if __name__ == "__main__":
    unittest.main()
