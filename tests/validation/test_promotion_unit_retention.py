"""Negative controls for candidate-bound retained unit qualification."""
import base64
import copy
import hashlib
import json
import unittest
from unittest.mock import patch
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts


def log(text):
    raw = text.encode()
    return {'base64': base64.b64encode(raw).decode(), 'sha256': hashlib.sha256(raw).hexdigest()}


def fixture():
    env = {'platform': 'linux-arm64', 'toolchain': 'rust1.97.1', 'features': ['hiqlite-store'], 'runtime': '2CPU'}
    return {'version': 1, 'repository': 1, 'pr': 1, 'branch': 'integration/test-into-main',
            'base_sha': 'c' * 40, 'source_commit': 'b' * 40, 'windows_waiver': None,
            'lanes': {'rust': [{'name': 'core', 'ids': ['a', 'b'], 'environment': env,
                'inventory': log('a\nb\n'), 'records': [{'sequence': 1, 'source_commit': 'b' * 40,
                    'environment': env, 'origin': {'kind': 'attested-local', 'identity': 'owned executor',
                    'command': 'cargo test --features hiqlite-store'}, 'changes': {},
                    'outcomes': {'a': 'pass', 'b': 'ignored'}, 'format': 'rust',
                    'log': log('test a ... ok\ntest b ... ignored\n')}]}]}}


def native_xctest_tree(status='Success', count='0'):
    def typed(kind, **fields):
        return {'_type': {'_name': kind}, **fields}
    def string(value):
        return typed('String', _value=value)
    def array(*values):
        return typed('Array', _values=list(values))
    target = 'plurx-iOSTests'
    base = 'test://com.apple.xcode/plurx/' + target + '/'
    leaf = typed('ActionTestMetadata', identifier=string('ExampleTests/testA()'),
        name=string('testA()'), identifierURL=string(base + 'ExampleTests/testA'),
        testStatus=string(status), failureSummariesCount=typed('Int', _value=count))
    def group(name, child):
        return typed('ActionTestSummaryGroup', name=string(name), identifier=string(name),
            identifierURL=string(base + name.replace(' ', '%20')), subtests=array(child),
            expectedFailures=array(), failureSummaries=array())
    testable = typed('ActionTestableSummary', name=string(target), targetName=string(target),
        identifierURL=string(base[:-1]), failureSummaries=array(),
        tests=array(group('Selected tests', group(target + '.xctest', group('ExampleTests', leaf)))))
    return typed('ActionTestPlanRunSummaries', summaries=array(
        typed('ActionTestPlanRunSummary', name=string('Test Scheme Action'), testableSummaries=array(testable))))


