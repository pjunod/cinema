"""New synthetic exact-case control; no actual API, CI units or replay."""
import copy
import hashlib
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch
import zipfile

from validation import python_unit_interrupted_recovery as recovery
from validation import python_unit_receipts as receipts


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


class SyntheticInterruptedAPI:
    """Real case identities, entirely synthetic source/log/archive bytes."""
    def __init__(self):
        self.proof = json.loads(Path(recovery.__file__).with_name(
            "python-unit-interrupted3972.json").read_bytes())
        self.case_digest = "d" * 64
        self.scope = copy.deepcopy(self.proof["scope"])
        self.prior = {"id": 3972, "commit_sha": recovery.COMMIT}
        self.current_run = 4000
        self.current_commit = "b" * 40
        self.original = {"id": 3972, "repository": {"id": 1}, "commit_sha": recovery.COMMIT,
                         "prettyref": self.scope["branch"], "workflow_id": "effort-ci.yml",
                         "status": "failure"}
        self.candidate = {**self.original, "id": self.current_run, "commit_sha": self.current_commit,
                          "status": "running"}
        self.pull = {"state": "open", "head": {"sha": self.current_commit,
            "ref": self.scope["branch"], "repo": {"id": 1}},
            "base": {"ref": self.scope["base"], "repo": {"id": 1}}}
        self.job = {"id": 41333, "run_id": 3972, "repo_id": 1, "attempt": 1,
                    "task_id": 15382, "name": receipts.job_name(self.scope), "status": "failure"}
        self.legacy = copy.deepcopy(self.proof["baseline"])
        self.start = {"version": 1, "scope": self.scope, "run": 3972,
                      "commit": recovery.COMMIT, "applicability_commit": recovery.COMMIT,
                      "complete": False, "passes": copy.deepcopy(self.legacy), "fixture_errors": []}
        self.sources = {path: ("synthetic source " + path).encode()
                        for path in self.proof["source_hashes"]}
        self.proof["source_hashes"] = {path: digest(raw) for path, raw in self.sources.items()}
        self.marker = {"id": 1650, "run_id": 3972, "name": receipts.key(self.scope) + "-start-3972",
                       "expired": False}
        self.refresh_start()
        self.finals = []
        self.run_artifacts = [self.marker]
        self.permission = "write"
        self.comments = [{"id": 9, "user": {"id": 1, "login": "synthetic-writer"},
            "body": "Python-Journal-Recovery: " + json.dumps(
                {"repository": 1, "pr": 767, "sha256": self.case_digest})}]
        lines = [recovery.COMMIT,
                 f"Artifact {self.marker['name']} has been successfully uploaded! "
                 f"Final size is {self.marker['size_in_bytes']} bytes. Artifact ID is 1650"]
        for suite in ("validation", "operations"):
            for key in self.proof["event_ids"]:
                if not key.startswith(suite + ":"):
                    continue
                identity = key.split(":", 1)[1]
                lines.append(identity.rsplit(".", 1)[1] + " (" + identity + ") ... ok")
            if suite == "validation":
                lines.extend([self.proof["validation_summary"], "OK"])
        interrupted = self.proof["interrupted_id"].split(":", 1)[1]
        lines.extend([interrupted.rsplit(".", 1)[1] + " (" + interrupted + ")",
                      self.proof["deadline"], "Error occurred running finally: context deadline exceeded"])
        self.log = ("\n".join(lines) + "\n").encode()
        self.bind_log()

    def refresh_start(self):
        raw = json.dumps(self.start).encode()
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, "w") as archive:
            archive.writestr("receipt.json", raw)
        self.archive = stream.getvalue()
        self.marker["size_in_bytes"] = len(self.archive)
        self.proof["start"].update(bytes=len(self.archive), zip_sha256=digest(self.archive),
                                   json_sha256=digest(raw))

    def bind_log(self):
        self.proof["log"] = {"bytes": len(self.log), "sha256": digest(self.log)}

    def get(self, path):
        if path == "/actions/runs/3972":
            return self.original
        if path == f"/actions/runs/{self.current_run}":
            return self.candidate
        if path == "/pulls/767":
            return self.pull
        if path == "/actions/runs/3972/artifacts":
            return self.run_artifacts
        if path == "/collaborators/synthetic-writer/permission":
            return {"user": {"id": 1}, "permission": self.permission}
        raise AssertionError(path)

    def pages(self, path, params=None, field=None):
        if path == "/issues/767/comments":
            return self.comments
        if path == "/actions/artifacts":
            return [self.marker] if params["name"] == self.marker["name"] else self.finals
        if path == "/actions/runs":
            return [self.prior]
        if path == "/actions/runs/3972/jobs":
            return [self.job]
        raise AssertionError(path)

    def bytes(self, path, params=None):
        if path == "/actions/artifacts/1650/zip":
            return self.archive
        if path == "/actions/jobs/41333/logs":
            return self.log
        if path.startswith("/raw/"):
            assert params == {"ref": recovery.COMMIT}
            return self.sources[path.removeprefix("/raw/")]
        raise AssertionError(path)


