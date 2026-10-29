"""Trust, input compatibility, and bounded native artifact retention."""
import copy
import io
import hashlib
import json
from pathlib import Path
import unittest
from unittest.mock import patch, mock_open
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
            job['status'] = 'failure'
            self.assertEqual(native.origin(API(), artifact, {'repository': 1, 'pr': 13},
                                           'current', 20, tv_cases=True)['job'], 8)
            job['status'] = 'success'
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
                    patch.object(native, 'original_desktop_member', return_value=b'{}'), \
                    patch.object(native, 'verify_tv'), patch.object(native, 'verify_desktop'), \
                    patch.object(native, 'input_digest', return_value='inputs'):
                record = native.adopt(None, {}, 'current', 20, [artifact])[family]
                self.assertEqual(record['operations'], operations)
                self.assertEqual(record['commit'], 'original')
                self.assertEqual(record['run'], 10)
                self.assertEqual(record['disposition'], 'adopted')
                self.assertNotIn('status', record)


    def test_family_selection_is_closed_and_default_all_remains_available(self):
        self.assertEqual(native.selected_families(),tuple(native.INPUTS))
        self.assertEqual(native.selected_families(['tv_navigation']),('tv_navigation',))
        for value in ([],['unknown'],['tv_navigation','tv_navigation'],'tv_navigation',[True]):
            with self.subTest(value=value),self.assertRaises(ReceiptError):native.selected_families(value)

    def test_tv_only_discovery_filters_desktop_before_candidate_limit(self):
        desktop=[{'id':i,'run_id':i,'name':f'desktop-macos-check-{i}-1','expired':False}
                 for i in range(1,native.MAX_CANDIDATES+2)]
        tv={'id':80,'run_id':80,'name':'tv-navigation-80-1','expired':False}
        class API:
            def get(self,path,query):return [*desktop,tv]
        self.assertEqual(native.native_artifacts(API(),100,families=('tv_navigation',)),[tv])
        with patch.object(native,'origin') as origin:
            self.assertEqual(native.adopt(None,{},'current',100,desktop,families=('tv_navigation',)),{})
            origin.assert_not_called()

    def test_generic_desktop_rejects_dedicated_retained_container(self):
        def packed(extra):
            stream=io.BytesIO()
            with zipfile.ZipFile(stream,'w',zipfile.ZIP_STORED) as archive:
                archive.writestr('desktop-macos-check.log',b'original native log')
                for name,value in extra:archive.writestr(name,value)
            stream.seek(0);return stream
        self.assertEqual(native.original_desktop_member(packed([])),b'original native log')
        for extra in ([('receipt.json',b'{"current_native_commands_executed":false}')],
                      [('origin-artifact.zip',b'old'),('origin-job.log',b'old')],
                      [('fresh-start.json',b'{}'),('fresh-finish.json',b'{}')]):
            with self.subTest(extra=extra),self.assertRaises(ReceiptError):native.original_desktop_member(packed(extra))


