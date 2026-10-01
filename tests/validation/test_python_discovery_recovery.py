"""Synthetic API fixtures only; original private CI blobs are not tracked."""
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

from validation import python_unit_receipts as receipts


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def archive(document):
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as zipped:
        zipped.writestr("receipt.json", json.dumps(document, sort_keys=True))
    return output.getvalue()


class SyntheticRecoveryAPI:
    def __init__(self, root):
        self.scope = {"repository": 7, "pr": 42, "branch": "codex/synthetic", "base": "effort/example"}
        self.commit, self.run, self.job = "a" * 40, 10, 20
        proof = {"version": 1, "repository": 7, "branch": self.scope["branch"], "suite": "operations",
                 "test_file": "tests/operations/test_fixture.py", "test_file_sha256": "b" * 64,
                 "command": "synthetic four passing fixtures", "output": "synthetic OK",
                 "test_ids": [f"test_fixture.Case.test_{i}" for i in range(4)],
                 "result": {"failed": 0, "errors": 0, "skipped": 0, "expected_failures": 0,
                            "unexpected_successes": 0, "count": 4}}
        raw = json.dumps(proof).encode()
        receipt_hash = digest(raw)
        directory = root / "validation/python-unit-local"
        directory.mkdir(parents=True, exist_ok=True)
        (directory / (receipt_hash + ".json")).write_bytes(raw)
        self.comment = {"id": 99, "user": {"id": 2, "login": "fixture_writer"},
                        "body": "Python-Unit-Receipt: " + json.dumps({"repository": 7, "pr": 42,
                            "suite": "operations", "sha256": receipt_hash})}
        self.baseline = {"operations:" + identity: {"commit": None, "run": None,
            "provenance": f"attested-local:{receipt_hash}:{'b' * 64}:comment-99"} for identity in proof["test_ids"]}
        self.start = {"version": 1, "scope": self.scope, "run": self.run, "commit": self.commit,
                      "complete": False, "fixture_errors": [], "passes": self.baseline}
        self.final = copy.deepcopy(self.start)
        self.ids = [f"test_fixture.Validation.test_{i}" for i in range(3)]
        self.final["passes"].update({"validation:" + identity: {"commit": self.commit, "run": self.run}
                                     for identity in self.ids})
        self.log = (self.commit + "\n" + "\n".join(f"test_{i} ({identity}) ... ok"
            for i, identity in enumerate(self.ids)) + "\nRan 3 tests in 0.001s\nOK\n"
            "Python receipt refusal: ReceiptError: Unit discovery failed; no cached pass may hide import errors\n"
            "validation: discovered=3, historical-passes=0, pending=3\n").encode()
        self.sources = {".github/workflows/effort-ci.yml": b"synthetic workflow ordering",
                        "validation/python_unit_receipts.py": b"synthetic original discovery ordering"}
        self.case = {"version": 1, "scope": self.scope, "run": self.run, "job": self.job,
                     "commit": self.commit, "count": 3, "local_count": 4,
                     "summary": "Ran 3 tests in 0.001s", "source_hashes":
                     {path: digest(data) for path, data in self.sources.items()}}
        self.actual = {"id": self.run, "repository": {"id": 7}, "commit_sha": self.commit,
                       "prettyref": self.scope["branch"], "workflow_id": "effort-ci.yml", "status": "failure"}
        self.jobs = [{"id": self.job, "run_id": self.run, "repo_id": 7, "attempt": 1,
                      "name": receipts.job_name(self.scope), "status": "failure"}]
        self.rebind()

    def rebind(self):
        self.start_raw, self.final_raw = archive(self.start), archive(self.final)
        self.marker = {"id": 30, "run_id": self.run, "name": receipts.key(self.scope) + "-start-10",
                       "expired": False, "size_in_bytes": len(self.start_raw)}
        self.artifact = {"id": 31, "run_id": self.run, "name": receipts.key(self.scope),
                         "expired": False, "size_in_bytes": len(self.final_raw)}
        self.case.update(start={"id": 30, "bytes": len(self.start_raw), "sha256": digest(self.start_raw)},
                         final={"id": 31, "bytes": len(self.final_raw), "sha256": digest(self.final_raw)},
                         log_sha256=digest(self.log))

    def pages(self, path, query=None, field=None):
        if path == "/actions/artifacts":
            return [self.marker] if "-start-" in query["name"] else [self.artifact]
        if path == "/actions/runs":
            return [{"id": self.run, "commit_sha": self.commit}]
        if path.endswith("/jobs"):
            return self.jobs
        if path == "/issues/42/comments":
            return [self.comment]
        raise AssertionError(path)

    def get(self, path):
        if path == "/actions/runs/10":
            return self.actual
        if path == "/collaborators/fixture_writer/permission":
            return {"user": {"id": 2}, "permission": "write"}
        raise AssertionError(path)

    def bytes(self, path, query=None):
        if path == "/actions/artifacts/30/zip":
            return self.start_raw
        if path == "/actions/artifacts/31/zip":
            return self.final_raw
        if path == "/actions/jobs/20/logs":
            return self.log
        if path.startswith("/raw/"):
            return self.sources[path.removeprefix("/raw/")]
        raise AssertionError(path)


