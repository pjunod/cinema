"""New adoption controls; first unit execution belongs to the candidate lane."""

import unittest
from pathlib import Path
import tempfile
from unittest import mock

from validation import main_preflight_adoption as adoption
from validation.main_preflight_adoption import decode_progress, select_adoption, terminal_status
from validation.python_unit_receipts import ReceiptError


class MainPreflightAdoptionCase(unittest.TestCase):
    def test_timeout_before_mandatory_upload_imports_no_node_outcomes(self):
        import contextlib
        import io
        import json
        from validation import main_unit_receipts as main
        scope, commit, rid = {'repository': 1, 'pr': 7}, 'a' * 40, 10
        repo = {'id': 1, 'full_name': 'owner/repository'}
        event = {'repository': repo, 'number': 7, 'action': 'reopened', 'pull_request': {
            'number': 7, 'state': 'open', 'draft': False,
            'head': {'repo': repo, 'ref': 'topic', 'sha': commit},
            'base': {'repo': repo, 'ref': 'main'}}}
        prior = {'id': rid, 'repository': repo, 'workflow_id': main.WORKFLOW,
                 'commit_sha': commit, 'event': 'pull_request', 'event_payload': json.dumps(event)}
        job = {'id': 20, 'run_id': rid, 'repo_id': 1, 'attempt': 1,
               'task_id': 30, 'name': main.JOB, 'status': 'failure'}
        workflow = (Path(__file__).resolve().parents[2] / '.github/workflows/main-fast-lane.yml').read_bytes()
        python_scope = dict(scope, branch='topic', base='main', workflow=main.WORKFLOW)
        python_start = {'version': main.receipts.VERSION, 'scope': python_scope, 'run': rid,
                        'commit': commit, 'complete': False, 'passes': {}, 'fixture_errors': []}
        env = {'platform': 'linux', 'machine': 'x86_64', 'python': [3, 12], 'node': 'v22.23.2'}
        node_start = {'version': 1, 'scope': scope, 'run': rid, 'commit': commit, 'job': 20,
                      'attempt': 1, 'environment': env, 'producer_blob': 'b' * 40,
                      'manifest_blob': 'b' * 40, 'outcomes': {}, 'skips': {}, 'phase_errors': []}
        py_key, node_key = main.key(python_scope), 'main-preflight-v1-r1-pr7'
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            print(commit, commit, 'triggered by event: pull_request', sep='\n')
            main.emit_snapshot(python_start, 'start')
            for name in (py_key, node_key):
                print(f'Artifact {name}-start-{rid} has been successfully uploaded!')
            main.emit_snapshot(dict(python_start, complete=True), 'final')
            print('this step has been cancelled: context deadline exceeded')
        raw = output.getvalue().encode()
        class API:
            log = raw
            final = []
            marker_missing = False
            def pages(self, path, query):
                name = query['name']
                if not name.endswith('-start-10'): return self.final
                return [] if self.marker_missing else [{'id': 1 if name.startswith('main-python') else 2,
                    'name': name, 'run_id': rid, 'expired': False}]
            def bytes(self, path, query=None):
                if path.endswith('/logs'): return self.log
                return python_start if path.endswith('/1/zip') else node_start
        api = API()
        with mock.patch.object(main, 'source', return_value=workflow), \
             mock.patch.object(adoption, 'artifact_json', side_effect=lambda value: value), \
             mock.patch.object(adoption, 'git', return_value='b' * 40), \
             mock.patch.object(adoption, 'environment', return_value=env), \
             mock.patch.object(adoption, 'atomic_json') as writer:
            self.assertTrue(adoption.recover_missing_final_before_node(api, scope, prior, [job], workflow))
            self.assertEqual(node_start['outcomes'], {})
            writer.assert_not_called()
            # Missing records alone cannot prove zero execution.
            for changed in (raw.replace(b'MAIN-UNIT-END final', b'truncated'),
                            raw + f'Artifact {py_key} has been successfully uploaded!\n'.encode(),
                            raw + b'Main preflight outcome {}\n',
                            raw.replace(b'context deadline exceeded', b'unknown failure')):
                api.log = changed
                with self.subTest(log=changed[-100:]), self.assertRaises(ReceiptError):
                    adoption.recover_missing_final_before_node(api, scope, prior, [job], workflow)
            api.log = raw
            for changed in (workflow.replace(b'if-no-files-found: error', b'if-no-files-found: ignore'),
                            workflow.replace(b'      - name: Check the shared player input contract',
                                b'        continue-on-error: true\n      - name: Check the shared player input contract'),
                            workflow.replace(b'        run: python3 -m validation.main_preflight_adoption node',
                                b'        if: always()\n        run: python3 -m validation.main_preflight_adoption node'),
                            workflow.replace(b'      - name: Check the shared player input contract',
                                b'        continue-on-error : true\n      - name: Check the shared player input contract'),
                            workflow.replace(b'      - name: Check the shared player input contract',
                                b'        \"continue-on-error\": true\n      - name: Check the shared player input contract')):
                with mock.patch.object(main, 'source', return_value=changed), self.assertRaises(ReceiptError):
                    adoption.recover_missing_final_before_node(api, scope, prior, [job], changed)
            api.final = [{'run_id': rid}]
            with self.assertRaises(ReceiptError):
                adoption.recover_missing_final_before_node(api, scope, prior, [job], workflow)
            api.final, api.marker_missing = [], True
            with self.assertRaises(ReceiptError):
                adoption.recover_missing_final_before_node(api, scope, prior, [job], workflow)
            api.marker_missing = False
            for changed in (dict(job, attempt=2), dict(job, repo_id=2)):
                with self.assertRaises(ReceiptError):
                    adoption.recover_missing_final_before_node(api, scope, prior, [changed], workflow)
            self.assertFalse(adoption.recover_missing_final_before_node(api, scope, prior, [dict(job, status='running')], workflow))
            self.assertFalse(adoption.recover_missing_final_before_node(api, scope, prior, [dict(job, name='scope', status='skipped')], workflow))

    def test_exact_prepare_retry_bridge_imports_no_outcomes(self):
        from validation import main_unit_receipts as main
        scope, prior, jobs = {'repository': 1, 'pr': 888}, {'id': 4431}, []
        with mock.patch.object(main, 'recover_preunit4431', return_value=True) as proof, \
             mock.patch.object(adoption, 'atomic_json') as writer:
            self.assertIs(adoption.recover_preunit4431(None, scope, prior, jobs), True)
            proof.assert_called_once_with(None, scope, prior, jobs)
            writer.assert_not_called()
        with mock.patch.object(main, 'recover_preunit4431', return_value=False):
            self.assertFalse(adoption.recover_preunit4431(None, scope, prior, jobs))
        with mock.patch.object(main, 'recover_preunit4431', side_effect=ReceiptError('missing first attempt')):
            with self.assertRaisesRegex(ReceiptError, 'missing first attempt'):
                adoption.recover_preunit4431(None, scope, prior, jobs)

    def test_generic_bridge_workflow_has_one_python_executor_and_distinct_node_receipts(self):
        commands = ['python3 -m validation.main_unit_receipts prepare',
                    'python3 -m validation.main_unit_receipts run --suite-dir tests/validation --suite-dir tests/operations',
                    'python3 -m validation.main_preflight_adoption node']
        self.assertTrue(adoption.adapter_workflow(commands))
        for removed in commands:
            self.assertFalse(adoption.adapter_workflow([command for command in commands if command != removed]))
        self.assertTrue(adoption.adapter_workflow(
            ['python3 -m validation.main_preflight_adoption ' + phase
             for phase in ('prepare', 'validation', 'operations', 'node')]))

    def test_exact_preunit4299_environment_recovery_imports_no_outcomes(self):
        import copy
        import hashlib
        import json
        proof = copy.deepcopy(adoption.PREUNIT4299)
        scope = {'repository': 1, 'pr': 845}
        repo = {'id': 1, 'full_name': 'owner/repository'}
        event = {'action': 'synchronized', 'number': 845, 'repository': repo,
                 'pull_request': {'number': 845, 'draft': False, 'state': 'open',
                    'head': {'repo': repo, 'sha': proof['commit']},
                    'base': {'repo': repo, 'ref': 'main', 'sha': proof['base']}}}
        prior = {'id': 4299, 'repository': repo, 'event': 'pull_request', 'status': 'failure',
                 'workflow_id': 'main-fast-lane.yml', 'commit_sha': proof['commit'],
                 'event_payload': json.dumps(event)}
        jobs = [{'id': identity, 'name': name, 'task_id': task, 'status': status,
                 'run_id': 4299, 'repo_id': 1, 'attempt': 1}
                for identity, name, task, status in proof['jobs']]
        raw = (proof['commit'] + ':refs/remotes/pull/845/head\n'
               'Main preflight receipt refusal: Legacy Linux/Python/Node environment applicability unavailable\n'
               "skipping post step for 'Publish preflight attempt-start journal'; main step was skipped\n"
               "skipping post step for 'Preserve per-ID preflight journal even on failure'; main step was skipped\n"
               "Job 'fast policy and contract preflight' failed\n").encode()
        proof['sources'] = {path: hashlib.sha256(b'source').hexdigest() for path in proof['sources']}
        proof['log_bytes'], proof['log_sha256'] = len(raw), hashlib.sha256(raw).hexdigest()
        class API:
            log = raw
            artifacts = []
            def get(self, path): return prior if path == '/actions/runs/4299' else self.artifacts
            def pages(self, path, query): return self.artifacts
            def bytes(self, path, query=None): return self.log if path == '/actions/jobs/43909/logs' else b'source'
        with mock.patch.object(adoption, 'PREUNIT4299', proof), mock.patch.object(adoption, 'atomic_json') as writer:
            self.assertTrue(adoption.recover_preunit4299(API(), scope, prior, jobs))
            self.assertFalse(adoption.recover_preunit4299(API(), scope, dict(prior, id=4300), jobs))
            for changed in (jobs[:-1], [dict(job, attempt=2) for job in jobs]):
                with self.assertRaises(ReceiptError):
                    adoption.recover_preunit4299(API(), scope, prior, changed)
            api = API()
            api.artifacts = [{'run_id': 4299}]
            with self.assertRaises(ReceiptError):
                adoption.recover_preunit4299(api, scope, prior, jobs)
            api = API()
            api.log = raw.replace(b"Job 'fast", b"Main preflight outcome positive\nJob 'fast")
            altered = dict(proof, log_bytes=len(api.log), log_sha256=hashlib.sha256(api.log).hexdigest())
            with mock.patch.object(adoption, 'PREUNIT4299', altered), self.assertRaises(ReceiptError):
                adoption.recover_preunit4299(api, scope, prior, jobs)
            writer.assert_not_called()

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

    def test_exact_upload_failure_bridge_accounts_zero_without_importing_outcomes(self):
        from validation import main_unit_receipts as main
        prior = {'id': 4324}
        jobs = [{'id': 44110, 'name': main.JOB}]
        scope = {'repository': 1, 'pr': 845}
        incomplete = {'complete': False, 'passes': {'validation:test_fixture.Case.test_ok': {'run': 4315}}}
        with mock.patch.object(main, 'recover_preunit_upload4324', return_value=incomplete) as proof:
            self.assertIs(adoption.recover_preunit_upload4324(None, scope, prior, jobs), True)
            proof.assert_called_once_with(None, scope, prior, jobs[0])
            self.assertIs(incomplete['complete'], False)
            self.assertFalse(adoption.recover_preunit_upload4324(None, scope, {'id': 4323}, jobs))
            with self.assertRaises(ReceiptError):
                adoption.recover_preunit_upload4324(None, scope, prior, jobs + jobs)
        with mock.patch.object(main, 'recover_preunit_upload4324', return_value=None):
            self.assertFalse(adoption.recover_preunit_upload4324(None, scope, prior, jobs))

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

    def test_exact_preunit4294_recovery_requires_every_bound_witness_and_imports_nothing(self):
        import copy
        import hashlib
        import json
        proof = copy.deepcopy(adoption.PREUNIT4294)
        repo = {"id": 1, "full_name": "owner/repository"}
        event = {"number": 845, "repository": repo, "action": "synchronized",
                 "pull_request": {"number": 845, "draft": False, "state": "open",
                    "head": {"repo": repo, "sha": proof["commit"]},
                    "base": {"repo": repo, "ref": "main", "sha": proof["base"]}}}
        prior = {"id": 4294, "commit_sha": proof["commit"], "status": "failure",
                 "repository": repo, "workflow_id": "main-fast-lane.yml", "event": "pull_request",
                 "event_payload": json.dumps(event)}
        jobs = [{"id": identity, "name": name, "task_id": task, "status": status,
                 "run_id": 4294, "repo_id": 1, "attempt": 1}
                for identity, name, task, status in proof["jobs"]]
        raw = (proof["commit"] + ":refs/remotes/pull/845/head\n"
               "Main preflight receipt refusal: Prior event/source/PR/base/readiness binding mismatch\n"
               "skipping post step for 'Publish preflight attempt-start journal'; main step was skipped\n"
               "skipping post step for 'Preserve per-ID preflight journal even on failure'; main step was skipped\n"
               "Job 'fast policy and contract preflight' failed\n").encode()
        # Fake API/source bytes are an isolated control seam, not a recovery input.
        proof["sources"] = {path: hashlib.sha256(b"synthetic source").hexdigest() for path in proof["sources"]}
        proof["log_bytes"], proof["log_sha256"] = len(raw), hashlib.sha256(raw).hexdigest()
        scope = {"repository": 1, "pr": 845}
        class FakeAPI:
            def __init__(self):
                self.actual = copy.deepcopy(prior)
                self.raw = raw
                self.source = b"synthetic source"
                self.artifacts = []
                self.start = []

            def get(self, path):
                return self.actual if path == "/actions/runs/4294" else self.artifacts

            def bytes(self, path, query=None):
                return self.raw if path == "/actions/jobs/43864/logs" else self.source

            def pages(self, path, query=None):
                return self.start if query["name"].endswith("-start-4294") else self.artifacts

        with mock.patch.object(adoption, "PREUNIT4294", proof), \
             mock.patch.object(adoption, "atomic_json") as writer:
            self.assertIs(adoption.recover_preunit4294(FakeAPI(), scope, prior, jobs), True)
            self.assertIs(adoption.recover_preunit4294(FakeAPI(), scope, {**prior, "id": 4295}, jobs), False)
            self.assertIs(adoption.recover_preunit4294(FakeAPI(), {**scope, "pr": 846}, prior, jobs), False)
            for field, value in (("id", 43899), ("attempt", 2), ("run_id", 4295),
                                 ("repo_id", 2), ("task_id", 0), ("status", "success")):
                changed = copy.deepcopy(jobs)
                changed[2][field] = value
                with self.assertRaises(ReceiptError):
                    adoption.recover_preunit4294(FakeAPI(), scope, prior, changed)
            for failure in ("run", "source", "log", "artifact", "start", "event", "unit-marker"):
                api = FakeAPI()
                if failure == "run":
                    api.actual["commit_sha"] = "b" * 40
                elif failure == "source":
                    api.source = b"changed source"
                elif failure == "log":
                    api.raw += b"changed log"
                elif failure == "artifact":
                    api.artifacts = [{"run_id": 4294}]
                elif failure == "start":
                    api.start = [{"run_id": 4294}]
                elif failure == "event":
                    changed = copy.deepcopy(event)
                    changed["pull_request"]["base"]["sha"] = "b" * 40
                    api.actual["event_payload"] = json.dumps(changed)
                else:
                    api.raw = raw.replace(b"Job 'fast", b"Main preflight outcome positive\nJob 'fast")
                with self.subTest(failure=failure), self.assertRaises(ReceiptError):
                    if failure == "unit-marker":
                        changed_proof = {**proof, "log_bytes": len(api.raw), "log_sha256": hashlib.sha256(api.raw).hexdigest()}
                        with mock.patch.object(adoption, "PREUNIT4294", changed_proof):
                            adoption.recover_preunit4294(api, scope, prior, jobs)
                    else:
                        adoption.recover_preunit4294(api, scope, prior, jobs)
            writer.assert_not_called()
