"""Small fake suites only: do not execute the repository suites here."""

import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

from validation import python_unit_receipts as receipts


SCOPE = {"repository": 7, "pr": 42, "branch": "codex/fix", "base": "effort/example"}
COMMIT = "a" * 40


def journal():
    return {"version": 1, "scope": dict(SCOPE), "run": 10,
            "commit": COMMIT, "complete": False, "passes": {}}


def Fake(name, events, outcome="pass"):
    class Fixture(unittest.TestCase):
        def id(self):
            return name

        def runTest(self):
            events.append(name)
            if outcome == "fail":
                self.fail("intentional fixture failure")
            if outcome == "skip":
                self.skipTest("intentional fixture skip")
    return Fixture()


class PythonReceiptCase(unittest.TestCase):
    def execute(self, state, suites):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "receipt.json"
            with patch("sys.stderr", new=io.StringIO()), patch("sys.stdout", new=io.StringIO()):
                result = receipts.execute(state, path, suites)
            self.assertEqual(json.loads(path.read_text()), state)
            return result

    def test_failed_only_retry_and_new_id_preserve_original_source(self):
        events, state = [], journal()
        self.assertEqual(self.execute(state, {
            "validation": [Fake("good", events), Fake("bad", events, "fail")],
            "operations": [Fake("ops", events)]}), 1)
        self.assertEqual(events, ["good", "bad", "ops"])
        old = dict(state["passes"]["validation:good"])
        state.update(commit="b" * 40, run=11, complete=False)
        events.clear()
        self.assertEqual(self.execute(state, {
            "validation": [Fake("good", events), Fake("bad", events), Fake("new", events)],
            "operations": [Fake("ops", events)]}), 0)
        self.assertEqual(events, ["bad", "new"])
        self.assertEqual(state["passes"]["validation:good"], old)

    def test_skip_never_counts_as_success(self):
        state, events = journal(), []
        self.assertEqual(self.execute(state, {
            "validation": [Fake("skip", events, "skip")],
            "operations": [Fake("ops", events)]}), 1)
        self.assertNotIn("validation:skip", state["passes"])

    def test_isolation_corruption_and_incomplete_refuse(self):
        state = journal()
        state["complete"] = True
        receipts.validate_journal(state, SCOPE, 10, COMMIT)
        for field in ("repository", "pr", "branch", "base"):
            other = dict(SCOPE)
            other[field] = "wrong"
            with self.assertRaises(receipts.ReceiptError):
                receipts.validate_journal(state, other, 10, COMMIT)
        state["complete"] = False
        with self.assertRaises(receipts.ReceiptError):
            receipts.validate_journal(state, SCOPE, 10, COMMIT)
        with self.assertRaises(receipts.ReceiptError):
            receipts.bounded_json(b'{"passes":{},"passes":{}}')

    def test_archive_paths_and_zero_discovery_refuse(self):
        archive = io.BytesIO()
        with zipfile.ZipFile(archive, "w") as target:
            target.writestr("../receipt.json", "{}")
        with self.assertRaises(receipts.ReceiptError):
            receipts.artifact_json(archive.getvalue())
        with patch("unittest.TestLoader.discover", return_value=unittest.TestSuite()):
            with self.assertRaises(receipts.ReceiptError):
                receipts.discover("validation")

    def test_atomic_publication_and_workflow_serialization(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "receipt.json"
            receipts.atomic_json(path, journal())
            self.assertEqual(json.loads(path.read_text()), journal())
            self.assertEqual([p.name for p in path.parent.iterdir()], ["receipt.json"])
        workflow = (Path(__file__).resolve().parents[2] / ".github/workflows/effort-ci.yml").read_text()
        self.assertNotIn("cancel-in-progress: true", workflow)
        self.assertIn("group: python-unit-${{ needs.scope.outputs.receipt_key }}", workflow)
        self.assertLess(workflow.index("Publish Python attempt-start marker"),
                        workflow.index("python_unit_receipts run"))
        self.assertIn("if: always() && steps.receipts.outcome == 'success'", workflow)

    def test_missing_expired_and_concurrent_attempts_refuse_before_units(self):
        class FakeAPI:
            def __init__(self, mode):
                self.mode = mode

            def pages(self, path, query=None, field=None):
                if path == "/actions/artifacts":
                    if self.mode == "expired":
                        return [{"id": 1, "run_id": 9, "name": receipts.key(SCOPE),
                                 "expired": True, "size_in_bytes": 1}]
                    return []
                if path == "/actions/runs":
                    return [{"id": 9, "commit_sha": COMMIT}]
                return [{"name": receipts.job_name(SCOPE), "repo_id": 7,
                         "attempt": 1, "status": "running" if self.mode == "concurrent" else "failure"}]
        for mode in ("missing", "expired", "concurrent"):
            with self.assertRaises(receipts.ReceiptError):
                receipts.restore(FakeAPI(mode), SCOPE, 10)

    def test_manual_dispatch_rejects_wrong_repo_head_base_and_ambiguity(self):
        class FakeAPI:
            def __init__(self, pulls):
                self.pulls = pulls

            def get(self, path):
                return {"id": 7}

            def pages(self, *args, **kwargs):
                return self.pulls
        valid = {"number": 42, "head": {"ref": "codex/fix", "sha": COMMIT,
                                          "repo": {"id": 7}},
                 "base": {"ref": "effort/example", "repo": {"id": 7}}}
        self.assertEqual(receipts.identity(FakeAPI([valid]), "codex/fix", COMMIT), SCOPE)
        for pulls in ([], [valid, valid]):
            with self.assertRaises(receipts.ReceiptError):
                receipts.identity(FakeAPI(pulls), "codex/fix", COMMIT)
        for field, value in (("sha", "b" * 40), ("repo", {"id": 8})):
            wrong = json.loads(json.dumps(valid))
            wrong["head"][field] = value
            with self.assertRaises(receipts.ReceiptError):
                receipts.identity(FakeAPI([wrong]), "codex/fix", COMMIT)
        wrong = json.loads(json.dumps(valid))
        wrong["base"]["ref"] = "main"
        with self.assertRaises(receipts.ReceiptError):
            receipts.identity(FakeAPI([wrong]), "codex/fix", COMMIT)

    def test_local_receipt_requires_hash_bound_writer_attestation(self):
        import hashlib
        import os
        proof = {"version": 1, "repository": 7, "branch": "codex/fix",
                 "suite": "validation", "test_file": "tests/validation/test_fixture.py",
                 "test_file_sha256": "c" * 64, "test_ids": ["test_fixture.Case.test_ok"],
                 "command": "python3 -m unittest named_fixture", "output": "Ran 1 test; OK",
                 "result": {"failed": 0, "errors": 0, "skipped": 0,
                            "expected_failures": 0, "unexpected_successes": 0, "count": 1}}
        raw = json.dumps(proof).encode()
        digest = hashlib.sha256(raw).hexdigest()
        class FakeAPI:
            def __init__(self, permission="write", attested=True):
                self.permission, self.attested = permission, attested

            def pages(self, *args, **kwargs):
                return [{"id": 5, "user": {"id": 2, "login": "writer"},
                         "body": "Python-Unit-Receipt: " + json.dumps({
                             "repository": 7, "pr": 42, "suite": "validation", "sha256": digest})}] if self.attested else []

            def get(self, path):
                return {"user": {"id": 2}, "permission": self.permission}
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root) / "validation/python-unit-local"
            directory.mkdir(parents=True)
            (directory / (digest + ".json")).write_bytes(raw)
            previous = os.getcwd()
            try:
                os.chdir(root)
                passed = receipts.local_receipts(FakeAPI(), SCOPE)
                self.assertIsNone(passed["validation:test_fixture.Case.test_ok"]["commit"])
                for api in (FakeAPI("read"), FakeAPI(attested=False)):
                    with self.assertRaises(receipts.ReceiptError):
                        receipts.local_receipts(api, SCOPE)
                self.assertEqual(receipts.local_receipts(FakeAPI(), {**SCOPE, "pr": 99,
                                                                  "branch": "codex/other"}), {})
            finally:
                os.chdir(previous)


if __name__ == "__main__":
    unittest.main()