class InterruptedJournalRecoveryCase(unittest.TestCase):
    def test_exact_deadline_recovery_retains_events_and_refuses_ambiguous_evidence(self):
        def recover(api):
            with patch.object(recovery, "interrupted_case", return_value=(api.proof, api.case_digest)):
                return recovery.recover_interrupted_pr767(api, api.scope, api.prior,
                                                         [api.job], api.legacy, api.current_run)

        api = SyntheticInterruptedAPI()
        handled, journal = recover(api)
        self.assertTrue(handled)
        self.assertFalse(journal["complete"])
        self.assertEqual(len(journal["passes"]), 384)
        self.assertEqual(journal["passes"][next(iter(api.legacy))], next(iter(api.legacy.values())))
        self.assertNotIn(api.proof["interrupted_id"], journal["passes"])
        for key in api.proof["event_ids"]:
            self.assertEqual(journal["passes"][key], {"commit": recovery.COMMIT, "run": 3972})
        # Exercise the actual restore integration, without running any suite.
        with patch.object(recovery, "interrupted_case", return_value=(api.proof, api.case_digest)), \
                patch.object(receipts, "local_receipts", return_value=api.legacy):
            self.assertEqual(receipts.restore(api, api.scope, api.current_run), journal["passes"])

        mutations = {
            "original metadata": lambda a: a.original.update(status="success"),
            "rerun": lambda a: a.job.update(attempt=2),
            "task identity": lambda a: a.job.update(task_id=15383),
            "current head": lambda a: a.pull["head"].update(sha="c" * 40),
            "unauthenticated": lambda a: a.comments.clear(),
            "reader attestor": lambda a: setattr(a, "permission", "read"),
            "source changed": lambda a: a.sources.update({"validation/python_unit_receipts.py": b"changed"}),
            "log hash": lambda a: setattr(a, "log", a.log + b"changed\n"),
            "expired start": lambda a: a.marker.update(expired=True),
            "unexpected final": lambda a: a.finals.append({"run_id": 3972}),
            "inherited map": lambda a: a.legacy.clear(),
            "fixture error": lambda a: (a.start["fixture_errors"].append("operations:fixture"), a.refresh_start()),
            "start complete": lambda a: (a.start.update(complete=True), a.refresh_start()),
            "truncated rebound log": lambda a: (setattr(a, "log", a.log.split(
                a.proof["deadline"].encode())[0]), a.bind_log()),
            "duplicate rebound header": lambda a: (setattr(a, "log", a.log.replace(
                b") ... ok\n", b") ... ok\n" + a.log.splitlines()[2] + b"\n", 1)), a.bind_log()),
            "unknown rebound outcome": lambda a: (setattr(a, "log", a.log.replace(
                b") ... ok\n", b") ... ERROR\n", 1)), a.bind_log()),
            "dropped rebound outcome": lambda a: (setattr(a, "log", a.log.replace(
                b") ... ok\n", b")\n", 1)), a.bind_log()),
        }
        for label, mutate in mutations.items():
            with self.subTest(refusal=label):
                changed = SyntheticInterruptedAPI()
                mutate(changed)
                with self.assertRaises(receipts.ReceiptError):
                    recover(changed)
        unrelated = dict(api.scope, pr=768)
        self.assertEqual(recovery.recover_interrupted_pr767(None, unrelated, api.prior,
                                                          [], {}, api.current_run), (False, None))
