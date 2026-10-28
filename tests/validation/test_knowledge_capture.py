"""Advisory PR-body parsing and bounded diff selection; no qualification claims."""
import os
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest

from validation.knowledge import KnowledgeError
from validation.knowledge.capture import changed_paths
from validation.knowledge.lint import inspect_body
from validation.knowledge.snapshot import SnapshotChanged

# Paths below belong to disposable repositories, not this documentation tree.
FIXTURE_DOC_ROOT = "docs"


def sample_graph():
    """Small data-only graph for the capture boundary, not schema validation."""
    nodes = (
        SimpleNamespace(id="plurx:obligation:fixture-open", kind="obligation",
                        attributes={"disposition": "open"}, scope=("src/**",)),
        SimpleNamespace(id="plurx:obligation:fixture-awaiting", kind="obligation",
                        attributes={"disposition": "implemented-awaiting-evidence"},
                        scope=(f"{FIXTURE_DOC_ROOT}/**",)),
        SimpleNamespace(id="plurx:obligation:fixture-closed", kind="obligation",
                        attributes={"disposition": "satisfied"}, scope=("src/**",)),
        SimpleNamespace(id="plurx:decision:fixture-choice", kind="decision",
                        attributes={}, scope=()),
    )
    return SimpleNamespace(nodes=nodes)


class KnowledgeCaptureTests(unittest.TestCase):
    def test_absent_body_is_unchecked_and_keeps_curated_obligations_visible(self):
        result = inspect_body(sample_graph(), None, ("src/route.rs", f"{FIXTURE_DOC_ROOT}/guide.md"))

        self.assertEqual(result["state"], "unchecked")
        self.assertEqual(result["matched_open_obligations"], [
            "plurx:obligation:fixture-awaiting", "plurx:obligation:fixture-open"])
        self.assertFalse(result["acknowledgment_proves_satisfaction"])
        self.assertEqual(result["acknowledged"], [])

    def test_empty_body_is_advisory_and_missing_acknowledgment_does_not_hide_scope(self):
        result = inspect_body(sample_graph(), "", ("src/route.rs",))

        self.assertEqual(result["state"], "advisory")
        self.assertEqual(result["matched_open_obligations"], ["plurx:obligation:fixture-open"])
        self.assertEqual(result["errors"], [])
        self.assertTrue(any("without Knowledge acknowledgment" in warning
                            for warning in result["warnings"]))

    def test_known_obligation_acknowledgment_does_not_satisfy_it(self):
        result = inspect_body(sample_graph(),
                              "Knowledge-Obligation: plurx:obligation:fixture-open",
                              ("src/route.rs",))

        self.assertEqual(result["errors"], [])
        self.assertEqual(result["acknowledged"], [
            {"kind": "obligation", "id": "plurx:obligation:fixture-open"}])
        self.assertEqual(result["matched_open_obligations"], ["plurx:obligation:fixture-open"])
        self.assertFalse(result["acknowledgment_proves_satisfaction"])

    def test_known_decision_can_be_acknowledged_without_matching_curated_scope(self):
        result = inspect_body(sample_graph(),
                              "Knowledge-Decision: plurx:decision:fixture-choice",
                              ("tests/unrelated.py",))

        self.assertEqual(result["errors"], [])
        self.assertEqual(result["acknowledged"], [
            {"kind": "decision", "id": "plurx:decision:fixture-choice"}])
        self.assertEqual(result["matched_open_obligations"], [])

    def test_unknown_and_wrong_kind_ids_fail_even_when_scope_does_not_match(self):
        cases = (("Knowledge-Obligation: plurx:obligation:missing",
                  "Unknown supplied Knowledge-Obligation ID"),
                 ("Knowledge-Obligation: plurx:decision:fixture-choice",
                  "Invalid supplied Knowledge-Obligation ID."))
        for field, expected_error in cases:
            with self.subTest(field=field):
                result = inspect_body(sample_graph(), field, ("tests/other.py",))
                self.assertEqual(result["acknowledged"], [])
                self.assertEqual(len(result["errors"]), 1)
                self.assertIn(expected_error, result["errors"][0])

    def test_malformed_supplied_id_is_rejected(self):
        result = inspect_body(sample_graph(), "Knowledge-Decision: not-an-id", ())

        self.assertEqual(result["acknowledged"], [])
        self.assertEqual(result["errors"], ["Invalid supplied Knowledge-Decision ID."])

    def test_impact_requires_a_concrete_reason(self):
        for reason in ("", "<reason>", "TODO"):
            with self.subTest(reason=reason):
                result = inspect_body(sample_graph(),
                                      "Knowledge-Impact: reviewed-no-change; " + reason, ())
                self.assertEqual(result["acknowledged"], [])
                self.assertEqual(len(result["errors"]), 1)

    def test_concrete_impact_reason_is_inert_and_leaves_open_obligation_matched(self):
        base = Path.cwd() / "target" / "knowledge-tests"
        base.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="capture-body-",
                                         dir=base) as directory:
            marker = Path(directory) / "must-not-exist"
            reason = "reviewed-no-change; $(touch " + str(marker) + ")"
            result = inspect_body(sample_graph(), "Knowledge-Impact: " + reason,
                                  ("src/route.rs",))
            self.assertFalse(marker.exists())

        self.assertEqual(result["errors"], [])
        self.assertEqual(result["acknowledged"], [
            {"kind": "impact", "reason": "$(touch " + str(marker) + ")"}])
        self.assertEqual(result["matched_open_obligations"], ["plurx:obligation:fixture-open"])
        self.assertFalse(result["acknowledgment_proves_satisfaction"])


