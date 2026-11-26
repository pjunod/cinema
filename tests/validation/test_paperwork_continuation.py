"""Paperwork adopts authentic method coverage and never credits deferred cases."""
import copy
import json
import os
import tempfile
from pathlib import Path
import unittest
from unittest.mock import Mock, patch

from validation import paperwork_continuation as pc
from validation import python_unit_receipts as r


class PaperworkContinuationTests(unittest.TestCase):
    def value(self):
        return {'version': 1, 'pr': 17, 'head': 'a' * 40, 'base': 'b' * 40,
                'python_unit_evidence': {'sha256': 'c' * 64, 'attachment_id': 8,
                                         'attachment_uuid': '12345678-1234-1234-1234-123456789abc'}}

    def scope(self):
        return {'schema': 2, 'repository': 'owner/repo', 'pull_request': 17,
                'head_ref': 'codex/work', 'head_sha': 'a' * 40, 'base_sha': 'b' * 40}

    def facts(self):
        repo = {'id': 1, 'full_name': 'owner/repo', 'owner': {'id': 3}}
        pull = {'number': 17, 'state': 'open', 'merged': False, 'draft': True,
                'title': 'fix(ci): fixture', 'body': pc.PREFIX + json.dumps(self.value()),
                'head': {'repo': repo, 'ref': 'codex/work', 'sha': 'd' * 40},
                'base': {'repo': repo, 'ref': 'main', 'sha': 'b' * 40}}
        api = Mock()
        api.get.side_effect = lambda path: repo if path == '' else (
            {'commit': {'id': 'b' * 40}} if path == '/branches/main' else pull)
        api.pages.return_value = [{'name': 'codex/work', 'commit': {'id': 'd' * 40}}]
        return api, pull

    def test_enrollment_is_unique_closed_and_bound_to_an_immutable_descriptor(self):
        body = pc.PREFIX + json.dumps(self.value())
        self.assertEqual(pc.enrollment(body), self.value())
        for candidate in ('', body + '\n' + body,
                          pc.PREFIX + json.dumps({**self.value(), 'skip': True}),
                          pc.PREFIX + json.dumps({**self.value(), 'head': 'moving-branch'})):
            with self.subTest(body=candidate), self.assertRaises((r.ReceiptError, ValueError)):
                pc.enrollment(candidate)

    def test_local_publication_authenticates_actual_old_head_without_relabeling_it(self):
        api, pull = self.facts()
        def git(root, *args):
            return 'a' * 40 if args == ('rev-parse', 'HEAD') else ('codex/work' if args[0] == 'symbolic-ref' else '')
        with patch.object(pc, 'git', side_effect=git), patch.object(pc, 'ancestor', return_value=True):
            bind, actual = pc.binding(api, self.value(), {}, '.', 'publication')
        self.assertEqual(bind['head_sha'], 'a' * 40)
        self.assertEqual(actual['head']['sha'], 'd' * 40)
        self.assertIs(actual, pull)

    def test_publication_refuses_dirty_foreign_branch_nonancestor_ci_and_ready_pr(self):
        for dirty, branch, inherited, env, ready in (
                ('dirty', 'codex/work', True, {}, False), ('', 'codex/other', True, {}, False),
                ('', 'codex/work', False, {}, False), ('', 'codex/work', True, {'GITHUB_RUN_ID': '9'}, False),
                ('', 'codex/work', True, {}, True)):
            api, pull = self.facts(); pull['draft'] = not ready
            def git(root, *args):
                return 'a' * 40 if args == ('rev-parse', 'HEAD') else (branch if args[0] == 'symbolic-ref' else dirty)
            with self.subTest(dirty=dirty, branch=branch, env=env, ready=ready), \
                    patch.object(pc, 'git', side_effect=git), \
                    patch.object(pc, 'ancestor', side_effect=lambda root, parent, child: True if parent == 'b' * 40 else inherited), \
                    self.assertRaises(r.ReceiptError):
                pc.binding(api, self.value(), env, '.', 'publication')

    def test_ci_cannot_use_the_future_local_head_binding(self):
        api, _ = self.facts()
        with patch.object(pc, 'git', return_value='a' * 40), patch.object(pc, 'ancestor', return_value=True):
            with self.assertRaisesRegex(r.ReceiptError, 'actual published event head'):
                pc.binding(api, self.value(), {'GITHUB_RUN_ATTEMPT': '1', 'GITHUB_SHA': 'a' * 40}, '.', 'preliminary')

    def test_ci_event_run_head_and_current_main_are_authenticated_together(self):
        api, pull = self.facts(); pull['head']['sha'] = 'a' * 40
        api.pages.return_value = [{'name': 'codex/work', 'commit': {'id': 'a' * 40}}]
        repo = pull['head']['repo']; event = {'pull_request': copy.deepcopy(pull)}
        run = {'id': 9, 'repository': repo, 'commit_sha': 'a' * 40, 'status': 'running',
               'workflow_id': 'main-fast-lane.yml', 'trigger_event': 'pull_request',
               'event_payload': json.dumps(event)}
        main = {'commit': {'id': 'b' * 40}}
        api.get.side_effect = lambda path: repo if path == '' else (main if path == '/branches/main'
                           else run if path == '/actions/runs/9' else pull)
        workflow = (Path(__file__).resolve().parents[2] / '.github/workflows/main-fast-lane.yml').read_bytes()
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'event.json'; path.write_text(json.dumps(event))
            env = {'GITHUB_RUN_ID': '9', 'GITHUB_RUN_ATTEMPT': '1', 'GITHUB_SHA': 'a' * 40,
                   'GITHUB_EVENT_PATH': str(path)}
            with patch.object(pc, 'git', return_value='a' * 40), patch.object(pc, 'ancestor', return_value=True), \
                    patch.object(pc.retention, 'git_bytes', return_value=workflow):
                self.assertEqual(pc.binding(api, self.value(), env, '.', 'preliminary')[0],
                                 {**self.scope(), 'base_ref': 'main'})
                for field, bad in (('id', 10), ('commit_sha', 'e' * 40), ('workflow_id', 'ci.yml'),
                                   ('trigger_event', 'workflow_dispatch'), ('status', 'failure')):
                    old = run[field]; run[field] = bad
                    with self.subTest(field=field), self.assertRaises(r.ReceiptError):
                        pc.binding(api, self.value(), env, '.', 'preliminary')
                    run[field] = old
                forged = copy.deepcopy(event); forged['pull_request']['head']['sha'] = 'e' * 40
                path.write_text(json.dumps(forged))
                with self.assertRaises(r.ReceiptError):
                    pc.binding(api, self.value(), env, '.', 'preliminary')

    def test_workflow_projection_allows_only_the_paperwork_environment_line(self):
        from validation import nextest_shard_continuation as n
        raw = (Path(__file__).resolve().parents[2] / '.github/workflows/main-fast-lane.yml').read_bytes()
        projected = n.projected_main_workflow(raw)
        self.assertEqual(pc.retention.digest(projected), n.HARNESS['.github/workflows/main-fast-lane.yml'])
        line = b'          PLURX_PAPERWORK_CONTINUATION: preliminary\n'
        moved = raw.replace(line, b'', 1).replace(b'  scope:\n', b'  scope:\n' + line, 1)
        for changed in (raw + line, moved, raw.replace(b'run --suite-dir tests/validation', b'run --suite-dir tests/other')):
            with self.subTest(raw=changed), self.assertRaises(r.ReceiptError):
                n.projected_main_workflow(changed)

    def test_local_evidence_needs_exact_writer_attestation_and_target_source(self):
        api, pull = self.facts(); bind = self.scope()
        document = {'repository': 1, 'pr': 17, 'branch': 'codex/work', 'base_sha': 'b' * 40,
                    'source_commit': 'a' * 40}
        raw = json.dumps(document).encode(); descriptor = {**self.value()['python_unit_evidence'], 'sha256': pc.retention.digest(raw)}
        claim = {'repository': 1, 'pr': 17, **descriptor}
        comment = {'id': 4, 'user': {'id': 3, 'login': 'writer'},
                   'body': 'Promotion-Unit-Evidence: ' + json.dumps(claim)}
        api.pages.return_value = [comment]
        with patch.object(r, 'verify_attestor') as verify:
            self.assertEqual(pc.local_attestation(api, raw, descriptor, bind, 'a' * 40, pull)[0], document)
            verify.assert_called_once()
        for comments, candidate in (([], 'a' * 40), ([comment], 'e' * 40),
                                    ([{**comment, 'body': comment['body'].replace('"pr": 17', '"pr": 18')}], 'a' * 40)):
            api.pages.return_value = comments
            with self.subTest(comments=comments, candidate=candidate), self.assertRaises(r.ReceiptError):
                pc.local_attestation(api, raw, descriptor, bind, candidate, pull)

    def plan(self):
        return {'source_commit': 'a' * 40, 'evidence_sha256': 'c' * 64,
                'comparison': {'base_sha': 'b' * 40, 'merge_target': 'b' * 40},
                'suites': {kind: {'inventory': ['test_docs_index.Case.test_body'],
                                  'uncovered': ['test_docs_index.Case.test_body'],
                                  'retained_source_records': {}} for kind in ('validation', 'operations')}}

    def test_unbound_historical_comparison_reexecutes_only_exact_actual_tree_cases(self):
        plan = self.plan()
        operations = plan['suites']['operations']
        retained = 'test_media_pool_wire.TreeTests.test_inventory_is_exhaustive'
        operations['inventory'] += sorted(pc.COMPARISON_METHODS | {retained})
        operations['retained_source_records'] = {
            'operations:' + name: {} for name in pc.COMPARISON_METHODS | {retained}
        }

        pc.bind_selection(plan, 'd' * 40, plan['source_commit'])

        self.assertEqual(plan['comparison'], {'base_sha': 'd' * 40, 'merge_target': 'd' * 40})
        self.assertEqual(set(operations['uncovered']),
                         pc.COMPARISON_METHODS | {'test_docs_index.Case.test_body'})
        self.assertEqual(set(operations['retained_source_records']), {'operations:' + retained})
        self.assertNotIn('dependency_profile', plan)

    def execution(self, plan):
        text = ''
        for kind in ('validation', 'operations'):
            identity = json.dumps(pc.marker(plan, kind))
            text += ('SELECTED-PYTHON-BEGIN ' + identity + '\n'
                     'test_body (test_docs_index.Case.test_body) ... ok\n\nRan 1 test in 0.01s\n\nOK\n'
                     'SELECTED-PYTHON-END ' + identity + '\n')
        return text.encode()

    def test_direct_current_outcomes_require_complete_exact_selection_and_success(self):
        plan = self.plan(); raw = self.execution(plan)
        self.assertEqual(set(pc.current_outcomes(raw, plan)),
                         {'validation:test_docs_index.Case.test_body', 'operations:test_docs_index.Case.test_body'})
        for changed in (raw.replace(b'... ok', b'... FAIL', 1), raw.replace(b'... ok', b'... skipped', 1),
                        raw.replace(b'Ran 1 test', b'Ran 2 tests', 1),
                        raw.replace(b'c' * 64, b'd' * 64, 1), raw.replace(b'b' * 40, b'e' * 40, 1), raw + raw,
                        raw.replace(b'test_body (test_docs_index.Case.test_body) ... ok\n', b'', 1)):
            with self.subTest(raw=changed), self.assertRaises(r.ReceiptError):
                pc.current_outcomes(changed, plan)

    def test_green_preflight_without_direct_body_outcomes_cannot_supply_coverage(self):
        plan = self.plan(); value = self.value(); bind = self.scope(); api, pull = self.facts()
        pull['body'] += '\nManual-Source-Gate: 9'
        inventory = {'operations:test_docs_index.Case.test_body'}
        api.pages.return_value = [{'name': 'fast policy and contract preflight', 'id': 12, 'run_id': 9,
                                   'repo_id': 1, 'owner_id': 3, 'attempt': 1, 'status': 'success'}]
        api.bytes.return_value = b'received task 1 of job preflight, triggered by event: workflow_dispatch\n'
        with patch.object(pc, 'prepare', return_value=(bind, pull, plan, inventory, set(), inventory)):
            with self.assertRaisesRegex(r.ReceiptError, 'execution framing'):
                pc.final_coverage(api, value, {'GITHUB_RUN_ID': '9'}, '.')
            api.bytes.return_value += self.execution(plan)
            coverage = pc.final_coverage(api, value, {'GITHUB_RUN_ID': '9'}, '.')
        self.assertEqual(coverage['current_pass_ids'], sorted(inventory))
        self.assertEqual(coverage['preflight_job'], 12)

    def test_coverage_requires_closed_fields_current_run_and_full_disjoint_inventory(self):
        bind = self.scope(); ids = ['operations:test_docs_index.Case.test_one']
        value = {'version': 1, 'head': 'a' * 40, 'base': 'b' * 40, 'pr': 17, 'run': 9,
                 'preflight_job': 12, 'evidence_sha256': 'c' * 64, 'inventory_sha256': pc.canonical_digest(ids),
                 'retained_ids': [], 'current_pass_ids': ids, 'preflight_log_sha256': 'd' * 64,
                 'dependency_runtime': None}
        pc.coverage_shape(value, bind, 9, ids)
        for changed in ({k: v for k, v in value.items() if k != 'current_pass_ids'},
                        {**value, 'run': 8}, {**value, 'head': 'e' * 40},
                        {**value, 'retained_ids': ids}, {**value, 'current_pass_ids': []}):
            with self.subTest(value=changed), self.assertRaises(r.ReceiptError):
                pc.coverage_shape(changed, bind, 9, ids)

    def test_rule_inventory_keeps_every_default_module_and_partitions_new_methods(self):
        names = sorted(module + '.Case.test_body' for module in pc.RULE_MODULES)
        plan = {'suites': {'operations': {'inventory': names, 'uncovered': names,
                                         'retained_source_records': {}}}}
        root = Path(__file__).resolve().parents[2]
        inventory, retained, due = pc.rule_inventory(plan, root)
        self.assertEqual(len(inventory), len(pc.RULE_MODULES)); self.assertFalse(retained)
        self.assertEqual(due, inventory)
        for changed in (names[:-1], names + names[-1:]):
            bad = copy.deepcopy(plan); bad['suites']['operations']['inventory'] = changed
            with self.assertRaises(r.ReceiptError):
                pc.rule_inventory(bad, root)

    def test_enrolled_python_selection_refuses_legacy_descriptors_before_any_provider(self):
        from validation import main_python_continuation as main, native_python_execute as native

        api, pull = self.facts()
        bind = self.scope()
        descriptor = self.value()['python_unit_evidence']
        for version in (main.POLICY, 'native-python-continuation-v1'):
            document = {'version': version}
            with self.subTest(version=version), \
                    patch.object(pc.retention, 'attachment_bytes', return_value=b'fixture'), \
                    patch.object(pc.retention, 'verify_identity', return_value=(document, pull, 1, {})), \
                    patch.object(main, 'prepare') as previous_main, \
                    patch.object(native.commands, 'expanded_commands') as previous_native:
                with self.assertRaisesRegex(r.ReceiptError, 'Typed paperwork selection'):
                    native.prepare(api, descriptor, bind, bind['head_sha'])
                previous_main.assert_not_called()
                previous_native.assert_not_called()

    def test_manual_scope_requires_enrolled_dispatch_python_evidence_before_selection(self):
        from validation import qualification as q, source_gate_landing as landing

        api, pull = self.facts()
        repo = pull['head']['repo']
        pull['head']['sha'] = 'a' * 40
        bind = {**self.scope(), 'base_ref': 'main'}
        api.pages.return_value[0]['commit']['id'] = 'a' * 40
        value = self.value()
        inputs = {'promotion_pr': str(value['pr']), 'promotion_head_sha': value['head'],
                  'promotion_base_sha': value['base']}
        event = {'ref': 'refs/heads/codex/work', 'inputs': inputs}
        run = {'id': 9, 'repository': repo, 'commit_sha': bind['head_sha'], 'status': 'running',
               'workflow_id': 'ci.yml', 'trigger_event': 'workflow_dispatch'}
        api.get.side_effect = lambda path: {'': repo, '/pulls/17': pull, '/actions/runs/9': run,
                                            '/branches/main': {'commit': {'id': bind['base_sha']}}}[path]
        env = {'GITHUB_RUN_ID': '9', 'GITHUB_RUN_ATTEMPT': '1', 'GITHUB_SHA': bind['head_sha']}
        with patch.object(q, '_resolve_manual_binding_and_pull', return_value=(bind, pull)), \
                patch.object(pc.retention, 'runner_api', return_value=api), \
                patch.object(pc, 'git', return_value=bind['head_sha']), \
                patch.object(pc, 'ancestor', return_value=True), \
                patch.object(pc.retention, 'git_bytes', return_value=b'ci workflow'), \
                patch.object(landing, 'WORKFLOW_SHA256', pc.retention.digest(b'ci workflow')):
            run['event_payload'] = json.dumps(event)
            with self.assertRaisesRegex(q.QualificationError, 'exact dispatched Python unit evidence'):
                q.bind_manual_scope(env, '.')
            for descriptor in ('', json.dumps({**value['python_unit_evidence'], 'attachment_id': 9})):
                inputs['python_unit_evidence'] = descriptor
                run['event_payload'] = json.dumps(event)
                with self.subTest(descriptor=descriptor), self.assertRaises(q.QualificationError):
                    q.bind_manual_scope(env, '.')
            inputs['python_unit_evidence'] = json.dumps(value['python_unit_evidence'])
            run['event_payload'] = json.dumps(event)
            self.assertEqual(q.bind_manual_scope(env, '.'), bind)
            pull['body'] = 'ordinary description'
            del inputs['python_unit_evidence']
            run['event_payload'] = json.dumps(event)
            api.get.reset_mock()
            self.assertEqual(q.bind_manual_scope(env, '.'), bind)
            api.get.assert_not_called()

    def test_writer_reauthenticates_live_enrollment_body_title_and_direct_coverage(self):
        from validation import qualification as q, source_gate_landing as landing

        api, pull = self.facts()
        pull['title'] = 'fix(ci): exact receipt'
        bind = self.scope()
        env = {'GITHUB_RUN_ID': '9'}
        with patch.object(pc.retention, 'runner_api', return_value=api), \
                patch.object(pc, 'final_coverage', return_value={'direct': True}) as verify:
            version_two = landing.expected_paperwork(api, pull, bind, env, '.')
            self.assertEqual(version_two['version'], 2)
            live = {**env, 'PLURX_SOURCE_GATE_PAPERWORK': json.dumps(version_two)}
            q.verify_source_gate_paperwork(live, bind, pull, '.')
            self.assertEqual(verify.call_count, 2)
            verify.assert_called_with(api, self.value(), live, '.')

            unenrolled = {**pull, 'body': 'ordinary description'}
            version_one = landing.expected_paperwork(api, unenrolled, bind, env, '.')
            self.assertEqual(version_one['version'], 1)
            ordinary = {**env, 'PLURX_SOURCE_GATE_PAPERWORK': json.dumps(version_one)}
            q.verify_source_gate_paperwork(ordinary, bind, unenrolled, '.')
            forged = {**live, 'PLURX_SOURCE_GATE_PAPERWORK': json.dumps(
                {**version_two, 'unit_coverage': {'direct': False}})}
            cases = ((ordinary, pull), (live, unenrolled), (forged, pull),
                     (live, {**pull, 'body': pull['body'] + '\nchanged'}),
                     (live, {**pull, 'title': 'fix(ci): changed title'}))
            for receipt, current_pull in cases:
                with self.subTest(receipt=receipt, pull=current_pull), self.assertRaises(q.QualificationError):
                    q.verify_source_gate_paperwork(receipt, bind, current_pull, '.')

    def test_unenrolled_manual_source_gate_preserves_full_paperwork_v1(self):
        from validation import source_gate_landing as landing
        api, pull = self.facts(); pull['head']['sha'] = 'a' * 40; pull['body'] = 'ordinary description'
        api.get.return_value = pull; api.get.side_effect = None
        with tempfile.TemporaryDirectory() as temporary:
            previous = os.getcwd(); os.chdir(temporary)
            try:
                output = Path(temporary) / 'environment'
                env = {'PROMOTION_BINDING': json.dumps(self.scope()), 'GITHUB_ENV': str(output),
                       'GITHUB_RUN_ID': '9', 'PLURX_PAPERWORK_CONTINUATION': 'preliminary'}
                with patch.dict(os.environ, env), patch.object(landing.retention, 'runner_api', return_value=api), \
                        patch.object(landing.subprocess, 'run') as invoke:
                    landing.paperwork()
                child = invoke.call_args.kwargs['env']
                self.assertNotIn('PLURX_PAPERWORK_CONTINUATION', child)
                self.assertNotIn('PLURX_PAPERWORK_COVERAGE_OUTPUT', child)
                record = json.loads(output.read_text().split('=', 1)[1])
                self.assertEqual(record['version'], 1); self.assertNotIn('unit_coverage', record)
            finally:
                os.chdir(previous)

    def test_preliminary_retained_coverage_never_claims_deferred_methods_passed(self):
        api, pull = self.facts(); value = self.value(); body = pc.PREFIX + json.dumps(value)
        with patch.object(pc.retention, 'runner_api', return_value=api), \
                patch.object(pc, 'git', return_value='b' * 40), \
                patch.object(pc, 'prepare', return_value=(self.scope(), pull, {}, {'old', 'new'}, {'old'}, {'new'})):
            result = pc.check({'PLURX_PAPERWORK_CONTINUATION': 'preliminary'}, body, 'b' * 40, '.')
        self.assertFalse(result['complete']); self.assertEqual(result['retained'], 1)
        self.assertEqual(result['deferred'], 1); self.assertNotIn('coverage', result)
        self.assertIsNone(pc.check({}, body, 'b' * 40, '.'))
        self.assertIsNone(pc.check({'PLURX_PAPERWORK_CONTINUATION': 'preliminary'}, 'ordinary PR', 'b' * 40, '.'))
