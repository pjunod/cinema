"""One missing packing case gets pinned bytes; no default or global install."""
import io
import json
import os
import unittest
from unittest.mock import patch
import warnings
import zipfile
from validation import selected_python_dependencies as d
from validation import native_python_execute as native
from validation import python_unit_receipts as r


class SelectedPythonDependencyTests(unittest.TestCase):
    def plan(self):
        return {'source_commit': 'a' * 40, 'comparison': {'base_sha': 'b' * 40, 'merge_target': 'b' * 40},
                'suites': {'operations': {'uncovered': [d.CASE.split(':', 1)[1]]}}}

    def test_only_the_exact_uncovered_packing_source_requests_numpy(self):
        raw = b'reviewed packing test'
        with patch.object(d.retention, 'git_bytes', return_value=raw), \
                patch.object(d, 'TEST_SHA256', d.retention.digest(raw)):
            self.assertEqual(d.requirements(self.plan(), 'a' * 40), d.PROFILE)
            legacy = self.plan(); legacy.pop('comparison')
            self.assertIsNone(d.requirements(legacy, 'a' * 40))
            covered = self.plan(); covered['suites']['operations']['uncovered'] = []
            self.assertIsNone(d.requirements(covered, 'a' * 40))
        with patch.object(d.retention, 'git_bytes', return_value=b'changed source'):
            with self.assertRaises(r.ReceiptError):
                d.requirements(self.plan(), 'a' * 40)

    def wheel(self, names):
        output = io.BytesIO()
        with warnings.catch_warnings():
            warnings.simplefilter('ignore', UserWarning)
            with zipfile.ZipFile(output, 'w') as archive:
                for name in names:
                    archive.writestr(name, b'fixture')
        return output.getvalue()

    def test_pinned_wheel_refuses_corruption_escape_and_duplicate_members(self):
        names = [f'numpy/file_{index}.py' for index in range(876)]
        raw = self.wheel(names)
        hashes = {name: d.retention.digest(b'fixture') for name in names}
        profile = {**d.PROFILE, 'wheel_sha256': d.retention.digest(raw),
                   'tree_sha256': d.retention.digest(json.dumps(hashes, sort_keys=True, separators=(',', ':')).encode())}
        with patch.object(d, 'SIZE', len(raw)), patch.object(d, 'PROFILE', profile):
            self.assertEqual(set(d.archive_files(raw)), set(names))
            with self.assertRaises(r.ReceiptError):
                d.archive_files(raw[:-1] + b'x')
        for names, message in ((['../escape'], 'Unsafe'), (['numpy/a.py', 'numpy/a.py'], 'Repeated')):
            changed = self.wheel(names)
            with patch.object(d, 'SIZE', len(changed)), \
                    patch.dict(d.PROFILE, wheel_sha256=d.retention.digest(changed)), \
                    self.assertRaisesRegex(r.ReceiptError, message):
                d.archive_files(changed)

    def test_runtime_identity_rejects_wrong_profile_interpreter_and_package(self):
        value = {'profile': dict(d.PROFILE), 'root': '/owned/selected-python-numpy-fixture',
                 'python_sha256': 'a' * 64, 'python_version': '3.12.12', 'python_implementation': 'cpython',
                 'platform': 'linux', 'machine': 'x86_64', 'numpy_version': '2.2.6',
                 'numpy_file': 'numpy/__init__.py', 'tree_sha256': d.PROFILE['tree_sha256'], 'openblas_threads': 1}
        d.validate_runtime(value, d.PROFILE)
        for changed in (None, {**value, 'machine': 'aarch64'}, {**value, 'numpy_version': 'unbound'},
                        {**value, 'python_version': '3.15.0'}, {**value, 'tree_sha256': 'b' * 64},
                        {**value, 'numpy_file': '../foreign.py'}):
            with self.subTest(value=changed), self.assertRaises(r.ReceiptError):
                d.validate_runtime(changed, d.PROFILE)
        with self.assertRaises(r.ReceiptError):
            d.validate_runtime(value, None)
        d.validate_runtime(None, None)

    def test_missing_dependency_eligibility_refuses_before_any_selection_is_published_or_run(self):
        plan = {**self.plan(), 'dependency_profile': d.PROFILE}
        env = {'GITHUB_SHA': 'a' * 40, 'PROMOTION_BINDING': '{}', 'PYTHON_UNIT_EVIDENCE': '{}'}
        with patch.dict(os.environ, env), patch.object(native.subprocess, 'check_output', return_value='a' * 40), \
                patch.object(native.retention, 'runner_api'), patch.object(native, 'prepare', return_value=plan), \
                patch.object(d, 'stage', side_effect=r.ReceiptError('ineligible import')) as stage, \
                patch.object(native.receipts, 'atomic_json') as publish, patch.object(native, 'run_selection') as run:
            with self.assertRaisesRegex(r.ReceiptError, 'ineligible import'):
                native.main('prepare')
        stage.assert_called_once(); publish.assert_not_called(); run.assert_not_called()
