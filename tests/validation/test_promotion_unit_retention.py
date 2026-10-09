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