class NativeCaseReceiptTests(unittest.TestCase):
    def test_current_case_specs_match_reviewed_boundary_and_refuse_extensions(self):
        root = Path(__file__).resolve().parents[2]
        specs = native.FAMILY_SPECS['tv_navigation']
        source = {path: (root / path).read_bytes() for path in specs}
        tree = ''.join('100644 blob ' + 'a' * 40 + '\t' + path + '\0' for path in specs)
        project = 'clients/apple/project.yml'
        extension = b''

        def show(command, timeout):
            self.assertEqual(command[:2], ['git', 'show'])
            self.assertEqual(len(command), 3)
            self.assertEqual(timeout, 10)
            commit, path = command[2].split(':', 1)
            self.assertIn(commit, ('original', 'current'))
            self.assertIn(path, specs)
            return source[path] + (extension if path == project else b'')

        with patch.object(native, 'regular_inputs'), \
                patch.object(native, 'git', return_value=tree), \
                patch.object(native.subprocess, 'check_output', side_effect=show):
            self.assertEqual(native.case_input_compatibility('original', 'current'), set())
            # Equal inputs still need the independently reviewed build boundary.
            for extension in [b'\n  - path: "../../extra/F.swift"\n',
                    b'\npackages: {Extra: ../Extra}\n',
                    b'\npreBuildScripts: [{script: cat ../../extra/file}]\n']:
                with self.subTest(extension=extension), self.assertRaisesRegex(
                        ReceiptError, '^Unreviewed native build specification: clients/apple/project[.]yml$'):
                    native.case_input_compatibility('original', 'current')

    def receipt(self):
        selected = ['Suite/testOne', 'Suite/testTwo']
        return {'schema': 2, 'source_sha': 'a' * 40, 'source_dirty': False,
            'source_tree_sha256': 'digest', 'required_tests': selected, 'selected_tests': selected,
            'status': 'failed', 'xcode': native.XCODE, 'runtime': native.RUNTIME,
            'source_unchanged': True,
            'finished_source': {'sha': 'a' * 40, 'dirty': False, 'tree_sha256': 'digest'},
            'command': ['xcodebuild', 'test', *['-only-testing:plurx-tvOSUITests/' + test for test in selected]],
            'test_summary': {'passedTests': 1, 'failedTests': 1, 'skippedTests': 0},
            'case_results': {'Suite/testOne': 'passed', 'Suite/testTwo': 'failed'}}

    def log(self):
        return ("Test Case '-[plurx_tvOSUITests.Suite testOne]' started.\n"
                "Test Case '-[plurx_tvOSUITests.Suite testOne]' passed (1.234 seconds).\n"
                "Test Case '-[plurx_tvOSUITests.Suite testTwo]' started.\n"
                "Test Case '-[plurx_tvOSUITests.Suite testTwo]' failed (2.345 seconds).\n")

    def test_failed_suite_retains_only_own_named_passes(self):
        receipt = self.receipt()
        receipt['suite_passes'] = {'Suite/testTwo': {'forged': True}}
        with patch.object(native, 'required_tests', return_value=receipt['required_tests']), \
                patch.object(native, 'full_source_digest', return_value='digest'):
            self.assertEqual(native.verify_tv_cases(receipt, self.log(), 'a' * 40), {'Suite/testOne'})
            for field, value in [('source_unchanged', False),
                    ('test_summary', {'passedTests': 2, 'failedTests': 0, 'skippedTests': 0}),
                    ('case_results', {'Suite/testOne': 'passed', 'Suite/testTwo': 'passed'})]:
                with self.subTest(field=field), self.assertRaises(ReceiptError):
                    native.verify_tv_cases({**receipt, field: value}, self.log(), 'a' * 40)

    def test_case_parser_rejects_duplicate_unstarted_and_unknown_events(self):
        selected = self.receipt()['selected_tests']
        for log in [self.log() + self.log(), self.log().split('\n', 1)[1],
                    self.log().replace('testOne', 'testUnexpected'),
                    self.log().replace('passed (1.234 seconds).', 'passed without a native duration')]:
            with self.subTest(log=log[:100]), self.assertRaises(ReceiptError):
                native.case_outcomes(log, selected)

    def test_legacy_failed_source_requires_the_exact_reviewed_full_producer(self):
        receipt = self.receipt()
        receipt['schema'] = 1
        producer = b'reviewed producer'
        with patch.object(native, 'required_tests', return_value=receipt['required_tests']), \
                patch.object(native, 'full_source_digest', return_value='digest'), \
                patch.object(native.subprocess, 'check_output', return_value=producer), \
                patch.dict(native.BUILD_SPECS, {'scripts/apple-tv-navigation-test': hashlib.sha256(producer).hexdigest()}):
            self.assertEqual(native.verify_tv_cases(receipt, self.log(), 'a' * 40), {'Suite/testOne'})
            receipt['source_tree_sha256'] = 'wrong'
            with self.assertRaises(ReceiptError):
                native.verify_tv_cases(receipt, self.log(), 'a' * 40)

    def test_fixture_compatibility_accepts_only_the_exact_reviewed_byte_pair(self):
        path = 'clients/apple/UITests/TVNavigationTestCase.swift'
        old, new = b'old fixture', b'reviewed fixture'
        pair = (hashlib.sha256(old).hexdigest(), hashlib.sha256(new).hexdigest())
        trees = [f'100644 blob {oid}\t{path}\0' for oid in ('a' * 40, 'b' * 40)]
        with patch.object(native, 'regular_inputs'), \
                patch.object(native, 'FAMILY_SPECS', {'tv_navigation': []}), \
                patch.object(native, 'FIXTURE_TRANSITIONS', {path: pair}):
            with patch.object(native, 'git', side_effect=trees), \
                    patch.object(native.subprocess, 'check_output', side_effect=[old, new]):
                self.assertEqual(native.case_input_compatibility('old', 'new'), native.TRANSITION_INVALIDATES)
            with patch.object(native, 'git', side_effect=trees), \
                    patch.object(native.subprocess, 'check_output', side_effect=[old, new + b' extra']), \
                    self.assertRaises(ReceiptError):
                native.case_input_compatibility('old', 'new')
            with patch.object(native, 'git', side_effect=[trees[0], trees[1].replace('100644', '100755')]), \
                    self.assertRaises(ReceiptError):
                native.case_input_compatibility('old', 'new')

    def test_local_resume_plan_requires_reauthenticated_original_records(self):
        plan = {'scope': {'pr': 1}, 'current_commit': 'current', 'current_run': 2,
                'tv_cases': {'passes': {'Suite/testOne': {'forged': True}}, 'pending': []}}
        with patch.object(native, 'api_context', return_value=(None, {'pr': 1}, 'current', 2)), \
                patch.object(native, 'safe_storage'), patch.object(native, 'REPORT') as report, \
                patch.object(native, 'native_artifacts', return_value=[]), \
                patch.object(native, 'adopt_tv_cases', return_value={'passes': {}, 'pending': ['Suite/testOne']}):
            report.read_bytes.return_value = json.dumps(plan).encode()
            with self.assertRaises(ReceiptError):
                native.main(['--verify-tv-plan'])


    def test_resume_verifies_each_exact_origin_once_without_new_discovery(self):
        sources = {'Suite/testOne': {'artifact': 7}, 'Suite/testTwo': {'artifact': 7},
                   'Suite/testThree': {'artifact': 9}}
        cases = {'required_tests': list(sources), 'passes': sources, 'pending': []}
        plan = {'scope': {'pr': 1}, 'current_commit': 'current', 'current_run': 2, 'tv_cases': cases}
        class API:
            def __init__(self):
                self.paths = []
            def get(self, path):
                self.paths.append(path)
                return {'id': int(path.rsplit('/', 1)[1])}
        api = API()
        with patch.object(native, 'api_context', return_value=(api, {'pr': 1}, 'current', 2)), \
                patch.object(native, 'safe_storage'), patch.object(native, 'REPORT') as report, \
                patch.object(native, 'native_artifacts', side_effect=AssertionError('No new discovery')), \
                patch.object(native, 'adopt_tv_cases', return_value=cases) as adopt:
            report.read_bytes.return_value = json.dumps(plan).encode()
            native.main(['--verify-tv-plan'])
            self.assertEqual(api.paths, ['/actions/artifacts/7', '/actions/artifacts/9'])
            self.assertEqual(adopt.call_args.args[-1], [{'id': 7}, {'id': 9}])
            adopt.return_value = {**cases, 'passes': {}}
            with self.assertRaises(ReceiptError):
                native.main(['--verify-tv-plan'])

    def test_exact_origin_lookup_rejects_wrong_identity_and_missing_artifacts(self):
        cases = {'required_tests': ['Suite/testOne'], 'passes': {'Suite/testOne': {'artifact': 7}}}
        class API:
            def get(self, path):
                return {'id': 8}
        with self.assertRaises(ReceiptError):
            native.planned_artifacts(API(), cases)
        with patch.object(API, 'get', side_effect=ReceiptError('Missing original artifact')), \
                self.assertRaises(ReceiptError):
            native.planned_artifacts(API(), cases)
        with patch.object(API, 'get', side_effect=AssertionError('Empty plan needs no lookup')):
            self.assertEqual(native.planned_artifacts(API(), {'required_tests': [], 'passes': {}}), [])


    def test_desktop_only_never_plans_tv_cases_or_publishes_tv_fields(self):
        with patch.object(native, 'api_context', return_value=(None, {'pr': 1}, 'current', 2)), \
                patch.object(native, 'native_artifacts', return_value=[]) as discovery, \
                patch.object(native, 'adopt', return_value={}) as adoption, \
                patch.object(native, 'adopt_tv_cases') as cases, \
                patch.object(native, 'required_tests') as required, \
                patch.object(native, 'safe_storage'), patch.object(native, 'atomic_json') as report, \
                patch.dict(native.os.environ, {'GITHUB_OUTPUT': '/owned/output'}), \
                patch('builtins.open', mock_open()):
            native.main(['--family', 'desktop_macos'])
            self.assertEqual(discovery.call_args.kwargs['families'], ('desktop_macos',))
            self.assertEqual(adoption.call_args.kwargs['families'], ('desktop_macos',))
            cases.assert_not_called()
            required.assert_not_called()
            self.assertNotIn('tv_cases', report.call_args.args[1])

    def test_tv_plan_verification_refuses_desktop_only_before_api_or_plan_read(self):
        with patch.object(native, 'api_context') as api, patch.object(native, 'planned_artifacts') as planned:
            with self.assertRaises(ReceiptError):
                native.main(['--verify-tv-plan', '--family', 'desktop_macos'])
            api.assert_not_called()
            planned.assert_not_called()


