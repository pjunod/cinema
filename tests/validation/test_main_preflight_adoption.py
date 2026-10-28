"""Per-ID preflight journal: history never refuses, adoption stays authenticated."""

import contextlib
import copy
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock
import zipfile

from validation import main_preflight_adoption as adoption
from validation.main_preflight_adoption import terminal_status
from validation.python_unit_receipts import ReceiptError

SCOPE = {"repository": 1, "pr": 7}
REPO = {"id": 1, "full_name": "owner/repository"}
ENV = {"platform": "linux", "python": [3, 12], "machine": "x86_64", "node": "v22.23.2"}
NAME = "main-preflight-v1-r1-pr7"
CURRENT, X, Y, Z = "c" * 40, "a" * 40, "b" * 40, "d" * 40
DIGEST = "e" * 64
ONE, TWO, THREE = ("node:" + script for script in adoption.NODE[:3])
FAMILIES = [{"id_prefix": identity, "inputs": ["tests/web/fixture.js"], "reason": "fixture"}
            for identity in (ONE, TWO, THREE)]
WORKFLOW = "jobs:\n  preflight:\n    steps:\n" + "".join("      - run: " + command + "\n" for command in (
    "python3 -m validation.main_unit_receipts prepare",
    "python3 -m validation.main_unit_receipts run --suite-dir tests/validation --suite-dir tests/operations",
    "python3 -m validation.main_preflight_adoption node"))


def zipped(value):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        archive.writestr("receipt.json", json.dumps(value))
    return buffer.getvalue()