class NativeAppleEvidenceCase(unittest.TestCase):
    def test_native_namespaces_preserve_methods_and_refuse_collisions_and_other_lanes(self):
        template = "Test Case '-[{}.ExampleTests testA]' passed (0.1 seconds).\n"
        for module in ('ReceiptTests', 'plurx_iOSTests', 'plurx_tvOSTests'):
            self.assertEqual(retention.log_outcomes(template.format(module), 'xctest-class'),
                             {'ExampleTests/testA': 'pass'})
        for invalid in (template.format('Unknown'), template.format('ReceiptTests') + template.format('plurx_iOSTests'),
                        template.format('ReceiptTests').replace('testA', 'notATest')):
            with self.subTest(invalid=invalid), self.assertRaises(receipts.ReceiptError):
                retention.log_outcomes(invalid, 'xctest-class')
        proof = fixture()
        suite = proof['lanes']['rust'][0]
        suite['ids'] = ['ExampleTests/testA']
        suite['inventory'] = log('ExampleTests/testA\n')
        record = suite['records'][0]
        record.update(format='xctest-class', outcomes={'ExampleTests/testA': 'pass'},
                      log=log(template.format('ReceiptTests')))
        with patch.object(retention, 'COHORTS', {'rust': {'core'}}), self.assertRaisesRegex(receipts.ReceiptError, 'outside Apple'):
            retention.validate(proof, 'd' * 40, differences=lambda a, b: set())

    def test_whole_native_result_preserves_pass_fail_and_skip_without_text_terminals(self):
        for status, count, outcome in [('Success', '0', 'pass'), ('Failure', '1', 'fail'), ('Skipped', '0', 'ignored')]:
            tree = native_xctest_tree(status, count)
            self.assertEqual(retention.log_outcomes(json.dumps(tree), 'xctest-result'), {'ExampleTests/testA': outcome})

    def test_native_result_refuses_target_uri_name_metadata_status_and_count_disagreement(self):
        def parts(tree):
            testable = tree['summaries']['_values'][0]['testableSummaries']['_values'][0]
            group = testable['tests']['_values'][0]['subtests']['_values'][0]['subtests']['_values'][0]
            return testable, group, group['subtests']['_values'][0]
        mutations = [
            lambda t, g, v: t['targetName'].update(_value='foreign'),
            lambda t, g, v: t['identifierURL'].update(_value='test://foreign/plurx/plurx-iOSTests'),
            lambda t, g, v: g['name'].update(_value='OtherTests'),
            lambda t, g, v: g['identifierURL'].update(_value=g['identifierURL']['_value'] + '?query'),
            lambda t, g, v: v['name'].update(_value='testB()'),
            lambda t, g, v: v['identifierURL'].update(_value=v['identifierURL']['_value'] + '#fragment'),
            lambda t, g, v: v['identifierURL'].update(_value=v['identifierURL']['_value'].replace('/testA', '%2FtestA')),
            lambda t, g, v: v['testStatus'].update(_value='ExpectedFailure'),
            lambda t, g, v: v['failureSummariesCount'].update(_value='1'),
            lambda t, g, v: v['failureSummariesCount'].update(_value='-1'),
            lambda t, g, v: g['expectedFailures']['_values'].append({'_type': {'_name': 'String'}, '_value': 'expected'}),
            lambda t, g, v: g['subtests']['_values'].append(copy.deepcopy(v)),
            lambda t, g, v: v.update(unrelated_metadata=copy.deepcopy(v)),
            lambda t, g, v: v['_type'].update(_name='UnexpectedMetadata'),
        ]
        for mutation in mutations:
            tree = native_xctest_tree()
            mutation(*parts(tree))
            with self.subTest(mutation=mutation), self.assertRaises(receipts.ReceiptError):
                retention.log_outcomes(json.dumps(tree), 'xctest-result')
        raw = json.dumps(native_xctest_tree()).replace('"_value": "Success"', '"_value": "Success", "_value": "Failure"')
        with self.assertRaises(receipts.ReceiptError):
            retention.log_outcomes(raw, 'xctest-result')

    def test_native_result_traversal_and_unowned_nested_metadata_are_bounded(self):
        tree = native_xctest_tree()
        tree['unrelated'] = [0] * 65536
        with self.assertRaisesRegex(receipts.ReceiptError, 'traversal limit'):
            retention.log_outcomes(json.dumps(tree), 'xctest-result')
        tree = native_xctest_tree()
        value = tree
        for _ in range(65):
            value['unrelated'] = {}
            value = value['unrelated']
        with self.assertRaisesRegex(receipts.ReceiptError, 'traversal limit'):
            retention.log_outcomes(json.dumps(tree), 'xctest-result')

    def test_shared_blobs_require_version_two_exact_digest_and_existing_embedded_bytes(self):
        original = log('original native log')
        digest = original['sha256']
        document = {'version': 2, 'command_units': {'logs': {digest: original}}}
        reference = {'sha256_ref': digest}
        self.assertEqual(retention.shared_blob(document, reference), original)
        for mutation in (lambda d: d.update(version=1), lambda d: d['command_units']['logs'].clear(),
                         lambda d: d['command_units']['logs'][digest].update(sha256='0' * 64),
                         lambda d: d['command_units']['logs'][digest].update(base64=base64.b64encode(b'changed').decode())):
            changed = copy.deepcopy(document)
            mutation(changed)
            with self.subTest(mutation=mutation), self.assertRaises(receipts.ReceiptError):
                retention.shared_blob(changed, reference)

    def test_shared_manifests_keep_source_candidate_path_hash_and_affected_id_checks(self):
        def evidence(mutation=lambda value: None):
            proof = fixture()
            proof.update(version=2, source_commit='b' * 40,
                         command_units={'version': 1, 'source_commit': 'b' * 40, 'logs': {}, 'commands': []})
            record = proof['lanes']['rust'][0]['records'][0]
            record['source_commit'] = 'a' * 40
            manifest = {'source_commit': 'a' * 40, 'candidate': 'b' * 40,
                'changes': {'crates/input.rs': {'before': retention.digest(b'old'),
                    'after': retention.digest(b'new'), 'affected_ids': []}}}
            mutation(manifest)
            item = log(json.dumps(manifest))
            original = record['log']
            proof['command_units']['logs'].update({item['sha256']: item, original['sha256']: original})
            record.update(changes={'sha256_ref': item['sha256']}, log={'sha256_ref': original['sha256']})
            return proof
        def validate(proof):
            with patch.object(retention, 'COHORTS', {'rust': {'core'}}), \
                 patch('validation.qualification_unit_continuation.validate', return_value={}):
                return retention.validate(proof, 'b' * 40,
                    source=lambda commit, path: b'old' if commit == 'a' * 40 else b'new',
                    differences=lambda before, after: {'crates/input.rs'} if before != after else set())
        self.assertEqual(validate(evidence())[0]['rust']['passed'], 1)
        for mutation in (lambda m: m.update(source_commit='c' * 40),
                         lambda m: m.update(candidate='c' * 40),
                         lambda m: m['changes'].clear(),
                         lambda m: m['changes']['crates/input.rs'].update(before='0' * 64),
                         lambda m: m['changes']['crates/input.rs'].update(after='0' * 64),
                         lambda m: m['changes']['crates/input.rs'].update(affected_ids=['unknown']),
                         lambda m: m['changes']['crates/input.rs'].update(affected_ids=['a'])):
            with self.subTest(mutation=mutation), self.assertRaises(receipts.ReceiptError):
                validate(evidence(mutation))


