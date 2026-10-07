"""New adoption controls; first unit execution belongs to the candidate lane."""

import unittest
from pathlib import Path
import tempfile
from unittest import mock

from validation import main_preflight_adoption as adoption
from validation.main_preflight_adoption import decode_progress, select_adoption, terminal_status
from validation.python_unit_receipts import ReceiptError


class MainPreflightAdoptionCase(unittest.TestCase):
    def test_ordered_events_bind_skips_without_inventing_positive_ids(self):
        inventory = {"validation": [f"v{i}" for i in range(293)],
                     "operations": [f"o{i}" for i in range(735)]}
        lines = ["From source transport", "state without impacting branches",
                 "." * 293, "Ran 293 tests in 1.000s", "", "OK", "Found in cache",
                 "." * 10 + "s" + "." * 700 + "s" + "." * 23,
                 "Ran 735 tests in 2.000s", "", "OK (skipped=2)"]
        result = decode_progress(lines, inventory)
        self.assertEqual(result["operations:o10"], "skipped")
        self.assertEqual(result["operations:o711"], "skipped")
        self.assertEqual(sum(value == "success" for value in result.values()), 1026)
        with self.assertRaises(ReceiptError):
            decode_progress([line.replace("." * 293, "." * 292) for line in lines], inventory)

    def test_unknown_inputs_refuse_and_changed_inputs_are_not_adopted(self):
        outcomes = {"v:a": "success", "v:b": "success", "o:c": "skipped"}
        with self.assertRaises(ReceiptError):
            select_adoption(outcomes, {"v:a": True, "v:b": False})
        self.assertEqual(select_adoption(outcomes, {"v:a": True, "v:b": False, "o:c": True}),
                         {"v:a": "success", "o:c": "skipped"})

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

    def test_legacy_source_and_job_must_match_before_log_adoption(self):
        run = {"id": 4275, "repository": {"id": 1}, "workflow_id": "main-fast-lane.yml",
               "commit_sha": adoption.LEGACY["commit"]}
        job = {"id": 43709, "run_id": 4275, "repo_id": 1, "attempt": 1, "status": "success",
               "name": "fast policy and contract preflight"}
        with self.assertRaises(ReceiptError):
            adoption.authenticate_legacy(run, job, b"changed log")
        with self.assertRaises(ReceiptError):
            adoption.authenticate_legacy(run, {**job, "attempt": 2}, b"")
        with self.assertRaises(ReceiptError):
            adoption.authenticate_legacy({**run, "commit_sha": "0" * 40}, job, b"")

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

    def test_authenticated_all_skipped_draft_is_zero_execution_but_attempted_job_refuses(self):
        scope = {"repository": 1, "pr": 845}
        repo = {"id": 1, "full_name": "owner/repository"}
        pull = {"number": 845, "draft": True, "state": "open",
                "head": {"repo": repo, "sha": "a" * 40},
                "base": {"repo": repo, "ref": "main"}}
        import json
        prior = {"id": 12, "repository": repo, "commit_sha": "a" * 40,
                 "workflow_id": "main-fast-lane.yml", "event": "pull_request",
                 "event_payload": json.dumps({"repository": repo, "pull_request": pull,
                                              "number": 845, "action": "converted_to_draft"})}
        job = {"id": 34, "run_id": 12, "repo_id": 1, "attempt": 1,
               "name": "fast policy and contract preflight", "status": "skipped"}
        adoption.require_missing_journal_safe(prior, [job], scope)
        for changed in ({**job, "status": "failure"}, {**job, "repo_id": 2}, {**job, "attempt": 2}):
            with self.assertRaises(ReceiptError):
                adoption.require_missing_journal_safe(prior, [changed], scope)
        with self.assertRaises(ReceiptError):
            adoption.require_missing_journal_safe(prior, [], scope)

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

    def test_legacy_inventory_resolves_local_fixture_bases_without_importing_tests(self):
        source = ("import unittest\nclass Fixture(unittest.TestCase):\n    def setUp(self): pass\n"
                  "class Cases(Fixture):\n    def test_b(self): pass\n    def test_a(self): pass\n"
                  "class Mixin:\n    def helper(self): pass\n"
                  "class Multiple(Mixin, unittest.TestCase):\n    def test_c(self): pass\n")
        def git(*args):
            if args[0] == "ls-tree":
                return args[-1] + "/test_sample.py"
            return source
        with mock.patch.object(adoption, "git", side_effect=git):
            self.assertEqual(adoption.inventory("a" * 40),
                             {suite: ["test_sample.Cases.test_a", "test_sample.Cases.test_b", "test_sample.Multiple.test_c"]
                              for suite in ("validation", "operations")})
        with mock.patch.object(adoption, "git", side_effect=lambda *args:
                               git(*args).replace("Cases(Fixture)", "Cases(unknown.Fixture)")):
            with self.assertRaises(ReceiptError):
                adoption.inventory("a" * 40)