class Ledger:
    """Forgejo metadata, artifacts, logs and git objects for one PR."""

    def __init__(self):
        self.runs, self.jobs, self.artifacts, self.blobs, self.logs = {}, {}, [], {}, {}
        self.gone = set()
        self.jobs[99] = [{"id": 990, "run_id": 99, "repo_id": 1, "attempt": 1, "name": adoption.JOB,
                          "status": "running"}]

    def run(self, rid, commit, own=(), inherited=None, skipped=()):
        pull = {"number": 7, "draft": False, "state": "open", "head": {"repo": REPO, "sha": commit},
                "base": {"repo": REPO, "ref": "main"}}
        self.runs[rid] = {"id": rid, "repository": REPO, "commit_sha": commit, "workflow_id": "main-fast-lane.yml",
                          "event": "pull_request", "event_payload": json.dumps(
                              {"number": 7, "repository": REPO, "pull_request": pull, "action": "synchronize"})}
        job = {"id": rid * 10, "run_id": rid, "repo_id": 1, "attempt": 1, "name": adoption.JOB, "status": "failure"}
        self.jobs[rid] = [job]
        records = {identity: {"commit": commit, "run": rid, "job": job["id"], "attempt": 1,
                              "outcome": "success", "environment": ENV, "inputs": DIGEST} for identity in own}
        skips = {identity: {**record, "outcome": "skipped"} for identity, record in
                 ((identity, {"commit": commit, "run": rid, "job": job["id"], "attempt": 1,
                              "environment": ENV, "inputs": DIGEST}) for identity in skipped)}
        start = {"version": 1, "scope": SCOPE, "run": rid, "commit": commit, "job": job["id"], "attempt": 1,
                 "environment": ENV, "producer_blob": "producer-" + commit, "manifest_blob": "manifest-" + commit,
                 "outcomes": dict(inherited or {}), "skips": {}, "phase_errors": []}
        final = dict(start, outcomes={**start["outcomes"], **records}, skips=skips)
        start_artifact = self.publish(f"{NAME}-start-{rid}", rid, start)
        self.publish(NAME, rid, final)
        self.logs[rid] = [f"{commit}:refs/remotes/pull/7/head",
                          f"Artifact {start_artifact['name']} has been successfully uploaded!",
                          *(adoption.outcome_event(identity, record) for identity, record in records.items()),
                          *(adoption.outcome_event(identity, record) for identity, record in skips.items()),
                          f"Artifact {NAME} has been successfully uploaded!"]
        return final

    def publish(self, name, rid, value):
        identity = len(self.blobs) + 1
        self.blobs[identity] = value
        self.artifacts.append({"id": identity, "name": name, "run_id": rid, "expired": False})
        return self.artifacts[-1]

    def final(self, rid):
        return self.blobs[[item for item in self.artifacts if item["name"] == NAME and item["run_id"] == rid][0]["id"]]

    def start(self, rid):
        return self.blobs[[item for item in self.artifacts if item["name"] == f"{NAME}-start-{rid}"][0]["id"]]

    def get(self, path):
        return copy.deepcopy(self.runs[int(path.split("/")[-1])])

    def pages(self, path, query=None, field=None):
        if path == "/actions/artifacts":
            return [dict(item) for item in self.artifacts if item["name"] == query["name"]]
        return copy.deepcopy(self.jobs[int(path.split("/")[3])])

    def bytes(self, path, query=None):
        identity = int(path.split("/")[3])
        if path.endswith("/zip"):
            return zipped(self.blobs[identity])
        return "\n".join(self.logs[identity // 10]).encode()

    def git(self, *arguments):
        if any(commit in argument for commit in self.gone for argument in arguments):
            raise subprocess.CalledProcessError(128, ["git", *arguments])
        if arguments[0] == "show" and arguments[1].endswith("main-fast-lane.yml"):
            return WORKFLOW
        if arguments[0] == "show" and arguments[1].endswith(str(adoption.MANIFEST)):
            return json.dumps({"version": 1, "families": getattr(self, "families", FAMILIES)})
        if arguments[0] == "rev-parse":
            commit, path = arguments[1].split(":", 1)
            return ("manifest-" if path == str(adoption.MANIFEST) else "producer-") + commit
        if arguments[0] == "ls-tree":
            return "tests/validation/test_fixture.py"
        return ""


def prepare(ledger, families=FAMILIES, available=None):
    """Run the real prepare against the ledger; return (adopted, journal, output)."""
    output = io.StringIO()
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        (root / "inputs.json").write_text(json.dumps({"version": 1, "families": families}))
        checks = (mock.patch.object(adoption, "commit_available", side_effect=available)
                  if available is not None else
                  mock.patch.object(adoption, "commit_available",
                                    side_effect=lambda commit: commit not in ledger.gone))
        with mock.patch.object(adoption, "api_context", return_value=(ledger, SCOPE, CURRENT, 99)), \
             mock.patch.object(adoption, "environment", return_value=ENV), \
             mock.patch.object(adoption, "git", side_effect=ledger.git), \
             mock.patch.object(adoption, "input_digest", return_value=DIGEST), \
             mock.patch.object(adoption, "MANIFEST", root / "inputs.json"), \
             mock.patch.object(adoption, "JOURNAL", root / "receipts" / "receipt.json"), \
             mock.patch.dict(os.environ, {"GITHUB_OUTPUT": str(root / "output")}), \
             checks, contextlib.redirect_stdout(output):
            adopted = adoption.prepare()
            journal = json.loads((root / "receipts" / "receipt.json").read_text())
    return adopted, journal, output.getvalue()


class MainPreflightAdoptionCase(unittest.TestCase):
    def test_prepare_crash_without_any_journal_contributes_nothing_and_does_not_block(self):
        ledger = Ledger()
        ledger.run(10, X, own=[ONE])
        # Run 11 crashed in prepare: it has run/job metadata and nothing else.
        ledger.runs[11] = dict(ledger.runs[10], id=11, commit_sha=Y)
        ledger.jobs[11] = [dict(ledger.jobs[10][0], id=110, run_id=11)]
        adopted, journal, _ = prepare(ledger)
        self.assertEqual(set(adopted), {ONE})
        self.assertEqual(journal["outcomes"], adopted)

    def test_force_pushed_journal_commit_is_not_adopted_and_does_not_block(self):
        # PR #997: run A published at X, X left the checkout, run B crashed in
        # prepare (`git show X:...` raised), and run C must start normally.
        gone = "f" * 40
        self.assertFalse(adoption.commit_available(gone))  # real git cat-file
        ledger = Ledger()
        ledger.run(10, gone, own=[ONE, TWO])
        ledger.gone.add(gone)
        ledger.runs[11] = dict(ledger.runs[10], id=11)
        adopted, journal, output = prepare(ledger, available=adoption.commit_available)
        self.assertEqual((adopted, journal["outcomes"]), ({}, {}))
        self.assertIn(f"run 10: preflight journal not adopted — commit {gone} is not available", output)
        # Even if the availability check were skipped, the git failure itself
        # means "not adopted" rather than the crash PR #997 hit.
        adopted, _, output = prepare(ledger, available=lambda commit: True)
        self.assertEqual(adopted, {})
        self.assertIn("run 10: preflight journal not adopted — CalledProcessError: Command", output)

    def test_inherited_outcome_is_kept_only_when_its_originating_journal_is_adopted(self):
        ledger = Ledger()
        older = ledger.run(9, Z, own=[THREE])
        gone = ledger.run(10, X, own=[ONE])
        ledger.gone.add(X)
        ledger.run(11, Y, own=[TWO], inherited={ONE: gone["outcomes"][ONE], THREE: older["outcomes"][THREE]})
        adopted, _, output = prepare(ledger)
        self.assertEqual(set(adopted), {TWO, THREE})
        self.assertEqual(adopted[THREE]["run"], 9)
        self.assertIn("run 11: 1 inherited outcomes not adopted", output)

    def test_each_authentication_check_alone_prevents_adoption_and_never_blocks(self):
        def final(ledger, **changes):
            ledger.final(11).update(changes)
        def both(ledger, **changes):
            ledger.final(11).update(changes)
            ledger.start(11).update(changes)
        def record(ledger, **changes):
            ledger.final(11)["outcomes"][TWO].update(changes)
        def logged(ledger, **changes):
            record(ledger, **changes)
            ledger.logs[11][2] = adoption.outcome_event(TWO, ledger.final(11)["outcomes"][TWO])
        def log(ledger, old, new):
            ledger.logs[11] = [line.replace(old, new) for line in ledger.logs[11]]
        mutations = {
            "final artifact expired": lambda l: [item.update(expired=True) for item in l.artifacts
                                                 if item["name"] == NAME and item["run_id"] == 11],
            "two final journals": lambda l: l.publish(NAME, 11, l.final(11)),
            "start journal missing": lambda l: l.artifacts.remove(
                [item for item in l.artifacts if item["name"] == f"{NAME}-start-11"][0]),
            "start journal for another job": lambda l: l.start(11).update(job=111),
            "start publication absent": lambda l: log(l, f"{NAME}-start-11 has been", "start was not"),
            "final publication duplicated": lambda l: l.logs[11].append(l.logs[11][-1]),
            "own outcome event missing": lambda l: l.logs[11].pop(2),
            "PR checkout absent": lambda l: log(l, "refs/remotes/pull/7/head", "refs/remotes/pull/8/head"),
            "run from another repository": lambda l: l.runs[11].update(repository={"id": 2, "full_name": "x/y"}),
            "draft PR event": lambda l: l.runs[11].update(event_payload=l.runs[11]["event_payload"].replace(
                '"draft": false', '"draft": true')),
            "re-run job": lambda l: l.jobs[11][0].update(attempt=2),
            "job still running": lambda l: l.jobs[11][0].update(status="running"),
            "second preflight job": lambda l: l.jobs[11].append(dict(l.jobs[11][0], id=111)),
            "unknown producer": lambda l: both(l, producer_blob="producer-" + X),
            "unknown manifest": lambda l: both(l, manifest_blob="manifest-" + X),
            "other runtime": lambda l: both(l, environment=dict(ENV, node="v20.0.0")),
            "phase error recorded": lambda l: both(l, phase_errors=[{"phase": "node", "error": "aborted"}]),
            "journal for another run": lambda l: final(l, run=12),
            # These two rewrite the outcome event as well, so only the record
            # origin and declared-input checks stand between them and adoption.
            "own record from another job": lambda l: logged(l, job=111),
            "own record with other inputs": lambda l: logged(l, inputs="0" * 64),
            "own skip from another job": lambda l: (l.final(11)["skips"][THREE].update(job=111),
                                                    l.logs[11].__setitem__(3, adoption.outcome_event(
                                                        THREE, l.final(11)["skips"][THREE]))),
            "own skip event missing": lambda l: l.logs[11].pop(3),
            "start journal with an unknown field": lambda l: l.start(11).update(unknown=True),
            "start journal with a malformed skip": lambda l: l.start(11)["skips"].update({THREE: {}}),
            "final journal is not a mapping": lambda l: l.blobs.__setitem__(
                [item for item in l.artifacts if item["name"] == NAME and item["run_id"] == 11][0]["id"], []),
            "skip recorded as success": lambda l: record(l, outcome="skipped"),
            "start positive dropped from final": lambda l: l.start(11)["outcomes"].update(
                {THREE: dict(l.final(11)["outcomes"][TWO])}),
            "commit not available": lambda l: l.gone.add(Y),
        }
        baseline = Ledger()
        baseline.run(10, X, own=[ONE])
        baseline.run(11, Y, own=[TWO], skipped=[THREE])
        self.assertEqual(set(prepare(baseline)[0]), {ONE, TWO})
        self.assertEqual(set(prepare(baseline)[1]["skips"]), {THREE})
        for name, mutation in mutations.items():
            ledger = copy.deepcopy(baseline)
            mutation(ledger)
            with self.subTest(check=name):
                adopted, _, output = prepare(ledger)
                self.assertEqual(set(adopted), {ONE})
                self.assertIn("run 11: preflight journal not adopted — ", output)

    def test_unwitnessed_or_changed_candidate_runs_instead_of_refusing(self):
        ledger = Ledger()
        ledger.run(10, X, own=[ONE, TWO])
        adopted, _, output = prepare(ledger, families=FAMILIES[1:])
        self.assertEqual(set(adopted), {TWO})
        self.assertIn(f"Not adopted: {ONE} — Missing reviewed input witness", output)

    def test_current_run_integrity_still_refuses(self):
        ledger = Ledger()
        ledger.run(10, X, own=[ONE])
        ledger.publish(NAME, 99, ledger.final(10))
        with self.assertRaisesRegex(ReceiptError, "Current run already has a journal"):
            prepare(ledger)
        ledger.artifacts.pop()
        ledger.jobs[99][0]["attempt"] = 2
        with self.assertRaisesRegex(ReceiptError, "Current job identity unavailable"):
            prepare(ledger)

    def test_current_source_integrity_failure_during_candidate_filtering_refuses(self):
        unit = "validation:test_fixture.Case.test_one"
        families = FAMILIES + [{"id_prefix": unit, "inputs": ["tests/validation/test_fixture.py"], "reason": "unit"}]
        ledger = Ledger()
        ledger.families = families
        ledger.run(10, X, own=[unit])
        class Source:
            error = adoption.IntegrityError("Current unit worktree differs from dispatched Git source")
            def __init__(self, commit): pass
            def fingerprint(self, commit, identity): raise self.error
            def finish(self, passes): pass
        with mock.patch.object(adoption, "SourceApplicability", Source):
            with self.assertRaisesRegex(adoption.IntegrityError, "worktree differs"):
                prepare(ledger, families=families)
            Source.error = ReceiptError("Unsupported dynamic unit discovery")
            adopted, _, output = prepare(ledger, families=families)
        self.assertEqual(adopted, {})
        self.assertIn(f"Not adopted: {unit} — Unsupported dynamic unit discovery", output)

    def test_unlisted_history_adopts_nothing_and_names_unexpected_error_types(self):
        ledger = Ledger()
        ledger.run(10, X, own=[ONE])
        listing = ledger.pages
        def broken(path, query=None, field=None):
            if path == "/actions/artifacts" and query["name"] == NAME:
                raise ReceiptError("Evidence pagination limit reached")
            return listing(path, query, field)
        ledger.pages = broken
        adopted, journal, output = prepare(ledger)
        self.assertEqual((adopted, journal["outcomes"]), ({}, {}))
        self.assertIn("Prior preflight journals unavailable — Evidence pagination limit reached", output)
        ledger = Ledger()
        ledger.run(10, X, own=[ONE])
        ledger.final(10)["outcomes"][ONE] = 5  # a record that is not a mapping
        adopted, _, output = prepare(ledger)
        self.assertEqual(adopted, {})
        self.assertRegex(output, r"run 10: preflight journal not adopted — (TypeError|AttributeError): ")
        ledger = Ledger()
        ledger.run(10, X, own=[ONE])
        def truncated(path, query=None):
            raise adoption.http.client.IncompleteRead(b"partial", 10)
        ledger.bytes = truncated
        adopted, _, output = prepare(ledger)
        self.assertEqual(adopted, {})
        self.assertIn("run 10: preflight journal not adopted — IncompleteRead", output)

    def test_split_receipt_workflow_has_one_python_executor_and_distinct_node_receipts(self):
        commands = ['python3 -m validation.main_unit_receipts prepare',
                    'python3 -m validation.main_unit_receipts run --suite-dir tests/validation --suite-dir tests/operations',
                    'python3 -m validation.main_preflight_adoption node']
        self.assertTrue(adoption.adapter_workflow(commands))
        for removed in commands:
            self.assertFalse(adoption.adapter_workflow([command for command in commands if command != removed]))
        self.assertTrue(adoption.adapter_workflow(
            ['python3 -m validation.main_preflight_adoption ' + phase
             for phase in ('prepare', 'validation', 'operations', 'node')]))


    def test_conflicting_status_aliases_refuse(self):
        self.assertEqual(terminal_status({"state": "success"}), "success")
        with self.assertRaises(ReceiptError):
            terminal_status({"state": "success", "status": "failure"})

    def test_content_and_pathset_changes_are_distinct_declared_inputs(self):
        old, new = "a" * 40, "b" * 40
        saved = dict(adoption._TREE_CACHE)
        try:
            adoption._TREE_CACHE.update({old: {"src/a.rs": "1" * 40},
                                         new: {"src/a.rs": "2" * 40}})
            content = {"inputs": ["src/**"]}
            names = {"inputs": ["absent-helper.py"], "pathsets": ["src/**"]}
            self.assertNotEqual(adoption.input_digest(old, content), adoption.input_digest(new, content))
            self.assertEqual(adoption.input_digest(old, names), adoption.input_digest(new, names))
            adoption._TREE_CACHE[new]["src/new.rs"] = "3" * 40
            adoption._DIGEST_CACHE.clear()
            self.assertNotEqual(adoption.input_digest(old, names), adoption.input_digest(new, names))
        finally:
            adoption._TREE_CACHE.clear()
            adoption._TREE_CACHE.update(saved)
            adoption._DIGEST_CACHE.clear()

    def test_knowledge_smoke_witness_binds_document_and_physical_child_bytes(self):
        root = Path(__file__).resolve().parents[2]
        rows = json.loads((root / "validation/main-preflight-inputs.json").read_text())["families"]
        row = adoption.witness("operations:test_knowledge_pilot.KnowledgePilotSmokeTests.test_code_context_is_source_bound_and_utf8_bounded", rows)
        selected = ["docs/PLAYBACK.md", "docs/ci/PYTHON-UNIT-PR-RECEIPTS.md",
                    "docs/streaming/PLAYBACK-CAPS-V2-PLAN.md",
                    "crates/plurx-core/src/playback/tests/child.rs",
                    "crates/plurxd/src/http/stream/tests/child.rs",
                    "tests/validation/test_main_unit_receipts.py"]
        old, new = "e" * 40, "f" * 40
        saved = dict(adoption._TREE_CACHE)
        try:
            files = {path: ("100644", "blob", "1" * 40) for path in selected}
            files["unrelated/body.rs"] = ("100644", "blob", "1" * 40)
            adoption._TREE_CACHE[old] = files
            for path in selected + ["unrelated/body.rs"]:
                adoption._TREE_CACHE[new] = {**files, path: ("100644", "blob", "2" * 40)}
                adoption._DIGEST_CACHE.clear()
                changed = adoption.input_digest(old, row) != adoption.input_digest(new, row)
                self.assertEqual(path != "unrelated/body.rs", changed, path)
        finally:
            adoption._TREE_CACHE.clear()
            adoption._TREE_CACHE.update(saved)
            adoption._DIGEST_CACHE.clear()

    def test_own_origin_binds_job_attempt_and_inherited_inputs_are_independently_rebound(self):
        journal = {"run": 12, "commit": "a" * 40, "job": 34, "attempt": 1, "environment": {}}
        record = {**journal, "inputs": "b" * 64, "outcome": "success"}
        adoption.require_record_origin(record, journal, None)
        for field, value in (("job", 99), ("attempt", 2), ("environment", {"node": "other"})):
            with self.assertRaises(ReceiptError):
                adoption.require_record_origin({**record, field: value}, journal, None)
        adoption.require_record_origin({**record, "inputs": "c" * 64},
                                       {**journal, "run": 13, "job": 35}, record)
        with self.assertRaises(ReceiptError):
            adoption.require_record_origin({**record, "outcome": "skipped"},
                                           {**journal, "run": 13, "job": 35}, record)


    def test_workflow_comments_and_conditional_steps_cannot_claim_executed_adapter_suites(self):
        from validation.regression_field import executed_suites
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / ".github/workflows").mkdir(parents=True)
            lane = root / ".github/workflows/main-fast-lane.yml"
            (root / "Makefile").write_text("")
            lane.write_text("jobs:\n  preflight:\n    steps:\n"
                            "      # python3 -m validation.main_preflight_adoption node\n"
                            "      - name: disabled\n        if: false\n"
                            "        run: python3 -m validation.main_preflight_adoption node\n")
            self.assertEqual(executed_suites(root), ((), ()))
            (root / "validation").mkdir()
            (root / "validation/main_preflight_adoption.py").write_text("NODE = ('tests/example.js',)\n")
            lane.write_text("jobs:\n  preflight:\n    steps:\n"
                            "      - run: python3 -m validation.main_preflight_adoption node\n")
            self.assertEqual(executed_suites(root), (("tests/example.js",), ()))

    def test_executable_mode_change_invalidates_content_and_boolean_pathsets_refuse(self):
        old, new = "c" * 40, "d" * 40
        saved = dict(adoption._TREE_CACHE)
        try:
            adoption._TREE_CACHE.update({old: {"scripts/control": ("100644", "blob", "1" * 40)},
                                         new: {"scripts/control": ("100755", "blob", "1" * 40)}})
            self.assertNotEqual(adoption.input_digest(old, {"inputs": ["scripts/control"]}),
                                adoption.input_digest(new, {"inputs": ["scripts/control"]}))
            with self.assertRaises(ReceiptError):
                adoption.input_digest(old, {"inputs": ["scripts/control"], "pathsets": True})
        finally:
            adoption._TREE_CACHE.clear()
            adoption._TREE_CACHE.update(saved)
            adoption._DIGEST_CACHE.clear()

    def test_fixture_error_after_success_is_durable_and_cannot_disappear_on_retry(self):
        class SyntheticFixture(unittest.TestCase):
            def test_synthetic_success(self):
                pass

            @classmethod
            def tearDownClass(cls):
                raise RuntimeError("synthetic teardown failure")

        scope = {"repository": 1, "pr": 999}
        journal = {"version": 1, "scope": scope, "commit": "a" * 40, "run": 12, "job": 34,
                   "attempt": 1, "environment": {}, "producer_blob": "", "manifest_blob": "",
                   "outcomes": {}, "skips": {}, "phase_errors": []}
        with tempfile.TemporaryDirectory() as directory, \
             mock.patch.object(adoption, "JOURNAL", Path(directory) / "receipt.json"), \
             mock.patch.object(adoption, "api_context", return_value=(None, scope, "a" * 40, 12)), \
             mock.patch.object(adoption, "git", return_value=""), \
             mock.patch.object(adoption, "MANIFEST", Path(directory) / "inputs.json"):
            # The manifest and journal readers share bounded_json; provide their real shapes.
            with mock.patch.object(adoption, "bounded_json", side_effect=lambda raw:
                                   {"families": []} if raw == b'{}' else journal), \
                 mock.patch.object(adoption, "discover", return_value=[SyntheticFixture("test_synthetic_success")]) as discovery, \
                 mock.patch.object(adoption, "witness", return_value={"inputs": ["synthetic"]}), \
                 mock.patch.object(adoption, "input_digest", return_value="b" * 64), \
                 mock.patch.object(adoption, "environment", return_value={}):
                adoption.JOURNAL.write_bytes(b'journal')
                adoption.MANIFEST.write_bytes(b'{}')
                self.assertEqual(adoption.execute_phase("validation"), 1)
                self.assertEqual(len(journal["outcomes"]), 1)
                self.assertEqual(len(journal["phase_errors"]), 1)
                with self.assertRaises(ReceiptError):
                    adoption.execute_phase("validation")
                self.assertEqual(discovery.call_count, 1)

    def test_method_witness_is_exact_and_cannot_cover_a_new_name_suffix(self):
        rows = [{"id_prefix": "validation:test_example.Case.test_one", "inputs": ["one"], "reason": "exact"},
                {"id_prefix": "validation:test_example.", "inputs": ["family"], "reason": "family"}]
        self.assertEqual(adoption.witness("validation:test_example.Case.test_one", rows)["inputs"], ["one"])
        self.assertEqual(adoption.witness("validation:test_example.Case.test_one_new", rows)["inputs"], ["family"])
        with self.assertRaises(ReceiptError):
            adoption.witness("validation:test_example.Case.test_one_new", rows[:1])

    def test_journal_schema_and_terminal_job_provenance_are_required(self):
        scope = {"repository": 1, "pr": 999}
        prior = {"id": 12, "commit_sha": "a" * 40}
        job = {"id": 34, "run_id": 12, "repo_id": 1, "attempt": 1,
               "name": "fast policy and contract preflight", "status": "success"}
        journal = {"version": 1, "scope": scope, "run": 12, "commit": "a" * 40,
                   "job": 34, "attempt": 1, "environment": {}, "producer_blob": "b" * 40,
                   "manifest_blob": "c" * 40, "outcomes": {}, "skips": {}, "phase_errors": []}
        adoption.validate_journal(journal, scope, prior, job, {})
        for changed in ({**job, "id": 35}, {**job, "repo_id": 2},
                        {**job, "run_id": 13}, {**job, "attempt": 2}, {**job, "status": "running"}):
            with self.assertRaises(ReceiptError):
                adoption.validate_journal(journal, scope, prior, changed, {})
        for changed in ({**journal, "unknown": True}, {**journal, "phase_errors": [{"phase": "node", "error": "in progress"}]},
                        {**journal, "outcomes": {"validation:test.Case.test_one": {"outcome": "success"}}}):
            with self.assertRaises(ReceiptError):
                adoption.validate_journal(changed, scope, prior, job, {})

    def test_recursive_input_globs_cover_root_and_nested_sources(self):
        for path in ("crates/plurxd/src/transcode.rs", "crates/plurxd/src/transcode/manager/create.rs"):
            self.assertTrue(adoption.matches_input(path, "crates/plurxd/src/**/*.rs"))
        self.assertTrue(adoption.matches_input("src/mod.rs", "src/**/**/*.rs"))
        self.assertFalse(adoption.matches_input("crates/plurxd/src/script.py", "crates/plurxd/src/**/*.rs"))
        self.assertTrue(adoption.matches_input("README.md", "*.md"))
        self.assertFalse(adoption.matches_input("clients/apple/README.md", "*.md"))
        self.assertFalse(adoption.matches_input(".github/workflows/nested/new.yml", ".github/workflows/*.yml"))
        with self.assertRaises(ReceiptError):
            adoption.matches_input("a", "**/" * 9 + "a")

    def test_pull_request_sync_aliases_preserve_source_repository_and_readiness_binding(self):
        import json
        repo = {"id": 1, "full_name": "owner/repository"}
        pull = {"number": 845, "draft": False, "state": "open",
                "head": {"repo": repo, "sha": "a" * 40},
                "base": {"repo": repo, "ref": "main"}}
        def run(action, changed_pull=None):
            return {"repository": repo, "commit_sha": "a" * 40, "workflow_id": "main-fast-lane.yml",
                    "event": "pull_request", "event_payload": json.dumps({"number": 845,
                        "repository": repo, "pull_request": changed_pull or pull, "action": action})}
        scope = {"repository": 1, "pr": 845}
        for action in ("synchronize", "synchronized"):
            self.assertEqual(adoption.bind_event(run(action), scope)["action"], action)
            for changed in ({**pull, "draft": True}, {**pull, "state": "closed"},
                            {**pull, "head": {"repo": repo, "sha": "b" * 40}},
                            {**pull, "base": {"repo": {**repo, "id": 2}, "ref": "main"}}):
                with self.assertRaises(ReceiptError):
                    adoption.bind_event(run(action, changed), scope)
        with self.assertRaises(ReceiptError):
            adoption.bind_event(run("synchronizing"), scope)