class RetainedUnitCase(unittest.TestCase):
    def validate(self, proof):
        with patch.object(retention, 'COHORTS', {lane: {suite['name'] for suite in suites}
                    for lane, suites in proof['lanes'].items()}):
            return retention.validate(proof, 'd' * 40, source=lambda c, p: b'source',
                                      differences=lambda a, b: set())

    def test_literal_outcomes_are_retained_without_execution(self):
        proof = fixture()
        before = copy.deepcopy(proof)
        result = self.validate(proof)
        self.assertEqual(result['rust']['passed'], 1)
        self.assertEqual(result['rust']['accounting'], 'literal-per-ID')
        self.assertEqual(proof, before)

    def test_tampered_missing_and_failed_evidence_refuses(self):
        mutations = [
            lambda p: p.update(version=2),
            lambda p: p['lanes']['rust'][0]['ids'].append('unexecuted'),
            lambda p: p['lanes']['rust'][0]['records'][0]['outcomes'].update(a='fail'),
            lambda p: p['lanes']['rust'][0]['records'][0]['log'].update(sha256='0' * 64),
            lambda p: p['lanes']['rust'][0]['records'][0]['environment'].update(runtime='unknown'),
            lambda p: p['lanes']['rust'][0]['records'][0].update(changes={'missing': {}}),
            lambda p: p.update(windows_waiver={'authority': 'not-human'}),
        ]
        for mutation in mutations:
            proof = copy.deepcopy(fixture())
            # Separate source environment from the expected witness.
            proof['lanes']['rust'][0]['records'][0]['environment'] = copy.deepcopy(proof['lanes']['rust'][0]['environment'])
            mutation(proof)
            with self.subTest(mutation=mutation), self.assertRaises(receipts.ReceiptError):
                self.validate(proof)

    def test_unknown_source_delta_and_wrong_digest_refuse(self):
        with self.assertRaises(receipts.ReceiptError):
            retention.validate(fixture(), 'd' * 40, differences=lambda a, b: {'crates/new.rs'})
        with self.assertRaises(receipts.ReceiptError):
            retention.verify(None, b'{}', '0' * 64, {}, 'd' * 40, 1, '0' * 36)

    def test_aggregate_android_keeps_counts_without_inventing_positive_ids(self):
        proof = fixture()
        suite = proof['lanes']['rust'][0]
        proof['lanes'] = {'android_jvm': [suite]}
        suite['ids'] = ['ExampleTest/broken']
        suite['inventory'] = log('ExampleTest/broken\n')
        suite['aggregate'] = {'total': 1076, 'failed': 1, 'baseline_sequence': 1}
        record = suite['records'][0]
        record['format'] = 'gradle-summary'
        record['outcomes'] = {'ExampleTest/broken': 'fail'}
        record['log'] = log('ExampleTest > broken FAILED\n1076 tests completed, 1 failed\n')
        repair = copy.deepcopy(record)
        repair.update(sequence=2, format='junit', outcomes={'ExampleTest/broken': 'pass'},
            log=log('<testsuite><testcase classname="package.ExampleTest" name="broken"/></testsuite>'))
        suite['records'].append(repair)
        result = self.validate(proof)['android_jvm']
        self.assertEqual(result['passed'], 1076)
        self.assertEqual(result['accounting'], 'aggregate-cohort-derived')
        self.assertEqual(suite['ids'], ['ExampleTest/broken'])
        suite['aggregate']['failed'] = 2
        with self.assertRaises(receipts.ReceiptError):
            self.validate(proof)

    def test_failed_unit_cannot_be_reclassified_as_ignored(self):
        proof = fixture()
        suite = proof['lanes']['rust'][0]
        original = suite['records'][0]
        original['outcomes']['a'] = 'fail'
        original['log'] = log('test a ... FAILED\ntest b ... ignored\n')
        later = copy.deepcopy(original)
        later.update(sequence=2, outcomes={'a': 'ignored'}, log=log('test a ... ignored\n'))
        suite['records'].append(later)
        with self.assertRaisesRegex(receipts.ReceiptError, 'ignoring'):
            self.validate(proof)

    def test_aggregate_without_baseline_and_incomplete_lane_refuse(self):
        proof = fixture()
        with self.assertRaisesRegex(receipts.ReceiptError, 'canonical'):
            retention.validate(proof, 'd' * 40, differences=lambda a, b: set())
        suite = proof['lanes']['rust'][0]
        suite['aggregate'] = {'total': 1076, 'failed': 2, 'baseline_sequence': 99}
        with self.assertRaisesRegex(receipts.ReceiptError, 'baseline'):
            self.validate(proof)

    def test_only_closed_compatible_environments_share_the_unit_contract(self):
        proof = fixture()
        suite = proof['lanes']['rust'][0]
        one = {**suite['environment'], 'host': 'owned-a'}
        two = {**suite['environment'], 'host': 'owned-b'}
        suite['compatible_environments'] = [one, two]
        suite['records'][0]['environment'] = two
        self.assertEqual(self.validate(proof)['rust']['passed'], 1)
        suite['records'][0]['environment'] = {**two, 'host': 'unknown'}
        with self.assertRaises(receipts.ReceiptError): self.validate(proof)
        suite['records'][0]['environment'] = two
        suite['compatible_environments'][1]['toolchain'] = 'unreviewed'
        with self.assertRaises(receipts.ReceiptError): self.validate(proof)

    def test_real_xctest_and_instrumentation_markers_are_decoded(self):
        self.assertEqual(retention.log_outcomes("Test Case '-[Module.Tests testA]' passed (0.1 seconds).", 'xctest'),
                         {'Module.Tests/testA': 'pass'})
        self.assertEqual(retention.log_outcomes('INSTRUMENTATION_STATUS: class=package.Tests\n'
             'INSTRUMENTATION_STATUS: test=testA\nINSTRUMENTATION_STATUS_CODE: 0\n', 'instrumentation'),
             {'Tests/testA': 'pass'})

    def test_serial_rust_traces_require_one_unambiguous_outcome(self):
        text = ('running 1 test\ntest exact::id ... trace\nmore trace\nok\n'
                '\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n')
        self.assertEqual(retention.log_outcomes(text, 'rust-serial'), {'exact::id': 'pass'})
        self.assertEqual(retention.log_outcomes(text.replace('\nok\n', '\nFAILED\n').replace('test result: ok. 1 passed; 0 failed;', 'test result: FAILED. 0 passed; 1 failed;'), 'rust-serial'),
                         {'exact::id': 'fail'})
        with self.assertRaises(receipts.ReceiptError):
            retention.log_outcomes(text.replace('more trace', 'ok'), 'rust-serial')
        for invalid in (text.replace('test result: ok. 1 passed; 0 failed;',
                                     'test result: FAILED. 0 passed; 1 failed;'),
                        text.split('test result:', 1)[0],
                        text + 'test unfinished ... trace\n'):
            with self.assertRaises(receipts.ReceiptError):
                retention.log_outcomes(invalid, 'rust-serial')
        self.assertEqual(retention.log_outcomes('test file.rs - item (line 12) ... ok\n', 'rust'),
                         {'file.rs - item (line 12)': 'pass'})

    def test_rust_should_panic_annotation_keeps_the_discovered_identity(self):
        self.assertEqual(retention.log_outcomes('test module::case - should panic ... ok\n', 'rust'),
                         {'module::case': 'pass'})
        with self.assertRaises(receipts.ReceiptError):
            retention.log_outcomes('test module::case - should panic ... ok\n'
                                   'test module::case ... ok\n', 'rust')

    def test_live_identity_and_writer_attestation_are_required(self):
        proof = fixture()
        raw = json.dumps(proof).encode()
        binding = {'pull_request': 1, 'head_sha': 'd' * 40, 'base_sha': 'c' * 40,
                   'head_ref': proof['branch']}
        class API:
            def get(self, path):
                if path == '': return {'id': 1}
                return {'number': 1, 'state': 'open', 'merged': False,
                    'head': {'repo': {'id': 1}, 'sha': 'd' * 40, 'ref': proof['branch']},
                    'base': {'repo': {'id': 1}, 'sha': 'c' * 40, 'ref': 'main'}}
            def pages(self, path): return []
        with self.assertRaisesRegex(receipts.ReceiptError, 'authenticated writer'):
            retention.verify(API(), raw, retention.digest(raw), binding, 'd' * 40, 1, '0' * 36)

