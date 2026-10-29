"""Whole-command Node evidence must retain its original job and full inputs."""
import contextlib
import hashlib
import io
import subprocess
import unittest
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
            producer.return_value = 'wrapped'
            with self.assertRaises(ReceiptError):
                web.authenticate(API(), scope, 'current', 2, 1, 'suite', 'v22.23.4')
            log = log.replace('🏁  Job succeeded', f'WEB-UNIT-ORIGINAL-PASS suite {commit}\n🏁  Job succeeded')
            self.assertEqual(web.authenticate(API(), scope, 'current', 2, 1, 'suite', 'v22.23.4')['node'], 'v22.23.4')
        with patch.object(web, 'git', return_value='  web_compile:\n    run: true\n'), self.assertRaises(ReceiptError):
            web.producer('old', 'current')

        legacy = '  web_compile:\n    run: legacy\n'
        wrapped = '  web_compile:\n    run: wrapped\n'
        apple = '  apple_compile:\n    run: true\n'
        with patch.object(web, 'LEGACY_JOB_SHA256', hashlib.sha256(legacy.encode()).hexdigest()), \
                patch.object(web, 'WRAPPED_JOB_SHA256', hashlib.sha256(wrapped.encode()).hexdigest()):
            with patch.object(web, 'git', side_effect=[legacy + apple, wrapped + apple]):
                self.assertEqual(web.producer('old', 'current'), 'legacy')
            with patch.object(web, 'git', side_effect=[legacy + apple, wrapped.replace('wrapped', 'changed') + apple]), \
                    self.assertRaises(ReceiptError):
                web.producer('old', 'current')

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
