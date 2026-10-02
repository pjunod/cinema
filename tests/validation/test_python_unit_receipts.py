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
            "commit": COMMIT, "complete": False, "passes": {}, "fixture_errors": []}


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

    def test_current_applicability_rejects_changed_bodies_and_retains_bound_candidates(self):
        """ONE fake-source control; never execute the repository test inventory."""
        import hashlib
        import os

        old_commit, current_commit = "a" * 40, "b" * 40
        module_path = "tests/validation/test_fixture.py"
        other_path = "tests/operations/test_ops.py"
        old_source = b'''import unittest
FLAG = True
def decorate(fn):
    return fn
class Helper:
    def helper(self):
        return 7
class Case(Helper, unittest.TestCase):
    def setUp(self):
        self.value = 7
    @decorate
    def test_changed(self):
        self.assertTrue(FLAG)
    def test_unchanged(self):
        self.assertEqual(self.helper(), self.value)
'''
        current_source = old_source.replace(b"self.assertTrue(FLAG)", b"self.assertFalse(FLAG)")
        other_source = b"import unittest\nclass Case(unittest.TestCase):\n    def test_other(self):\n        pass\n"
        changed = "validation:test_fixture.Case.test_changed"
        unchanged = "validation:test_fixture.Case.test_unchanged"
        other = "operations:test_ops.Case.test_other"
        sources = {(old_commit, module_path): old_source, (current_commit, module_path): current_source,
                   (old_commit, other_path): other_source, (current_commit, other_path): other_source}
        current_files = {module_path: current_source, other_path: other_source}
        reads = []

        def source_reader(commit, path):
            reads.append((commit, path))
            if (commit, path) not in sources:
                raise receipts.ReceiptError("intentional missing historical blob")
            return sources[commit, path]

        def guard():
            return receipts.SourceApplicability(current_commit, source_reader, current_files.__getitem__)

        old_pass = {"commit": old_commit, "run": 10}
        new_pass = {"commit": current_commit, "run": 11}
        same_guard = guard()
        self.assertFalse(same_guard(changed, old_pass))
        self.assertTrue(same_guard(unchanged, old_pass))
        self.assertTrue(same_guard(other, old_pass))
        before = list(reads)
        self.assertTrue(same_guard(unchanged, old_pass))
        self.assertEqual(reads, before, "each immutable source blob is cached")
        same_guard.finish({unchanged: old_pass, other: old_pass})

        # Sibling method edits/additions and line/comment movement do not
        # invalidate the unchanged method's actual assertions or fixtures.
        sources[current_commit, module_path] = b"# new comment\n\n" + current_source + b"    def test_sibling(self):\n        self.fail('new sibling')\n"
        current_files[module_path] = sources[current_commit, module_path]
        self.assertTrue(guard()(unchanged, old_pass))
        sources[current_commit, module_path] = current_source
        current_files[module_path] = current_source
        for changed_fixture in (
                old_source.replace(b"return 7", b"return 8"),
                old_source.replace(b"self.value = 7", b"self.value = 8"),
                old_source.replace(b"@decorate", b"@unittest.skip('new decorator')"),
                old_source.replace(b"Case(Helper, unittest.TestCase)", b"Case(unittest.TestCase, Helper)")):
            sources[current_commit, module_path] = changed_fixture
            current_files[module_path] = changed_fixture
            self.assertFalse(guard()(changed, old_pass))
        sources[current_commit, module_path] = current_source
        current_files[module_path] = current_source

        def archived(value):
            data = io.BytesIO()
            with zipfile.ZipFile(data, "w") as archive:
                archive.writestr("receipt.json", json.dumps(value))
            return data.getvalue()

        class FakeAPI:
            def __init__(self, current_passes=None, comments=None, old_passes=None):
                self.comments = comments or []
                self.zips, self.finals, self.markers = {}, [], {}
                for run, commit, passes in (
                        (10, old_commit, old_passes if old_passes is not None else
                         {changed: old_pass, unchanged: old_pass, other: old_pass}),
                        (11, current_commit, current_passes or {})):
                    final = {**journal(), "run": run, "commit": commit,
                             "complete": True, "passes": passes}
                    start = {**final, "complete": False, "passes": {}}
                    for identity, value, name in (
                            (run + 100, start, receipts.key(SCOPE) + "-start-" + str(run)),
                            (run + 200, final, receipts.key(SCOPE))):
                        raw = archived(value)
                        self.zips["/actions/artifacts/" + str(identity) + "/zip"] = raw
                        item = {"id": identity, "run_id": run, "name": name,
                                "expired": False, "size_in_bytes": len(raw)}
                        if value["complete"]:
                            self.finals.append(item)
                        else:
                            self.markers[name] = [item]

            def pages(self, path, query=None, field=None):
                if path == "/actions/artifacts":
                    return self.finals if query["name"] == receipts.key(SCOPE) else self.markers[query["name"]]
                if path == "/actions/runs":
                    return [{"id": 10, "commit_sha": old_commit},
                            {"id": 11, "commit_sha": current_commit}]
                if path.endswith("/jobs"):
                    return [{"name": receipts.job_name(SCOPE), "repo_id": 7,
                             "attempt": 1, "status": "success"}]
                if path == "/issues/42/comments":
                    return self.comments
                raise AssertionError("unexpected fake API path")

            def bytes(self, path, query=None):
                return self.zips[path]

            def get(self, path):
                return {"user": {"id": 2}, "permission": "write"}

        with tempfile.TemporaryDirectory() as root:
            previous = os.getcwd()
            os.chdir(root)
            try:
                restored = receipts.restore(FakeAPI(), SCOPE, 12, applicability=guard())
                self.assertEqual(restored, {unchanged: old_pass, other: old_pass})
                newest = receipts.restore(FakeAPI({changed: new_pass}), SCOPE, 12, applicability=guard())
                self.assertEqual(newest[changed], new_pass, "stale old journal cannot mask new applicable source")
                events = []
                state = {**journal(), "run": 12, "commit": current_commit,
                         "passes": dict(restored), "applicability_commit": current_commit}
                self.assertEqual(self.execute(state, {
                    "validation": [Fake(changed.split(":", 1)[1], events),
                                   Fake(unchanged.split(":", 1)[1], events)],
                    "operations": [Fake(other.split(":", 1)[1], events)]}), 0)
                self.assertEqual(events, [changed.split(":", 1)[1]])
                self.assertEqual(state["passes"][unchanged], old_pass)
                self.assertEqual(state["passes"][changed], {"commit": current_commit, "run": 12})

                directory = Path("validation/python-unit-local")
                directory.mkdir(parents=True)

                def local(file_bytes, test_id, source_commit=None, identity=5):
                    proof = {"version": 1, "repository": 7, "branch": "codex/fix", "suite": "validation",
                             "test_file": module_path, "test_file_sha256": hashlib.sha256(file_bytes).hexdigest(),
                             "test_ids": [test_id.split(":", 1)[1]], "command": "explicit fake command",
                             "output": "explicit fake outcome", "result": {"failed": 0, "errors": 0,
                             "skipped": 0, "expected_failures": 0, "unexpected_successes": 0, "count": 1}}
                    if source_commit is not None:
                        proof["source_commit"] = source_commit
                    raw = json.dumps(proof).encode()
                    digest = hashlib.sha256(raw).hexdigest()
                    (directory / (digest + ".json")).write_bytes(raw)
                    return {"id": identity, "user": {"id": 2, "login": "writer"}, "body":
                            "Python-Unit-Receipt: " + json.dumps({"repository": 7, "pr": 42,
                            "suite": "validation", "sha256": digest})}

                stale_local = local(old_source, changed)
                with self.assertRaisesRegex(receipts.ReceiptError, "Null-source"):
                    receipts.restore(FakeAPI(comments=[stale_local]), SCOPE, 12, applicability=guard())
                repaired = receipts.restore(FakeAPI({changed: new_pass}, [stale_local]),
                                            SCOPE, 12, applicability=guard())
                self.assertEqual(repaired[changed], new_pass)
                fresh_local = local(current_source, changed, current_commit, 6)
                locally_bound = receipts.restore(FakeAPI(comments=[fresh_local, stale_local]),
                                                 SCOPE, 12, applicability=guard())
                self.assertIsNone(locally_bound[changed]["run"])
                self.assertEqual(locally_bound[changed]["commit"], current_commit)
                self.assertTrue(locally_bound[changed]["provenance"].endswith("comment-6"))
                stale_claim = json.loads(stale_local["body"].split(": ", 1)[1])
                old_local_source = {"commit": None, "run": None, "provenance":
                    "attested-local:" + stale_claim["sha256"] + ":" +
                    hashlib.sha256(old_source).hexdigest() + ":comment-5"}
                inherited_local = receipts.restore(FakeAPI(
                    {changed: new_pass}, [fresh_local, stale_local],
                    {changed: old_local_source, unchanged: old_pass, other: old_pass}),
                    SCOPE, 12, applicability=guard())
                self.assertEqual(inherited_local[changed], locally_bound[changed],
                                 "old authenticated local attribution remains historical, not applicable")
                exact_null = local(current_source, unchanged, identity=7)
                bound = receipts.restore(FakeAPI(comments=[fresh_local, stale_local, exact_null]),
                                         SCOPE, 12, applicability=guard())
                self.assertIsNone(bound[unchanged]["commit"])
                self.assertIsNone(bound[unchanged]["run"])
            finally:
                os.chdir(previous)

        # Unknown input is not a green or permission to replay all methods.
        for broken in (b"def load_tests(*args): pass\n" + current_source,
                       current_source.replace(b"Helper, unittest.TestCase", b"ForeignBase"),
                       current_source.replace(b"def setUp(self):", b"def run(self):")):
            sources[current_commit, module_path] = broken
            current_files[module_path] = broken
            unknown = guard()
            self.assertFalse(unknown(unchanged, old_pass))
            with self.assertRaises(receipts.ReceiptError):
                unknown.finish({})
        sources[current_commit, module_path] = current_source
        current_files[module_path] = current_source
        missing = guard()
        self.assertFalse(missing(unchanged, {"commit": "c" * 40, "run": 9}))
        with self.assertRaisesRegex(receipts.ReceiptError, "missing historical"):
            missing.finish({})
        current_files[module_path] = old_source
        dirty = guard()
        self.assertFalse(dirty(unchanged, old_pass))
        with self.assertRaisesRegex(receipts.ReceiptError, "worktree differs"):
            dirty.finish({})
        current_files[module_path] = current_source
        for limit, value in (("MAX_TEST_SOURCE", 1), ("MAX_SOURCE_CACHE_BYTES", 1),
                             ("MAX_SOURCE_AST_NODES", 1), ("MAX_SOURCE_CACHE_AST_NODES", 1),
                             ("MAX_SOURCE_CACHE_FILES", 0)):
            with patch.object(receipts, limit, value):
                bounded = guard()
                self.assertFalse(bounded(unchanged, old_pass))
                with self.assertRaises(receipts.ReceiptError):
                    bounded.finish({})
        with patch.object(receipts, "discover") as discovery:
            with tempfile.TemporaryDirectory() as root:
                with self.assertRaisesRegex(receipts.ReceiptError, "CLI applicability"):
                    receipts.execute(journal(), Path(root) / "receipt.json")
            discovery.assert_not_called()
        raw_state = {**journal(), "commit": current_commit,
                     "applicability_commit": current_commit,
                     "passes": {changed: old_pass}}
        with patch.object(receipts, "SourceApplicability", return_value=guard()), \
                patch.object(receipts, "discover") as discovery:
            with tempfile.TemporaryDirectory() as root:
                with self.assertRaisesRegex(receipts.ReceiptError, "lack current applicability"):
                    receipts.execute(raw_state, Path(root) / "receipt.json")
            discovery.assert_not_called()
        source = Path(receipts.__file__).read_text()
        self.assertIn("applicability=SourceApplicability(commit)", source)
        self.assertIn('"applicability_commit": commit', source)

    def test_sibling_definition_metadata_binds_fixture_without_replaying_bodies(self):
        """ONE new local fixture edge; earlier combined success stays retained."""
        prefix = b'''import unittest
FLAG = None
def configure(value):
    global FLAG
    FLAG = value
    return lambda fn: fn
class Case(unittest.TestCase):
    def test_kept(self):
        self.assertTrue(FLAG)
'''
        old_commit, current_commit = "a" * 40, "b" * 40
        path = "tests/validation/test_fixture.py"
        key = "validation:test_fixture.Case.test_kept"
        prior = {"commit": old_commit, "run": 10}
        for declaration, introspect in (
                (b"    @configure(True)\n    def test_sibling(self):\n", False),
                (b"    def test_sibling(self, value=configure(True)):\n", False),
                (b"    def test_sibling(self, *, value=configure(True)):\n", False),
                (b"    def test_sibling(self, value: configure(True)):\n", True),
                (b"    def test_sibling(self) -> configure(True):\n", True),
                (b"    @configure(True)\n    async def test_sibling(self):\n", False)):
            with self.subTest(declaration=declaration):
                old = prefix + declaration + b"        pass\n"
                changed = old.replace(b"configure(True)", b"configure(False)")
                # Execute only this controlled fixture's class definitions, not
                # any repository or fixture test method. Python may defer an
                # annotation until inspection; its fixture effect is still bound.
                for raw, expected in ((old, True), (changed, False)):
                    namespace = {}
                    exec(compile(raw, "owned-synthetic-fixture", "exec"), namespace)
                    if introspect:
                        namespace["Case"].test_sibling.__annotations__
                    self.assertIs(namespace["FLAG"], expected)
                source = {(old_commit, path): old, (current_commit, path): changed}
                def guard():
                    return receipts.SourceApplicability(
                        current_commit, lambda commit, name: source[commit, name],
                        lambda name: source[current_commit, name])
                invalidated = guard()
                self.assertFalse(invalidated(key, prior))
                invalidated.finish({})
                source[current_commit, path] = old.replace(
                    b"        pass\n", b"        raise AssertionError('body never executed')\n")
                preserved = guard()
                self.assertTrue(preserved(key, prior), "sibling body alone is not fixture metadata")
                preserved.finish({key: prior})

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
        self.assertIn("group: python-unit-${{ github.repository }}-${{ github.ref }}", workflow)
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

    def test_http_diagnostics_reveal_only_status_and_normalized_path(self):
        import urllib.error
        api = receipts.API("https://forge.example.test/api/v1", "owner/repo", "secret-token")
        class FakeOpener:
            def open(self, request, timeout):
                raise urllib.error.HTTPError(request.full_url, 403, "secret-reason",
                                             {"Authorization": "secret-header"}, io.BytesIO(b"secret-body"))
        api.opener = FakeOpener()
        with self.assertRaises(receipts.ReceiptHTTPError) as caught:
            api.get("/collaborators/writer/permission", {"secret-query": "secret-value"})
        self.assertEqual(str(caught.exception),
                         "HTTP 403 at /repos/owner/repo/collaborators/writer/permission; evidence unavailable")
        self.assertNotIn("secret", str(caught.exception))

    def test_collaborator_403_accepts_only_explicit_verified_owner_enrollment(self):
        class FakeAPI:
            def __init__(self, status=403, permission=None):
                self.status, self.permission = status, permission
            def get(self, path):
                if self.permission is not None:
                    return {"user": {"id": 1}, "permission": self.permission}
                raise receipts.ReceiptHTTPError(self.status, "/repos/noirr/plurx" + path)
        scope = {**SCOPE, "repository": 1}
        owner = {"id": 1, "login": "pjunod"}
        with patch("sys.stderr", new=io.StringIO()):
            receipts.verify_attestor(FakeAPI(), scope, owner)
            for api, candidate_scope, candidate_owner in (
                (FakeAPI(401), scope, owner), (FakeAPI(404), scope, owner),
                (FakeAPI(), SCOPE, owner), (FakeAPI(), scope, {"id": 2, "login": "pjunod"}),
                (FakeAPI(), scope, {"id": 1, "login": "reader"}),
                (FakeAPI(permission="read"), scope, owner)):
                with self.assertRaises(receipts.ReceiptError):
                    receipts.verify_attestor(api, candidate_scope, candidate_owner)

    def test_literal_job_and_step_local_artifact_identity_do_not_need_job_outputs(self):
        workflow = (Path(__file__).resolve().parents[2] / ".github/workflows/effort-ci.yml").read_text()
        preflight = workflow.split("  preflight:", 1)[1].split("  rust_compile:", 1)[0]
        self.assertIn("name: Python unit receipts", preflight)
        self.assertIn("group: python-unit-${{ github.repository }}-${{ github.ref }}", preflight)
        self.assertIn("name: ${{ steps.receipts.outputs.receipt_key }}-start-${{ github.run_id }}", preflight)
        self.assertIn("name: ${{ steps.receipts.outputs.receipt_key }}\n", preflight)
        self.assertNotIn("needs.scope.outputs.receipt_key", preflight)
        self.assertEqual(receipts.job_name(SCOPE), "Python unit receipts")
        self.assertNotEqual(receipts.key(SCOPE), receipts.key({**SCOPE, "pr": 99}))

    def test_failed_prepare_recovery_requires_exact_run_job_source_and_log(self):
        proof = json.loads((Path(__file__).resolve().parents[2] /
                            "validation/python-unit-preunit-failure3705.json").read_text())
        prior = {"id": proof["run"], "commit_sha": proof["commit"]}
        job = {"id": proof["job"], "run_id": proof["run"], "repo_id": 1,
               "attempt": 1, "name": "", "status": "failure"}
        class FakeAPI:
            def __init__(self, mismatch):
                self.mismatch = mismatch
            def get(self, path):
                return {"repository": {"id": 1}, "commit_sha": proof["commit"],
                        "prettyref": proof["scope"]["branch"], "workflow_id": "effort-ci.yml",
                        "status": "success" if self.mismatch == "run" else "failure"}
            def bytes(self, path, query=None):
                return b"contradictory-source-or-log"
        for mismatch in ("run", "source"):
            with self.assertRaises(receipts.ReceiptError):
                receipts.pre_unit_recovery(FakeAPI(mismatch), proof["scope"], prior, [job])
        self.assertFalse(receipts.pre_unit_recovery(FakeAPI("source"), SCOPE, prior, [job]))
        with self.assertRaises(receipts.ReceiptError):
            receipts.pre_unit_recovery(FakeAPI("source"), proof["scope"], prior,
                                      [{**job, "attempt": 2}])

    def test_effort_node_units_have_only_the_existing_web_lane(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / ".github/workflows/effort-ci.yml").read_text()
        preflight = workflow.split("  preflight:", 1)[1].split("  web_static:", 1)[0]
        self.assertNotIn("player-input-contract.test.js", preflight)
        self.assertNotIn("player-dom.test.js", preflight)
        web_lane = workflow.split("  web_static:", 1)[1].split("  apple_compile:", 1)[0]
        self.assertIn("run: make effort-web-static-check", web_lane)
        self.assertNotIn("run: make web-check", web_lane)
        makefile = (root / "Makefile").read_text()
        self.assertIn("player-input-contract.test.js", makefile)
        self.assertIn("player-dom.test.js", makefile)

    def test_real_api_transport_accepts_only_explicit_repository_root_or_safe_suffix(self):
        api = receipts.API("https://forge.example.test/api/v1", "owner/repo", "fixture-token")
        calls = []
        class FakeOpener:
            def open(self, request, timeout):
                calls.append((request.full_url, timeout))
                return io.BytesIO(b'{"id":7}')
        api.opener = FakeOpener()
        self.assertEqual(api.get(""), {"id": 7})
        self.assertEqual(calls, [("https://forge.example.test/api/v1/repos/owner/repo", 15)])
        for unsafe in ("../escape", "/../escape", "relative"):
            with self.assertRaises(receipts.ReceiptError):
                api.get(unsafe)
        self.assertEqual(len(calls), 1)

    def test_post_success_class_fixture_skip_is_not_silently_green(self):
        events, state = [], journal()
        class Fixture(unittest.TestCase):
            def test_method(self):
                events.append("method")
            @classmethod
            def tearDownClass(cls):
                raise unittest.SkipTest("intentional fixture skip")
        method = Fixture("test_method")
        self.execute(state, {"validation": [method], "operations": [Fake("ops", events)]})
        self.assertIn("validation:" + method.id(), state["passes"])
        self.assertEqual(len(state["fixture_errors"]), 1)
        with self.assertRaises(receipts.ReceiptError):
            receipts.validate_journal(state, SCOPE, 10, COMMIT)

    def test_post_success_class_fixture_error_is_persisted_and_blocks_reuse(self):
        events, state = [], journal()
        class Fixture(unittest.TestCase):
            def test_method(self):
                events.append("method")

            @classmethod
            def tearDownClass(cls):
                raise RuntimeError("intentional post-success fixture failure")
        method = Fixture("test_method")
        self.assertEqual(self.execute(state, {
            "validation": [method], "operations": [Fake("ops", events)]}), 1)
        self.assertIn("validation:" + method.id(), state["passes"])
        self.assertEqual(len(state["fixture_errors"]), 1)
        self.assertIn("tearDownClass", state["fixture_errors"][0])
        with self.assertRaises(receipts.ReceiptError):
            receipts.validate_journal(state, SCOPE, 10, COMMIT)
        with tempfile.TemporaryDirectory() as root:
            with self.assertRaises(receipts.ReceiptError):
                receipts.execute(state, Path(root) / "receipt.json", {
                    "validation": [Fixture("test_method")], "operations": [Fake("ops", events)]})
        self.assertEqual(events, ["method", "ops"])


if __name__ == "__main__":
    unittest.main()