class PrivateAttachmentCase(unittest.TestCase):
    def test_metadata_origin_uuid_and_size_are_exact(self):
        raw = b'{"private": "evidence"}'
        identifier = '01234567-89ab-cdef-0123-456789abcdef'
        digest = retention.digest(raw)
        class Response:
            def __enter__(self): return self
            def __exit__(self, *args): pass
            def read(self, limit): return raw
        class Opener:
            def open(self, request, timeout):
                self.request = request
                return Response()
        class API:
            root = 'https://forge.lan/api/v1/repos/example/plurx'
            token = 'synthetic-token'
            opener = Opener()
            item = {'id': 123, 'uuid': identifier, 'type': 'attachment',
                    'name': digest + '.json', 'size': len(raw),
                    'browser_download_url': 'https://forge.lan/attachments/' + identifier}
            def get(self, path):
                assert path == '/issues/1/assets/123'
                return self.item
        api = API()
        with patch.dict('os.environ', GITHUB_SERVER_URL='https://forge.lan'), \
                patch.object(retention, 'comment_evidence_bytes', return_value=raw):
            self.assertEqual(retention.attachment_bytes(api, 1, 123, identifier, digest), raw)
            for key, value in [('id', 124), ('uuid', 'wrong'), ('type', 'external'),
                ('name', 'other.json'), ('size', 1),
                ('browser_download_url', 'https://elsewhere.invalid/attachments/' + identifier),
                ('browser_download_url', 'https://forge.lan/attachments/' + identifier + '?token=bad')]:
                before = copy.deepcopy(api.item)
                api.item[key] = value
                with self.subTest(key=key), self.assertRaises(receipts.ReceiptError):
                    retention.attachment_bytes(api, 1, 123, identifier, digest)
                api.item = before

    def chunks(self):
        import gzip
        raw = b'{"private": "source-bound evidence"}'
        identifier = '01234567-89ab-cdef-0123-456789abcdef'
        digest = retention.digest(raw)
        packed = base64.b64encode(gzip.compress(raw, mtime=0)).decode()
        split = len(packed) // 2
        rows = []
        for index, part in enumerate((packed[:split], packed[split:])):
            claim = {'version': 1, 'repository': 1, 'pr': 1,
                'attachment_id': 123, 'attachment_uuid': identifier,
                'sha256': digest, 'index': index, 'count': 2, 'gzip_base64': part}
            rows.append({'id': index + 1, 'user': {'id': 7, 'login': 'writer'},
                'body': 'Promotion-Unit-Evidence-Chunk: ' + json.dumps(claim)})
        class API:
            def get(self, path):
                assert path == ''
                return {'id': 1}
            def pages(self, path):
                assert path == '/issues/1/comments'
                return self.comments
        api = API()
        api.comments = rows
        return api, raw, identifier, digest

    def test_private_comment_chunks_preserve_exact_bytes_and_writer(self):
        api, raw, identifier, digest = self.chunks()
        api.comments.reverse()
        with patch.object(receipts, 'verify_attestor') as authenticate:
            self.assertEqual(retention.comment_evidence_bytes(api, 1, 123, identifier, digest), raw)
            authenticate.assert_called_once_with(api, {'repository': 1}, {'id': 7, 'login': 'writer'})

    def test_missing_duplicate_wrong_bound_or_tampered_chunks_refuse(self):
        def alter(rows, key, value):
            claim = json.loads(rows[0]['body'].split(': ', 1)[1])
            claim[key] = value
            rows[0]['body'] = 'Promotion-Unit-Evidence-Chunk: ' + json.dumps(claim)
        mutations = [lambda rows: rows.pop(), lambda rows: rows.append(copy.deepcopy(rows[0])),
            lambda rows: alter(rows, 'count', 3), lambda rows: alter(rows, 'count', 129),
            lambda rows: alter(rows, 'index', True), lambda rows: alter(rows, 'attachment_id', 124),
            lambda rows: alter(rows, 'attachment_uuid', 'wrong'),
            lambda rows: alter(rows, 'sha256', '0' * 64),
            lambda rows: alter(rows, 'gzip_base64', '!not-base64!'),
            lambda rows: alter(rows, 'unknown', 1),
            lambda rows: alter(rows, 'gzip_base64', 'A' * 60001)]
        for mutate in mutations:
            api, raw, identifier, digest = self.chunks()
            mutate(api.comments)
            with self.subTest(mutation=mutate), patch.object(receipts, 'verify_attestor'), \
                    self.assertRaises((receipts.ReceiptError, ValueError)):
                retention.comment_evidence_bytes(api, 1, 123, identifier, digest)

    def test_each_comment_writer_is_authenticated_without_content_download(self):
        api, raw, identifier, digest = self.chunks()
        api.comments[1]['user'] = {'id': 8, 'login': 'unknown'}
        def authenticate(api, claim, user):
            receipts.require(user['id'] == 7, 'Untrusted evidence chunk writer')
        with patch.object(receipts, 'verify_attestor', side_effect=authenticate), \
                self.assertRaisesRegex(receipts.ReceiptError, 'Untrusted'):
            retention.comment_evidence_bytes(api, 1, 123, identifier, digest)

    def test_compressed_logs_keep_exact_original_digest_and_bounds(self):
        import gzip
        raw = b'test exact ... ok\n'
        packed = {'gzip_base64': base64.b64encode(gzip.compress(raw)).decode(),
                  'sha256': retention.digest(raw)}
        self.assertEqual(retention.decoded_log(packed), raw.decode())
        packed['gzip_base64'] = base64.b64encode(gzip.compress(raw) + b'trailing').decode()
        with self.assertRaises(receipts.ReceiptError): retention.decoded_log(packed)