class ChangedPathsTests(unittest.TestCase):
    def setUp(self):
        base = Path(os.environ.get("PLURX_TEST_TMPDIR", Path.cwd() / "target" / "knowledge-tests"))
        base.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(prefix="capture-git-", dir=base)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name) / "repo"
        self.root.mkdir()
        self.git("init", "-q")
        self.git("config", "user.name", "Synthetic fixture")
        self.git("config", "user.email", "fixture@localhost")
        self.write("src/first file.txt", "initial one\n")
        self.write("src/second.txt", "initial two\n")
        self.commit("fixture: initial files")
        self.head = self.git("rev-parse", "HEAD")

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.root), *args],
            env={**os.environ, "GIT_OPTIONAL_LOCKS": "0"}, stderr=subprocess.DEVNULL,
            text=True).strip()

    def write(self, relative, contents):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)
        return path

    def commit(self, message):
        self.git("add", ".")
        self.git("commit", "-q", "-m", message)

    def indexed(self, *, untracked=(), verify=None):
        snapshot = SimpleNamespace(head=self.head, untracked=tuple(untracked),
                                   verify=verify or (lambda root: True))
        return SimpleNamespace(snapshot=snapshot)

    def test_full_diff_preserves_rename_members_and_only_selected_untracked_paths(self):
        self.git("mv", "src/first file.txt", "src/renamed file.txt")
        self.write("src/selected new.txt", "selected\n")
        self.write("private-looking.txt", "not selected by snapshot\n")

        paths = changed_paths(self.root,
                              self.indexed(untracked=("src/selected new.txt",)),
                              worktree=True)

        self.assertEqual(paths, ("src/first file.txt", "src/renamed file.txt",
                                 "src/selected new.txt"))
        self.assertNotIn("private-looking.txt", paths)

    def test_staged_diff_excludes_unstaged_only_and_untracked_paths(self):
        self.write("src/first file.txt", "staged update\n")
        self.git("add", "src/first file.txt")
        self.write("src/second.txt", "unstaged update\n")
        self.write("src/untracked.txt", "untracked\n")

        paths = changed_paths(self.root,
                              self.indexed(untracked=("src/untracked.txt",)), staged=True)

        self.assertEqual(paths, ("src/first file.txt",))

    def test_unsafe_or_unknown_base_is_rejected_without_writing_a_file(self):
        marker = self.root / "must-not-be-created"
        for base in ("missing-base", "--output=" + str(marker)):
            with self.subTest(base=base):
                with self.assertRaises(KnowledgeError):
                    changed_paths(self.root, self.indexed(), base=base)
                self.assertFalse(marker.exists())

    def test_snapshot_change_before_or_during_selection_is_refused(self):
        checks = []

        def changes_during_selection(root):
            checks.append(root)
            return len(checks) == 1

        with self.assertRaisesRegex(SnapshotChanged, "before diff selection"):
            changed_paths(self.root, self.indexed(verify=lambda root: False), worktree=True)
        self.assertEqual(checks, [])

        with self.assertRaisesRegex(SnapshotChanged, "during diff selection"):
            changed_paths(self.root, self.indexed(verify=changes_during_selection), worktree=True)
        self.assertEqual(checks, [self.root, self.root])