class CurrentNativeCaseTests(unittest.TestCase):
    def fixture(self, retained_count=0):
        current = 'a' * 40
        required = ['Suite/testOne', 'Suite/testTwo']
        retained = {test: {'artifact': 7, 'test': test} for test in required[:retained_count]}
        pending = required[retained_count:]
        plan = {'required_tests': required, 'passes': retained, 'pending': pending}
        report = {'version': 1, 'scope': {'repository': 1, 'pr': 3}, 'current_commit': current,
                  'current_run': 10, 'adoptions': {}, 'compatibility': 'pinned', 'tv_cases': plan}
        receipt = {'schema': 2, 'source_sha': current, 'source_dirty': False,
                   'source_tree_sha256': 'd' * 64, 'finished_source': {'sha': current, 'dirty': False, 'tree_sha256': 'd' * 64},
                   'source_unchanged': True, 'xcode': native.XCODE, 'runtime': native.RUNTIME,
                   'status': 'passed', 'native_suite_complete': True, 'required_tests': required,
                   'selected_tests': pending, 'retained_passes': retained,
                   'case_results': {test: 'passed' for test in pending},
                   'suite_passes': {**retained, **{test: {'source_sha': current, 'own': True} for test in pending}}}
        log = ''
        if pending:
            receipt['command'] = ['xcodebuild', 'test', *['-only-testing:plurx-tvOSUITests/' + test for test in pending]]
            receipt['test_summary'] = {'passedTests': len(pending), 'failedTests': 0, 'skippedTests': 0}
            for test in pending:
                suite, case = test.split('/')
                log += f"Test Case '-[plurx_tvOSUITests.{suite} {case}]' started.\n"
                log += f"Test Case '-[plurx_tvOSUITests.{suite} {case}]' passed (0.1 seconds).\n"
        job = {'run_id': 10, 'repo_id': 1, 'attempt': 1, 'task_id': 20, 'status': 'success'}
        return report, receipt, log.encode(), job

    def archive(self, receipt, log=None, extra=()):
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, 'w') as archive:
            archive.writestr('receipt.json', json.dumps(receipt))
            if log is not None:
                archive.writestr('xcodebuild.log', log)
            for name, data in extra:
                archive.writestr(name, data)
        stream.seek(0)
        return stream

    def verify(self, report, receipt, log, job, extra=()):
        with patch.object(native, 'authenticate_tv_plan', return_value=report['tv_cases']), \
                patch.object(native, 'required_tests', return_value=receipt['required_tests']), \
                patch.object(native, 'full_source_digest', return_value='d' * 64):
            return native.current_tv_result(None, report['scope'], report['current_commit'], 10,
                                            report, self.archive(receipt, log, extra), job)

    def test_schema2_all_current_and_mixed_cases_keep_truthful_origins(self):
        for retained in (0, 1):
            with self.subTest(retained=retained):
                report, receipt, log, job = self.fixture(retained)
                result = self.verify(report, receipt, log, job, [('DerivedData/opaque', b'bounded payload')])
                self.assertEqual(result['disposition'], 'composed_cases' if retained else 'fresh_execution')
                self.assertEqual(result['original_cases'], report['tv_cases']['passes'])
                self.assertEqual(result['own_selected_cases'], report['tv_cases']['pending'])
                self.assertEqual(result['own_log_sha256'], hashlib.sha256(log).hexdigest())

    def test_all_retained_early_return_has_no_fabricated_execution_evidence(self):
        report, receipt, _, job = self.fixture(2)
        result = self.verify(report, receipt, None, job)
        self.assertEqual(result['disposition'], 'composed_cases')
        self.assertNotIn('own_selected_cases', result)
        self.assertNotIn('own_log_sha256', result)
        for field, value in [('command', []), ('test_summary', {}), ('case_results', {'Suite/testOne': 'passed'})]:
            with self.subTest(field=field), self.assertRaises(ReceiptError):
                self.verify(report, {**receipt, field: value}, None, job)
        for extra in ([('xcodebuild.log', b'')], [('screenshots/unused.png', b'opaque')]):
            with self.subTest(extra=extra), self.assertRaises(ReceiptError):
                self.verify(report, receipt, None, job, extra)

    def test_current_case_selection_source_and_complete_map_refuse_tampering(self):
        report, receipt, log, job = self.fixture(1)
        for field, value in [('schema', True), ('source_sha', 'b' * 40), ('source_dirty', True),
                             ('finished_source', {}), ('source_unchanged', False), ('status', 'failed'),
                             ('selected_tests', []), ('retained_passes', {}), ('suite_passes', {}),
                             ('native_suite_complete', False), ('case_results', {'Suite/testTwo': 'failed'})]:
            with self.subTest(field=field), self.assertRaises(ReceiptError):
                self.verify(report, {**receipt, field: value}, log, job)
        with self.assertRaises(ReceiptError):
            self.verify(report, receipt, log, {**job, 'status': 'failure'})
        with self.assertRaises(ReceiptError):
            self.verify(report, receipt, None, job)
        with self.assertRaises(ReceiptError):
            self.verify(report, receipt, log + b"Test Case '-[plurx_tvOSUITests.Suite extra]' passed (0.1 seconds).\n", job)
        with self.assertRaises(ReceiptError):
            self.verify(report, receipt, log, job, [('xcodebuild.log', log)])
        with self.assertRaises(ReceiptError):
            self.verify(report, receipt, log, job, [('./receipt.json', b'{}')])

    def test_plan_reauthenticates_exact_origins_and_refuses_changed_or_invalidated_credit(self):
        report, _, _, _ = self.fixture(1)
        artifact = {'id': 7}
        with patch.object(native, 'planned_artifacts', return_value=[artifact]) as origins, \
                patch.object(native, 'adopt_tv_cases', return_value=report['tv_cases']) as adopted:
            self.assertEqual(native.authenticate_tv_plan(None, report['scope'], report['current_commit'], 10, report), report['tv_cases'])
            origins.assert_called_once_with(None, report['tv_cases'])
            adopted.assert_called_once_with(None, report['scope'], report['current_commit'], 10, [artifact])
            adopted.return_value = {'required_tests': report['tv_cases']['required_tests'], 'passes': {},
                                    'pending': report['tv_cases']['required_tests']}
            with self.assertRaises(ReceiptError):
                native.authenticate_tv_plan(None, report['scope'], report['current_commit'], 10, report)
            adopted.side_effect = ReceiptError('Expired or changed original artifact')
            with self.assertRaises(ReceiptError):
                native.authenticate_tv_plan(None, report['scope'], report['current_commit'], 10, report)
        for changed in ({**report, 'version': True}, {**report, 'scope': {'pr': 4}}, {**report, 'unexpected': True}):
            with self.subTest(changed=changed), self.assertRaises(ReceiptError):
                native.authenticate_tv_plan(None, report['scope'], report['current_commit'], 10, changed)