class RetainedQualificationCase(unittest.TestCase):
    def parameters(self):
        from validation.qualification import REQUIRED_JOBS
        binding = {'schema': 1, 'repository': 'example/plurx', 'pull_request': 1,
                   'head_ref': 'integration/test-into-main', 'base_ref': 'main',
                   'head_sha': 'a' * 40, 'base_sha': 'b' * 40}
        environment = {'GITHUB_EVENT_NAME': 'workflow_dispatch', 'GITHUB_REPOSITORY': 'example/plurx',
            'GITHUB_SHA': 'a' * 40, 'GITHUB_WORKFLOW_REF': 'example/plurx/ci.yml',
            'GITHUB_RUN_ID': '1', 'GITHUB_RUN_ATTEMPT': '1',
            'PLURX_PROMOTION_BINDING': json.dumps(binding), 'PLURX_PROMOTION_PR': '1',
            'PLURX_PROMOTION_HEAD_SHA': 'a' * 40, 'PLURX_PROMOTION_BASE_SHA': 'b' * 40}
        retained = {'version': 1, 'candidate_sha': 'a' * 40, 'base_sha': 'b' * 40,
            'pull_request': 1, 'sha256': 'c' * 64, 'attestation_comments': [123],
            'lanes': {'rust': {'mode': 'retained-unit-evidence'},
                      'windows_compile': {'mode': 'explicit-human-waiver'}}}
        results = {job: 'success' for job in REQUIRED_JOBS}
        results.update(rust='skipped', windows_compile='skipped')
        return environment, retained, results

    def test_receipt_preserves_skipped_and_distinct_waiver(self):
        from validation.qualification import build_receipt
        env, proof, results = self.parameters()
        env['PLURX_RETAINED_UNITS'] = json.dumps(proof)
        receipt = build_receipt(env, results, 'a' * 40, 'd' * 40)
        self.assertEqual(receipt['jobs']['rust'], 'skipped')
        self.assertEqual(receipt['jobs']['windows_compile'], 'skipped')
        self.assertEqual(receipt['retained_unit_evidence'], proof)

    def test_unknown_skipped_failed_and_moved_evidence_refuse(self):
        from validation.qualification import build_receipt, QualificationError
        for mutation in ('missing', 'failed', 'moved', 'uncovered', 'fake_waiver'):
            env, proof, results = self.parameters()
            if mutation == 'failed': results['rust'] = 'failure'
            if mutation == 'moved': proof['candidate_sha'] = 'e' * 40
            if mutation == 'uncovered': results['apple'] = 'skipped'
            if mutation == 'fake_waiver': proof['lanes']['windows_compile']['mode'] = 'retained-unit-evidence'
            if mutation != 'missing': env['PLURX_RETAINED_UNITS'] = json.dumps(proof)
            with self.subTest(mutation=mutation), self.assertRaises(QualificationError):
                build_receipt(env, results, 'a' * 40, 'd' * 40)
