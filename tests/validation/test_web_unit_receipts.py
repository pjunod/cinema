"""Whole-command Node evidence must retain its original job and full inputs."""
import contextlib
import copy
import hashlib
import io
import json
import subprocess
import unittest
import zipfile
from unittest.mock import patch

from validation import web_unit_receipts as web
from validation.python_unit_receipts import ReceiptError


class WebUnitReceiptTests(unittest.TestCase):
    def test_source_membership_modes_and_reviewed_transition_are_bound(self):
        path = 'crates/plurxd/src/subtitle_translation/runtime.rs'
        old, new = b'old reviewed clock selector', b'new reviewed clock selector'
        trees = [{path: ('100644', 'blob', 'a')}, {path: ('100644', 'blob', 'b')}]
        pair = (hashlib.sha256(old).hexdigest(), hashlib.sha256(new).hexdigest())
        with patch.object(web, 'TRANSITIONS', {path: pair}), \
                patch.object(web, 'MAKEFILE_SHA256', hashlib.sha256(b'make').hexdigest()):
            with patch.object(web, 'source_inputs', side_effect=trees), \
                    patch.object(web.subprocess, 'check_output', side_effect=[old, new, b'make', b'make']):
                self.assertEqual(web.unchanged_inputs('old', 'new')['reviewed_transitions'], [path])
            for altered in [new + b' unrelated change']:
                with patch.object(web, 'source_inputs', side_effect=trees), \
                        patch.object(web.subprocess, 'check_output', side_effect=[old, altered]), \
                        self.assertRaises(ReceiptError):
                    web.unchanged_inputs('old', 'new')
            for changed in [{}, {path: ('100755', 'blob', 'b')},
                            {**trees[0], 'tests/web/new.test.js': ('100644', 'blob', 'new')}]:
                with patch.object(web, 'source_inputs', side_effect=[trees[0], changed]), self.assertRaises(ReceiptError):
                    web.unchanged_inputs('old', 'new')
        with patch.object(web, 'git', return_value='120000 blob abc\tclients/apple/UITests/link\0'), \
                self.assertRaises(ReceiptError):
            web.source_inputs('old')

    def test_original_success_job_and_own_execution_are_required(self):
        commit, scope = 'a' * 40, {'repository': 1, 'pr': 3}
        run = {'id': 1, 'repository': {'id': 1}, 'commit_sha': commit}
        job = {'id': 7, 'run_id': 1, 'repo_id': 1, 'attempt': 1, 'status': 'success',
               'name': web.JOB, 'runs_on': web.RUNS_ON}
        log = (f'node: v22.23.4\n{commit}:refs/remotes/pull/3/head\n'
               '# tests 5\n# pass 5\n# fail 0\n🏁  Job succeeded')
        class API:
            def get(self, path):
                return run
            def pages(self, path):
                return [job]
            def bytes(self, path):
                return log.encode()
        with patch.object(web, 'bind_event') as event, patch.object(web, 'commit_available', return_value=True), \
                patch.object(web, 'producer', return_value='legacy') as producer, \
                patch.object(web, 'unchanged_inputs', return_value={}), \
                patch.object(web.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0)):
            self.assertEqual(web.authenticate(API(), scope, 'current', 2, 1, 'suite', 'v22.23.4')['job'], 7)
            event.assert_called_once_with(run, scope)
            for field, value in [('status', 'failure'), ('attempt', 2), ('repo_id', 2), ('runs_on', [])]:
                original = job[field]
                job[field] = value
                with self.subTest(field=field), self.assertRaises(ReceiptError):
                    web.authenticate(API(), scope, 'current', 2, 1, 'suite', 'v22.23.4')
                job[field] = original
            original_log = log
            log = log.replace('node: v22.23.4\n', '')
            with self.assertRaises(ReceiptError):
                web.authenticate(API(), scope, 'current', 2, 1, 'suite', 'v22.23.4')
            log = original_log
            producer.return_value = 'wrapped'
            with self.assertRaises(ReceiptError):
                web.authenticate(API(), scope, 'current', 2, 1, 'suite', 'v22.23.4')
            log = log.replace('🏁  Job succeeded', f'WEB-UNIT-ORIGINAL-PASS suite {commit}\n🏁  Job succeeded')
            with patch.object(web, 'original_report', return_value={}):
                self.assertEqual(web.authenticate(API(), scope, 'current', 2, 1, 'suite', 'v22.23.4')['node'], 'v22.23.4')
        with patch.object(web, 'git', return_value='  web_compile:\n    run: true\n'), self.assertRaises(ReceiptError):
            web.producer('old', 'current')

        legacy = '  web_compile:\n    run: legacy\n'
        wrapped = '  web_compile:\n    run: wrapped\n'
        apple = '  apple_compile:\n    run: true\n'
        with patch.object(web, 'LEGACY_JOB_SHA256', hashlib.sha256(legacy.encode()).hexdigest()), \
                patch.object(web, 'WRAPPED_JOB_SHA256', hashlib.sha256(wrapped.encode()).hexdigest()), \
                patch.object(web, 'CURRENT_JOB_SHA256', hashlib.sha256(wrapped.encode()).hexdigest()):
            with patch.object(web, 'git', side_effect=[legacy + apple, wrapped + apple]):
                self.assertEqual(web.producer('old', 'current'), 'legacy')
            with patch.object(web, 'git', side_effect=[legacy + apple, wrapped.replace('wrapped', 'changed') + apple]), \
                    self.assertRaises(ReceiptError):
                web.producer('old', 'current')

    def test_committed_current_web_producer_and_inventory_are_self_compatible(self):
        current = web.git('rev-parse', 'HEAD')
        self.assertEqual(web.producer(current, current), 'wrapped')
        inputs = web.unchanged_inputs(current, current)
        self.assertEqual(inputs['original_input_digest'], inputs['current_input_digest'])
        self.assertEqual(inputs['reviewed_transitions'], [])

    def test_exact_pre_migration_originals_remain_compatible(self):
        prior = 'd2a28aba4b66449865c53c0f44a92251585cdbe2'
        current = web.git('rev-parse', 'HEAD')
        self.assertEqual(web.producer(prior, current), 'wrapped')
        # Freeze the original inventory check without requiring unrelated
        # future source edits to remain compatible with this historical run.
        self.assertEqual(web.unchanged_inputs(prior, prior)['reviewed_transitions'], [])

        original_git = web.git
        for target in ('producer', 'wrapper'):
            def altered(*args):
                raw = original_git(*args)
                if target == 'producer' and args == ('show', prior + ':' + web.WORKFLOW):
                    return raw.replace('        shell: bash', '        shell: sh')
                if target == 'wrapper' and args == (
                        'rev-parse', prior + ':validation/web_unit_receipts.py'):
                    return 'f' * 40
                return raw
            with self.subTest(target=target), patch.object(web, 'git', side_effect=altered), \
                    self.assertRaises(ReceiptError):
                web.producer(prior, current)

        original_output = subprocess.check_output

        def altered_wrapper_bytes(args, **kwargs):
            raw = original_output(args, **kwargs)
            if args == ['git', 'show', prior + ':validation/web_unit_receipts.py']:
                return raw + b'\n'
            return raw

        with patch.object(web.subprocess, 'check_output', side_effect=altered_wrapper_bytes), \
                self.assertRaises(ReceiptError):
            web.producer(prior, current)

    def test_wrapped_original_reports_round_trip_without_a_node_log_banner(self):
        # Captured CI8682 schema: the pinned image emits no setup-node banner.
        # Node identity comes from each executed wrapper's immutable report.
        commit = 'd2a28aba4b66449865c53c0f44a92251585cdbe2'
        scope = {'repository': 1, 'pr': 1640}
        run = {'id': 8682, 'repository': {'id': 1}, 'commit_sha': commit}
        job = {'id': 83066, 'run_id': 8682, 'repo_id': 1, 'attempt': 1,
               'status': 'success', 'name': web.JOB, 'runs_on': web.RUNS_ON}
        reports = {key + '.json': {'version': 1, 'scope': scope,
            'current_commit': commit, 'current_run': 8682, 'disposition': 'executed',
            'command': argv, 'returncode': 0, 'source_unchanged': True, 'node': 'v22.23.4'}
            for key, argv in web.COMMANDS.items()}

        def make_api(changed=None, archive_change=None, artifact_change=None, log_change=None):
            values = copy.deepcopy(reports)
            if changed:
                changed(values['suite.json'])
            members = [(name, json.dumps(value).encode()) for name, value in values.items()]
            if archive_change:
                members = archive_change(members)
            stream = io.BytesIO()
            with zipfile.ZipFile(stream, 'w', zipfile.ZIP_DEFLATED) as archive:
                for name, raw in members:
                    archive.writestr(name, raw)
            raw = stream.getvalue()
            artifacts = [{'id': 12292, 'name': 'web-unit-adoption-8682-1',
                          'run_id': 8682, 'expired': False, 'size_in_bytes': len(raw)}]
            if artifact_change:
                artifact_change(artifacts)
            log = (f'{commit}:refs/remotes/pull/1640/head\n'
                   '# tests 1070\n# pass 1070\n# fail 0\n'
                   + ''.join(f'WEB-UNIT-ORIGINAL-PASS {key} {commit}\n' for key in web.COMMANDS)
                   + f'Artifact web-unit-adoption-8682-1 has been successfully uploaded! '
                     f'Final size is {len(raw)} bytes. Artifact ID is 12292\n🏁  Job succeeded')
            if log_change:
                log = log_change(log)

            class API:
                def get(self, path):
                    assert path == '/actions/runs/8682'
                    return run
                def pages(self, path, query=None):
                    if path == '/actions/runs/8682/jobs':
                        return [job]
                    assert path == '/actions/artifacts' and query == {'name': 'web-unit-adoption-8682-1'}
                    return artifacts
                def bytes(self, path):
                    if path == '/actions/jobs/83066/logs':
                        return log.encode()
                    assert path == '/actions/artifacts/12292/zip'
                    return raw
            return API()

        with patch.object(web, 'bind_event'), patch.object(web, 'commit_available', return_value=True), \
                patch.object(web, 'producer', return_value='wrapped'), \
                patch.object(web, 'unchanged_inputs', return_value={}), \
                patch.object(web.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0)):
            for command in web.COMMANDS:
                proof = web.authenticate(make_api(), scope, 'current', 8683, 8682, command, 'v22.23.4')
                self.assertEqual(proof['artifact'], 12292)
                self.assertEqual(proof['node'], 'v22.23.4')
            for field, value in (
                ('version', 2), ('version', True), ('scope', {'repository': 2, 'pr': 1640}),
                ('current_commit', 'f' * 40), ('current_run', 8681),
                ('disposition', 'adopted'), ('command', ['true']), ('returncode', 1),
                ('returncode', False), ('source_unchanged', False), ('node', 'v22.24.0'),
            ):
                with self.subTest(field=field, value=value), self.assertRaises(ReceiptError):
                    web.authenticate(make_api(changed=lambda report: report.update({field: value})),
                                     scope, 'current', 8683, 8682, 'suite', 'v22.23.4')
            variants = [
                {'artifact_change': lambda items: items.append(dict(items[0]))},
                {'artifact_change': lambda items: items[0].update(expired=True)},
                {'artifact_change': lambda items: items[0].update(size_in_bytes=65_537)},
                {'artifact_change': lambda items: items[0].update(run_id=8681)},
                {'archive_change': lambda members: members + [members[0]]},
                {'archive_change': lambda members: [('../suite.json', members[0][1]), *members[1:]]},
                {'archive_change': lambda members: [(name, b'{bad') for name, _ in members]},
                {'archive_change': lambda members: [(name, b'{"version":1,"version":1}') for name, _ in members]},
                {'log_change': lambda log: log.replace('WEB-UNIT-ORIGINAL-PASS suite ', 'WEB-UNIT-ADOPTED suite ')},
                {'log_change': lambda log: log.replace('Artifact web-unit-adoption-', 'Missing web-unit-adoption-')},
            ]
            for variant in variants:
                with self.subTest(variant=list(variant)), self.assertRaises((ReceiptError, ValueError)):
                    web.authenticate(make_api(**variant), scope, 'current', 8683, 8682, 'suite', 'v22.23.4')

    def test_command_router_falls_back_and_never_marks_adoption_as_original(self):
        with patch.object(web, 'api_context', return_value=(None, {}, 'current', 2)), \
                patch.object(web.subprocess, 'check_output', return_value='v22.23.4'), \
                patch.object(web, 'candidate_runs', return_value=[1]), \
                patch.object(web, 'authenticate', side_effect=ReceiptError('Unknown original')) as authenticate, \
                patch.object(web, 'git', side_effect=['current', '']), patch.object(web, 'write_report') as report, \
                patch.object(web.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0)) as execute:
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(web.main(['--command', 'player-typedef']), 0)
            execute.assert_called_once_with(['node', 'tests/web/player-typedef.test.js'])
            self.assertIn('WEB-UNIT-ORIGINAL-PASS player-typedef current', output.getvalue())
            self.assertEqual(report.call_args.args[1]['disposition'], 'executed')
            authenticate.side_effect = None
            authenticate.return_value = {'job': 7}
            execute.reset_mock()
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(web.main(['--command', 'player-typedef']), 0)
            execute.assert_not_called()
            self.assertIn('WEB-UNIT-ADOPTED player-typedef', output.getvalue())
            self.assertNotIn('WEB-UNIT-ORIGINAL-PASS', output.getvalue())
            self.assertEqual(report.call_args.args[1]['disposition'], 'adopted')