class DiscoveryRecoveryTests(unittest.TestCase):
    def restore(self, api):
        with patch.object(receipts, "discovery_recovery_case", return_value=api.case), \
             patch("sys.stdout", new=io.StringIO()):
            return receipts.restore(api, api.scope, 11)

    def test_actual_restore_retains_positive_ids_and_preflights_all_suites(self):
        with tempfile.TemporaryDirectory() as directory:
            previous = Path.cwd()
            try:
                os.chdir(directory)
                api = SyntheticRecoveryAPI(Path(directory))
                restored = self.restore(api)
                self.assertEqual(restored, api.final["passes"])
                self.assertFalse(api.final["complete"])
                self.assertEqual({k: restored[k] for k in api.baseline}, api.baseline)
                events = []
                class NeverRun(unittest.TestCase):
                    def runTest(self):
                        events.append("unexpected-method")
                def discovery(name):
                    if name == "operations":
                        raise receipts.ReceiptError("synthetic later discovery failure")
                    return [NeverRun()]
                state = {**copy.deepcopy(api.start), "passes": {}}
                with patch.object(receipts, "discover", side_effect=discovery), \
                     patch("sys.stderr", new=io.StringIO()), patch("sys.stdout", new=io.StringIO()):
                    with self.assertRaises(receipts.ReceiptError):
                        receipts.execute(state, Path(directory) / "current.json")
                self.assertEqual(events, [])
                self.assertEqual(state["passes"], {})
                self.assertFalse(state["complete"])
            finally:
                os.chdir(previous)

    def test_partial_recovery_refuses_tamper_ambiguity_and_unapproved_attempts(self):
        modes = ("zip", "marker", "job", "run", "source", "scope", "unknown", "local",
                 "provenance", "duplicate", "missing_ok", "operations", "order", "fixture")
        with tempfile.TemporaryDirectory() as directory:
            previous = Path.cwd()
            try:
                os.chdir(directory)
                for mode in modes:
                    with self.subTest(mode=mode):
                        api = SyntheticRecoveryAPI(Path(directory))
                        if mode == "zip":
                            api.final_raw += b"tampered-trailer"
                        elif mode == "marker":
                            api.marker["size_in_bytes"] += 1
                        elif mode == "job":
                            api.jobs[0]["attempt"] = 2
                        elif mode == "run":
                            api.actual["status"] = "success"
                        elif mode == "source":
                            api.sources["validation/python_unit_receipts.py"] = b"other-source"
                        elif mode == "scope":
                            api.case["scope"] = {**api.scope, "pr": 43}
                        elif mode == "unknown":
                            api.case["run"] = 9
                        elif mode == "local":
                            api.start["passes"][next(iter(api.baseline))]["provenance"] = "attested-local:foreign"
                            api.rebind()
                        elif mode == "provenance":
                            api.final["passes"]["validation:" + api.ids[0]]["run"] = 9
                            api.rebind()
                        elif mode == "duplicate":
                            api.log = api.log.replace(api.ids[1].encode(), api.ids[0].encode())
                            api.rebind()
                        elif mode == "missing_ok":
                            api.log = api.log.replace(b" ... ok", b" ... FAIL", 1)
                            api.rebind()
                        elif mode == "operations":
                            api.log += b"test_extra (test_ops.Case.test_extra) ... ok\noperations: discovered=1\n"
                            api.rebind()
                        elif mode == "order":
                            api.log = api.log.replace(b"\nOK\n", b"\n").replace(api.commit.encode(), b"OK\n" + api.commit.encode())
                            api.rebind()
                        elif mode == "fixture":
                            api.final["fixture_errors"] = ["validation:tearDownClass"]
                            api.rebind()
                        with self.assertRaises(receipts.ReceiptError):
                            self.restore(api)
            finally:
                os.chdir(previous)


if __name__ == "__main__":
    unittest.main()
