"""Trust, input compatibility, and bounded native artifact retention."""
import copy
import io
import hashlib
import json
import unittest
from unittest.mock import patch
import urllib.request
import zipfile

from validation import native_receipts as native
from validation.python_unit_receipts import ReceiptError


class NativeReceiptTests(unittest.TestCase):
    def test_original_exact_suite_and_clean_snapshot_required(self):
        receipt = {'schema': 1, 'source_sha': 'a' * 40, 'source_dirty': False,
            'source_tree_sha256': 'digest', 'required_tests': ['Suite/testOne'],
            'selected_tests': ['Suite/testOne'], 'status': 'passed',
            'native_suite_complete': True, 'xcode': native.XCODE, 'runtime': native.RUNTIME,
            'test_summary': {'passedTests': 1, 'failedTests': 0, 'skippedTests': 0},
            'command': ['xcodebuild', 'test', '-only-testing:plurx-tvOSUITests/Suite/testOne']}
        with patch.object(native, 'required_tests', return_value=['Suite/testOne']), \
                patch.object(native, 'full_source_digest', return_value='digest'):
            native.verify_tv(receipt, 'a' * 40)
            for key, value in [('source_dirty', True), ('status', 'failed'),
                    ('source_sha', 'b' * 40), ('source_tree_sha256', 'wrong'),
                    ('selected_tests', []), ('required_tests', ['Other/test']),
                    ('native_suite_complete', False), ('runtime', 'other'), ('xcode', 'other'),
                    ('test_summary', {'passedTests': 1, 'failedTests': 0, 'skippedTests': 1}),
                    ('command', ['xcodebuild', 'test'])]:
                with self.subTest(key=key), self.assertRaises(ReceiptError):
                    native.verify_tv({**receipt, key: value}, 'a' * 40)

    def test_archive_crc_duplicates_traversal_and_expansion_rejected(self):
        def archive(names):
            stream = io.BytesIO()
            with zipfile.ZipFile(stream, 'w', zipfile.ZIP_STORED) as output:
                for name, data in names:
                    output.writestr(name, data)
            stream.seek(0)
            return stream
        self.assertEqual(native.archive_member(archive([('receipt.json', b'{}')]), 'receipt.json'), b'{}')
        for names in [[('../receipt.json', b'{}')], [('receipt.json', b'{}'), ('/bad', b'')],
                [('receipt.json', b'{}'), ('receipt.json', b'{}')]]:
            with self.subTest(names=names), self.assertRaises(ReceiptError):
                native.archive_member(archive(names), 'receipt.json')
        with patch.object(native, 'MAX_EXPANDED', 1), self.assertRaises(ReceiptError):
            native.archive_member(archive([('receipt.json', b'{}')]), 'receipt.json')
        stream = archive([('receipt.json', b'{"field":1}')])
        corrupt = stream.getvalue().replace(b'{"field":1}', b'{"field":2}')
        with self.assertRaises(zipfile.BadZipFile):
            native.archive_member(io.BytesIO(corrupt), 'receipt.json')

    def test_redirect_drops_auth_even_on_same_origin(self):
        request = urllib.request.Request('https://forge.invalid/a', headers={
            'Authorization': 'token secret', 'Cookie': 'secret'})
        redirected = native.PublicRedirect().redirect_request(
            request, None, 302, '', {}, 'https://forge.invalid/b?signature=example')
        self.assertIsNone(redirected.get_header('Authorization'))
        self.assertIsNone(redirected.get_header('Cookie'))
        with self.assertRaises(ReceiptError):
            native.PublicRedirect().redirect_request(request, None, 302, '', {}, 'file:///etc/passwd')

    def test_legacy_native_producer_is_exact_and_cannot_hide_failed_commands(self):
        with patch.object(native, 'git', side_effect=['  apple_compile:\n    fake: true\n', '  apple_compile:\n    different: true\n']):
            with self.assertRaises(ReceiptError):
                native.producer('a' * 40, 'b' * 40)
        with self.assertRaises(ReceiptError):
            native.native_block('  other_job:\n    run: make apple-tv-navigation-test\n')

    def test_unchanged_input_proof_is_per_family(self):
        with patch.object(native, 'build_specs'), patch.object(native, 'regular_inputs'), \
                patch.object(native, 'input_digest', side_effect=['same', 'same']) as digest:
            self.assertTrue(native.unchanged('old', 'new', 'tv_navigation'))
            self.assertIn('tests/contracts/native-api.json', digest.call_args.args[1]['inputs'])
        with patch.object(native, 'build_specs'), patch.object(native, 'regular_inputs'), \
                patch.object(native, 'input_digest', side_effect=['old', 'new']):
            self.assertFalse(native.unchanged('old', 'new', 'desktop_macos'))

    def test_failed_origin_and_changed_inputs_never_download_or_adopt(self):
        artifact = {'id': 1, 'run_id': 1, 'name': 'tv-navigation-1-1'}
        with patch.object(native, 'origin', side_effect=ReceiptError('failed original job')), \
                patch.object(native, 'download') as download:
            self.assertEqual(native.adopt(None, {}, 'current', 2, [artifact]), {})
            download.assert_not_called()
        with patch.object(native, 'origin', return_value={'commit': 'old'}), \
                patch.object(native, 'unchanged', return_value=False), \
                patch.object(native, 'download') as download:
            self.assertEqual(native.adopt(None, {}, 'current', 2, [artifact]), {})
            download.assert_not_called()

    def test_desktop_requires_nonempty_xctest_pass(self):
        native.verify_desktop(b"Test Suite 'All tests' passed\nExecuted 61 tests, with 0 failures (0 unexpected)")
        for log in [b'0 tests passed', b"Test Suite 'All tests' passed\nExecuted 0 tests, with 0 failures (0 unexpected)",
                    b"Test Suite 'All tests' passed\nExecuted 61 tests, with 0 failures (0 unexpected)\nTest Case 'x' failed"]:
            with self.subTest(log=log), self.assertRaises(ReceiptError):
                native.verify_desktop(log)

    def test_origin_api_binds_ready_event_job_publication_and_merge_ancestry(self):
        commit = 'a' * 40
        event = {'number': 12}
        run = {'id': 10, 'repository': {'id': 1}, 'event_payload': json.dumps(event), 'commit_sha': commit}
        pull = {'merged': True, 'state': 'closed', 'head': {'sha': commit, 'repo': {'id': 1}},
                'base': {'repo': {'id': 1}, 'ref': 'main'}, 'merge_commit_sha': 'b' * 40}
        job = {'id': 8, 'run_id': 10, 'repo_id': 1, 'attempt': 1, 'status': 'success',
               'name': native.JOB, 'runs_on': native.RUNS_ON}
        artifact = {'id': 3, 'name': 'tv-navigation-10-1', 'run_id': 10, 'expired': False}
        log = (commit + ':refs/remotes/pull/12/head\nArtifact tv-navigation-10-1 has been successfully uploaded!\n' + native.XCODE).encode()
        class API:
            def get(self, path):
                return copy.deepcopy(pull if '/pulls/' in path else run)
            def pages(self, path):
                return [copy.deepcopy(job)]
            def bytes(self, path):
                return log
        with patch.object(native, 'bind_event') as bind, \
                patch.object(native, 'commit_available', return_value=True), \
                patch.object(native, 'producer'), \
                patch.object(native.subprocess, 'run') as command:
            command.return_value.returncode = 0
            record = native.origin(API(), artifact, {'repository': 1, 'pr': 13}, 'current', 20)
            self.assertEqual(record['commit'], commit)
            bind.assert_called_once_with(run, {'repository': 1, 'pr': 12})
            self.assertEqual(command.call_count, 2)
            for key, value in [('status', 'failure'), ('attempt', 2), ('repo_id', 2), ('runs_on', [])]:
                saved = job[key]
                job[key] = value
                with self.subTest(key=key), self.assertRaises(ReceiptError):
                    native.origin(API(), artifact, {'repository': 1, 'pr': 13}, 'current', 20)
                job[key] = saved
            pull['merged'] = False
            with self.assertRaises(ReceiptError):
                native.origin(API(), artifact, {'repository': 1, 'pr': 13}, 'current', 20)
            pull['merged'] = True
            command.return_value.returncode = 1
            with self.assertRaises(ReceiptError):
                native.origin(API(), artifact, {'repository': 1, 'pr': 13}, 'current', 20)

    def test_closure_binds_file_additions_removals_and_modes(self):
        from validation import main_preflight_adoption as preflight
        original = '100644 blob ' + 'a' * 40 + '\tclients/apple/Test.swift\0'
        cases = [original + '100644 blob ' + 'b' * 40 + '\tclients/apple/New.swift\0',
                 '', original.replace('100644', '100755')]
        for index, changed in enumerate(cases):
            old, new = f'old-{index}', f'new-{index}'
            with self.subTest(changed=changed), patch.object(native, 'build_specs'), \
                    patch.object(native, 'regular_inputs'), patch.object(preflight, '_TREE_CACHE', {}), \
                    patch.object(preflight, '_DIGEST_CACHE', {}), \
                    patch.object(preflight, 'git', side_effect=[original, changed]):
                self.assertFalse(native.unchanged(old, new, 'tv_navigation'))

    def test_large_artifact_download_is_bounded_and_stays_local(self):
        from pathlib import Path
        import tempfile
        class API:
            root, token = 'https://forge.invalid/api/v1/repos/owner/repo', 'secret'
        class Response(io.BytesIO):
            pass
        class Opener:
            def open(self, request, timeout):
                return Response(b'x' * 9)
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(native, 'REPORT', Path(directory) / 'receipt.json'), \
                patch.object(native, 'MAX_ARCHIVE', 8), \
                patch.object(native.urllib.request, 'build_opener', return_value=Opener()):
            with self.assertRaises(ReceiptError):
                native.download(API(), {'id': 1, 'size_in_bytes': 9})
            with self.assertRaises(ReceiptError):
                native.download(API(), {'id': 1, 'size_in_bytes': 8})

    def test_build_specs_reject_quoted_scalar_package_and_script_extensions(self):
        original = b'sources:\n  - path: Sources\n'
        digest = hashlib.sha256(original).hexdigest()
        for family in native.FAMILY_SPECS:
            with self.subTest(family=family), patch.dict(native.BUILD_SPECS,
                    {path: digest for path in native.FAMILY_SPECS[family]}):
                with patch.object(native.subprocess, 'check_output', return_value=original):
                    native.build_specs('origin', family)
                for extension in [b'  - path: "../../extra/F.swift"\n',
                        b'sources: [../../extra/F.swift]\n', b'packages: {Extra: ../Extra}\n',
                        b'preBuildScripts: [{script: cat ../../extra/file}]\n']:
                    with self.subTest(extension=extension), \
                            patch.object(native.subprocess, 'check_output', return_value=original + extension), \
                            self.assertRaises(ReceiptError):
                        native.build_specs('origin', family)

    def test_equal_symlink_and_gitlink_inputs_cannot_supply_unchanged_proof(self):
        for family, path in [('tv_navigation', 'tests/contracts/native-api.json'),
                             ('desktop_macos', 'clients/server-tray/macos/Resources'),
                             ('tv_navigation', 'clients/apple'), ('desktop_macos', 'clients')]:
            for mode, kind in [('120000', 'blob'), ('160000', 'commit')]:
                tree = f'{mode} {kind} ' + 'a' * 40 + '\t' + path + '\0'
                for unsafe_source in ('origin', 'current', 'both'):
                    def source_tree(*args):
                        return tree if unsafe_source == 'both' or args[-1] == unsafe_source else ''
                    with self.subTest(family=family, path=path, mode=mode, source=unsafe_source), \
                            patch.object(native, 'build_specs'), \
                            patch.object(native, 'git', side_effect=source_tree), \
                            patch.object(native, 'input_digest', return_value='equal') as digest, \
                            self.assertRaises(ReceiptError):
                        native.unchanged('origin', 'current', family)
                    digest.assert_not_called()

    def test_bounded_discovery_retains_candidates_at_page_limit_and_api_failure(self):
        native_artifact = {'id': 1, 'run_id': 10, 'name': 'tv-navigation-10-1', 'expired': False}
        other = {'id': 2, 'run_id': 11, 'name': 'unrelated-journal', 'expired': False}
        class API:
            def __init__(self, failure=False):
                self.calls, self.failure = [], failure
            def get(self, path, query):
                self.calls.append((path, query))
                if query['page'] == 1:
                    return [native_artifact] + [other] * 49
                if self.failure:
                    raise ReceiptError('page unavailable')
                return [other] * 50  # More than 1,000 unrelated retained artifacts.
        for failure, expected_calls in [(False, native.MAX_ARTIFACT_PAGES), (True, 2)]:
            api = API(failure)
            with self.subTest(failure=failure):
                self.assertEqual(native.native_artifacts(api, 20), [native_artifact])
                self.assertEqual(len(api.calls), expected_calls)
                self.assertTrue(all(path == '/actions/artifacts' and query['limit'] == 50
                                    for path, query in api.calls))

    def test_discovery_caps_deduplicates_and_ignores_ineligible_metadata(self):
        eligible = [{'id': i, 'run_id': i, 'name': f'tv-navigation-{i}-1', 'expired': False}
                    for i in range(1, native.MAX_CANDIDATES + 1)]
        class API:
            calls = 0
            def get(self, path, query):
                self.calls += 1
                return [eligible[0], {'id': 30, 'run_id': 'bad', 'name': 'tv-navigation-30-1'},
                        {**eligible[0], 'id': 31, 'expired': True},
                        {**eligible[0], 'id': 32, 'run_id': 100}, *eligible]
        api = API()
        self.assertEqual(native.native_artifacts(api, 100), eligible)
        self.assertEqual(api.calls, 1)

    def test_original_pass_adopts_build_and_test_operations_with_original_provenance(self):
        for family, name, operations in [
                ('tv_navigation', 'tv-navigation-10-1', ['make apple-build', 'make apple-tv-navigation-test']),
                ('desktop_macos', 'desktop-macos-check-10-1', ['make desktop-macos-check'])]:
            artifact = {'id': 3, 'run_id': 10, 'name': name}
            origin = {'commit': 'original', 'run': 10, 'job': 8, 'artifact': 3,
                      'environment_contract': {}}
            with self.subTest(family=family), patch.object(native, 'origin', return_value=origin), \
                    patch.object(native, 'unchanged', return_value=True), \
                    patch.object(native, 'download', return_value=io.BytesIO()), \
                    patch.object(native, 'archive_member', return_value=b'{}'), \
                    patch.object(native, 'verify_tv'), patch.object(native, 'verify_desktop'), \
                    patch.object(native, 'input_digest', return_value='inputs'):
                record = native.adopt(None, {}, 'current', 20, [artifact])[family]
                self.assertEqual(record['operations'], operations)
                self.assertEqual(record['commit'], 'original')
                self.assertEqual(record['run'], 10)
                self.assertEqual(record['disposition'], 'adopted')
                self.assertNotIn('status', record)
