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


class OriginalScopeEvidenceCase(unittest.TestCase):
    def retired_fixture(self):
        policy = dict(retention.RETIRED_DAEMON)
        old, successor = policy['id'], policy['successor']
        proof = fixture()
        proof.update(version=2, source_commit='b' * 40,
                     command_units={'version': 1, 'source_commit': 'b' * 40,
                                    'logs': {}, 'commands': []})
        suite = proof['lanes']['rust'][0]
        suite.update(name='daemon', ids=['a', successor], inventory=log('a\n' + successor + '\n'))
        first = suite['records'][0]
        text = 'test a ... ok\ntest ' + old + ' ... ok\n'
        first.update(source_commit=policy['source'], outcomes={'a': 'pass', old: 'pass'}, log=log(text))
        proof['command_units']['logs'][first['log']['sha256']] = first['log']
        policy['log'] = first['log']['sha256']
        second = copy.deepcopy(first)
        second.update(sequence=2, source_commit='b' * 40, outcomes={successor: 'pass'},
                      log=log('test ' + successor + ' ... ok\n'))
        suite['records'].append(second)
        return proof, policy

    def validate_retired(self, proof, policy, differences=lambda a, b: set(), source=lambda a, b: None,
                         second_policy=None):
        with patch.object(retention, 'RETIRED_DAEMON', policy), \
                patch.object(retention, 'RETIRED_DVH1_DAEMON',
                             retention.RETIRED_DVH1_DAEMON if second_policy is None else second_policy), \
                patch.object(retention, 'COHORTS', {'rust': {proof['lanes']['rust'][0]['name']}}), \
                patch('validation.qualification_unit_continuation.validate', return_value={}):
            return retention.validate(proof, 'b' * 40, differences=differences, source=source)

    def test_retired_original_pass_keeps_full_native_scope_and_later_successor_accounting(self):
        proof, policy = self.retired_fixture()
        original = copy.deepcopy(proof)
        adopted, _ = self.validate_retired(proof, policy)
        self.assertEqual(adopted['rust']['passed'], 2)
        retired = adopted['rust']['retired_original_passes']
        self.assertEqual([(row['test_id'], row['outcome'], row['successor']) for row in retired],
                         [(policy['id'], 'pass', policy['successor'])])
        self.assertEqual(proof, original)

    def test_retired_original_refuses_other_capture_context_and_nonpassing_or_earlier_successor(self):
        mutations = [
            lambda p: p['lanes']['rust'][0].update(name='core'),
            lambda p: p['lanes']['rust'][0]['records'][0].update(source_commit='a' * 40),
            lambda p: p['lanes']['rust'][0]['records'][0].update(log=log('test a ... ok\n')),
            lambda p: p['lanes']['rust'][0]['records'][0]['outcomes'].update(unreviewed='pass'),
            lambda p: p['lanes']['rust'][0]['records'].pop(),
            lambda p: p['lanes']['rust'][0]['records'][1].update(sequence=0),
        ]
        for mutate in mutations:
            proof, policy = self.retired_fixture()
            mutate(proof)
            with self.subTest(mutation=mutate), self.assertRaises(receipts.ReceiptError):
                self.validate_retired(proof, policy)
        for status, native in [('ignored', 'ignored'), ('fail', 'FAILED')]:
            proof, policy = self.retired_fixture()
            record = proof['lanes']['rust'][0]['records'][1]
            record.update(outcomes={policy['successor']: status},
                          log=log('test ' + policy['successor'] + ' ... ' + native + '\n'))
            with self.subTest(status=status), self.assertRaises(receipts.ReceiptError):
                self.validate_retired(proof, policy)
        proof, policy = self.retired_fixture()
        record = proof['lanes']['rust'][0]['records'][0]
        record['outcomes'][policy['id']] = 'fail'
        record['log'] = log('test a ... ok\ntest ' + policy['id'] + ' ... FAILED\n')
        policy['log'] = record['log']['sha256']
        with self.assertRaises(receipts.ReceiptError):
            self.validate_retired(proof, policy)

        proof, policy = self.retired_fixture()
        records = proof['lanes']['rust'][0]['records']
        duplicate = copy.deepcopy(records[0])
        duplicate['sequence'] = 2
        records[1]['sequence'] = 3
        records.append(duplicate)
        with self.assertRaisesRegex(receipts.ReceiptError, 'Duplicate retired original'):
            self.validate_retired(proof, policy)

        proof, policy = self.retired_fixture()
        records = proof['lanes']['rust'][0]['records']
        records[0]['sequence'] = 2
        records[1]['sequence'] = 1
        with self.assertRaisesRegex(receipts.ReceiptError, 'later applicable successor'):
            self.validate_retired(proof, policy)

    def test_retired_successor_refuses_source_invalidation_and_later_withheld_failure(self):
        for later_failure in (False, True):
            proof, policy = self.retired_fixture()
            suite = proof['lanes']['rust'][0]
            if later_failure:
                record = copy.deepcopy(suite['records'][1])
                record.update(sequence=3, outcomes={policy['successor']: 'fail'},
                              log=log('test ' + policy['successor'] + ' ... FAILED\n'))
                suite['records'].append(record)
            suite['records'][-1]['source_commit'] = 'a' * 40
            for record in suite['records']:
                if record['source_commit'] != 'b' * 40:
                    record['changes'] = {'crates/input.rs': {
                        'before': retention.digest(b'old'), 'after': retention.digest(b'new'),
                        'affected_ids': [policy['successor']] if record is suite['records'][-1] else []}}
            with self.subTest(later_failure=later_failure), self.assertRaises(receipts.ReceiptError):
                self.validate_retired(proof, policy,
                    differences=lambda a, b: {'crates/input.rs'} if a != b else set(),
                    source=lambda commit, path: b'new' if commit == 'b' * 40 else b'old')

    def test_two_finite_retirements_keep_mixed_logs_and_refuse_wrong_context_or_successor(self):
        def paired():
            proof, first_policy = self.retired_fixture()
            suite = proof['lanes']['rust'][0]
            second_policy = dict(retention.RETIRED_DVH1_DAEMON)
            old, successor = second_policy['id'], second_policy['successor']
            original = copy.deepcopy(suite['records'][0])
            original.update(sequence=3, source_commit=second_policy['source'],
                            outcomes={'b': 'pass', old: 'pass'},
                            log=log('test b ... ok\ntest ' + old + ' ... ok\n'))
            second_policy['log'] = original['log']['sha256']
            repaired = copy.deepcopy(suite['records'][1])
            repaired.update(sequence=4, outcomes={successor: 'pass'},
                            log=log('test ' + successor + ' ... ok\n'))
            suite['ids'] += ['b', successor]
            suite['inventory'] = log('\n'.join(suite['ids']) + '\n')
            suite['records'] += [original, repaired]
            return proof, first_policy, second_policy

        proof, first_policy, second_policy = paired()
        original = copy.deepcopy(proof)
        adopted, _ = self.validate_retired(proof, first_policy, second_policy=second_policy)
        self.assertEqual(adopted['rust']['passed'], 4)
        retired = adopted['rust']['retired_original_passes']
        self.assertEqual({(r['source_commit'], r['log_sha256'], r['test_id'], r['successor'])
                          for r in retired},
                         {(p['source'], p['log'], p['id'], p['successor'])
                          for p in (first_policy, second_policy)})
        self.assertEqual(proof, original, 'both original mixed logs stay intact')

        for fault in ('source', 'log', 'extra', 'omitted-sibling', 'retired-fail',
                      'retired-ignored', 'successor-fail', 'successor-ignored',
                      'successor-earlier', 'duplicate', 'invalidated-successor'):
            proof, first_policy, second_policy = paired()
            records = proof['lanes']['rust'][0]['records']
            original, repaired = records[2:]
            differences, source = lambda a, b: set(), lambda a, b: None
            if fault == 'source':
                original['source_commit'] = 'a' * 40
            elif fault == 'log':
                original['log'] = log('test b ... ok\n')
            elif fault == 'extra':
                original['outcomes']['unreviewed'] = 'pass'
            elif fault == 'omitted-sibling':
                del original['outcomes']['b']
            elif fault in ('retired-fail', 'retired-ignored'):
                status, native = ('fail', 'FAILED') if fault == 'retired-fail' else ('ignored', 'ignored')
                original['outcomes'][second_policy['id']] = status
                original['log'] = log('test b ... ok\ntest ' + second_policy['id'] + ' ... ' + native + '\n')
                second_policy['log'] = original['log']['sha256']
            elif fault in ('successor-fail', 'successor-ignored'):
                status, native = ('fail', 'FAILED') if fault == 'successor-fail' else ('ignored', 'ignored')
                repaired.update(outcomes={second_policy['successor']: status},
                                log=log('test ' + second_policy['successor'] + ' ... ' + native + '\n'))
            elif fault == 'successor-earlier':
                original['sequence'], repaired['sequence'] = 4, 3
            elif fault == 'duplicate':
                duplicate = copy.deepcopy(original)
                duplicate['sequence'] = 5
                records.append(duplicate)
            else:
                repaired['source_commit'] = 'a' * 40
                for record in records:
                    if record['source_commit'] != 'b' * 40:
                        record['changes'] = {'crates/input.rs': {
                            'before': retention.digest(b'old'), 'after': retention.digest(b'new'),
                            'affected_ids': [second_policy['successor']] if record is repaired else []}}
                differences = lambda a, b: {'crates/input.rs'} if a != b else set()
                source = lambda commit, path: b'new' if commit == 'b' * 40 else b'old'
            with self.subTest(fault=fault), self.assertRaises(receipts.ReceiptError):
                self.validate_retired(proof, first_policy, differences=differences, source=source,
                                      second_policy=second_policy)

    def test_structural_slots_bound_before_decode_and_duplicate_canonical_names_refuse(self):
        from validation import qualification_unit_continuation as commands
        proof = fixture()
        proof.update(version=2, source_commit='b' * 40, command_units={
            'version': 2, 'source_commit': 'b' * 40, 'logs': {}, 'commands': []})
        suite = proof['lanes']['rust'][0]
        item = log(json.dumps(suite))
        command = log(json.dumps({'name': 'validation', 'records': []}))
        proof['command_units']['logs'].update({item['sha256']: item, command['sha256']: command})
        proof['lanes']['rust'] = [{'sha256_ref': item['sha256']}]
        proof['command_units']['commands'] = [{'sha256_ref': command['sha256']}]
        with patch.object(commands, 'canonical', return_value={'validation': 'committed command'}):
            expanded = retention.expanded_document(proof)
            self.assertEqual(expanded['lanes']['rust'][0], suite)
            self.assertEqual(expanded['command_units']['commands'][0]['name'], 'validation')
            for field, count in [('suites', 9), ('commands', 513)]:
                changed = copy.deepcopy(proof)
                slots = changed['lanes']['rust'] if field == 'suites' else changed['command_units']['commands']
                slots *= count
                with self.subTest(field=field), self.assertRaises(receipts.ReceiptError), \
                        patch('validation.unit_evidence_objects.Objects.resolve') as decode:
                    try:
                        retention.expanded_document(changed)
                    finally:
                        decode.assert_not_called()
            for field in ('suites', 'commands'):
                changed = copy.deepcopy(proof)
                slots = changed['lanes']['rust'] if field == 'suites' else changed['command_units']['commands']
                slots *= 2
                with self.subTest(field=field), self.assertRaises(receipts.ReceiptError):
                    retention.expanded_document(changed)
            changed = copy.deepcopy(proof)
            changed['command_units']['version'] = 1
            with self.assertRaises(receipts.ReceiptError):
                retention.expanded_document(changed)

            dictionary={'format':'source-applicability-dictionary-v1','paths':['input.rs'],
                'hashes':[retention.digest(b'old'),retention.digest(b'new')],
                'affected_ids':[[]], 'reasons':[]}
            dictionary_blob=log(json.dumps(dictionary))
            manifest={'format':'source-applicability-columns-v1','source_commit':'a'*40,
                'candidate':'b'*40,'dictionary':{'sha256_ref':dictionary_blob['sha256']},
                'rows':'0,0,1,0\n'}
            manifest_blob=log(json.dumps(manifest))
            current=copy.deepcopy(suite)
            current['records'][0].update(source_commit='a'*40,changes={'sha256_ref':manifest_blob['sha256']})
            suite_blob=log(json.dumps(current))
            proof['command_units']['logs'].update({row['sha256']:row for row in
                [dictionary_blob,manifest_blob,suite_blob]})
            proof['lanes']['rust']=[{'sha256_ref':suite_blob['sha256']}]
            original=copy.deepcopy(proof)
            from validation.unit_evidence_objects import Objects,StructuralBudget
            budget=StructuralBudget()
            decoded=retention.expanded_document(proof,Objects(proof['command_units']['logs'],budget))
            measured=(budget.bytes,budget.nodes)
            self.assertEqual(proof,original)
            witness=decoded['lanes']['rust'][0]['records'][0]['changes']['input.rs']
            self.assertEqual(witness,{'before':retention.digest(b'old'),'after':retention.digest(b'new'),'affected_ids':[]})
            with patch.object(commands,'validate',return_value={}), \
                    patch.object(retention,'COHORTS',{'rust':{'core'}}):
                adopted,_=retention.validate(decoded,'b'*40,
                    differences=lambda a,b:{'input.rs'}if a!=b else set(),
                    source=lambda commit,path:b'new'if commit=='b'*40 else b'old')
                self.assertEqual(adopted['rust']['passed'],1)
                self.assertEqual((budget.bytes,budget.nodes),measured)
                witness['after']='f'*64
                with self.assertRaises(receipts.ReceiptError):
                    retention.validate(decoded,'b'*40,
                        differences=lambda a,b:{'input.rs'}if a!=b else set(),
                        source=lambda commit,path:b'new'if commit=='b'*40 else b'old')

    def test_generic_ci_authentication_uses_bounded_integer_ids_and_one_live_origin(self):
        proof = fixture()
        records = proof['lanes']['rust'][0]['records']
        records[0]['origin'].update(kind='authenticated-ci', identity='run:77/job:88')
        second = copy.deepcopy(records[0])
        second['sequence'] = 2
        records.append(second)
        binding = {'pull_request': 1, 'head_sha': 'd' * 40, 'base_sha': 'c' * 40,
                   'head_ref': proof['branch']}
        class API:
            def __init__(self, raw):
                self.calls = []
                self.digest = retention.digest(raw)
            def get(self, path):
                self.calls.append(path)
                if path == '': return {'id': 1}
                if path == '/actions/runs/77':
                    return {'repository': {'id': 1}, 'commit_sha': 'b' * 40}
                if path == '/pulls/1':
                    return {'number': 1, 'state': 'open', 'merged': False,
                        'head': {'repo': {'id': 1}, 'sha': 'd' * 40, 'ref': proof['branch']},
                        'base': {'repo': {'id': 1}, 'sha': 'c' * 40, 'ref': 'main'}}
                raise AssertionError(path)
            def pages(self, path):
                self.calls.append(path)
                if path == '/actions/runs/77/jobs':
                    return [{'id': 88, 'run_id': 77, 'repo_id': 1, 'status': 'failure', 'attempt': 1}]
                if path == '/issues/1/comments':
                    claim = {'repository': 1, 'pr': 1, 'sha256': self.digest,
                             'attachment_id': 1, 'attachment_uuid': '0' * 36}
                    return [{'id': 1, 'user': {'id': 2, 'login': 'writer'},
                             'body': 'Promotion-Unit-Evidence: ' + json.dumps(claim)}]
                raise AssertionError(path)
            def bytes(self, path):
                self.calls.append(path)
                self.assert_path = path
                return base64.b64decode(records[0]['log']['base64'])
        raw = json.dumps(proof).encode()
        api = API(raw)
        with patch.object(receipts, 'verify_attestor'), \
                patch.object(retention, 'validate', return_value={'rust': {'mode': 'retained-unit-evidence'}}):
            retention.verify(api, raw, retention.digest(raw), binding, 'd' * 40, 1, '0' * 36)
        for path in ['/actions/runs/77', '/actions/runs/77/jobs', '/actions/jobs/88/logs']:
            self.assertEqual(api.calls.count(path), 1)
        from validation import unit_evidence_objects as objects
        from validation import qualification_unit_continuation as commands
        shared = copy.deepcopy(proof)
        shared.update(version=2, command_units={})
        shared_raw = json.dumps(shared).encode()
        shared_api = API(shared_raw)
        def typed_auth(api, command_proof, document, repository, live_logs=None):
            self.assertIsNotNone(live_logs)
            self.assertEqual(live_logs.get(api, 77, 88), base64.b64decode(records[0]['log']['base64']))
            self.assertEqual(live_logs.bytes, len(base64.b64decode(records[0]['log']['base64'])))
        with patch.object(receipts, 'verify_attestor'), \
                patch.object(retention, 'expanded_document', side_effect=lambda value: value), \
                patch.object(commands, 'authenticate', side_effect=typed_auth), \
                patch.object(retention, 'validate', return_value=({'rust': {}}, {})):
            retention.verify(shared_api, shared_raw, retention.digest(shared_raw), binding, 'd' * 40, 1, '0' * 36)
        self.assertEqual(shared_api.calls.count('/actions/jobs/88/logs'), 1)
        with patch.object(objects, 'CI_LOG_BYTES', 1), patch.object(receipts, 'verify_attestor'), \
                patch.object(retention, 'validate'), self.assertRaisesRegex(receipts.ReceiptError, 'budget'):
            retention.verify(API(raw), raw, retention.digest(raw), binding, 'd' * 40, 1, '0' * 36)
        for identity in ['run:0/job:88', 'run:077/job:88', 'run:77/job:088',
                         'run:' + '1' * 20 + '/job:88', 'run:77/job:88/trailing']:
            changed = copy.deepcopy(proof)
            changed['lanes']['rust'][0]['records'][0]['origin']['identity'] = identity
            raw = json.dumps(changed).encode()
            api = API(raw)
            with self.subTest(identity=identity), patch.object(receipts, 'verify_attestor'), \
                    patch.object(retention, 'validate'), self.assertRaises(receipts.ReceiptError):
                retention.verify(api, raw, retention.digest(raw), binding, 'd' * 40, 1, '0' * 36)
            self.assertFalse(any(path.startswith('/actions/') for path in api.calls))


class NativeAppleEvidenceCase(unittest.TestCase):
    def test_native_namespaces_preserve_methods_and_refuse_collisions_and_other_lanes(self):
        template = "Test Case '-[{}.ExampleTests testA]' passed (0.1 seconds).\n"
        for module in ('ReceiptTests', 'plurx_iOSTests', 'plurx_tvOSTests'):
            started = "Test Case '-[{}.ExampleTests testA]' started.\n".format(module)
            self.assertEqual(retention.log_outcomes(started + template.format(module), 'xctest-class'),
                             {'ExampleTests/testA': 'pass'})
            self.assertEqual(retention.log_outcomes('asynchronous stderr prefix>' + template.format(module),
                                                   'xctest-class'), {'ExampleTests/testA': 'pass'})
        for invalid in (template.format('Unknown'), template.format('ReceiptTests') + template.format('plurx_iOSTests'),
                        template.format('ReceiptTests').replace('testA', 'notATest'),
                        template.format('ReceiptTests').replace('passed', 'passedX'),
                        'stderr prefix>' + template.format('ReceiptTests').replace('passed', 'unknown'),
                        'Test Case malformed>' + template.format('ReceiptTests'),
                        template.format('ReceiptTests') + template.format('ReceiptTests').replace('passed', 'unknown')):
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

        # The fixed historical instrumentation summary proves one whole command.
        # Source declarations and assumption skips are not measured named passes.
        policy = copy.deepcopy(retention.ANDROID_DEVICE_5172)
        raw = ('[command]/usr/bin/sh -c ' + policy['command'] + '\n'
               'tv.plurx.app.ExampleTest:.\nTime: 133.708\n\nOK (78 tests)\n'
               '🏁  Job succeeded\n')
        policy['log'] = log(raw)['sha256']
        sources = {
            '.github/workflows/ci.yml': b'env:\n  TEST: true\nconcurrency:\n  group: test\n'
                b'jobs:\n  android_device:\n    name: test\n',
            'Makefile': b'ANDROID_IMAGE ?= fixture\n# CI pre-pulls the image fixture\n'
                b'android-instrumentation-run:\n\tfixture\n.PHONY: android-instrumentation\n',
        }
        read = lambda commit, path: sources.get(path, b'reader')
        with patch.object(retention, 'android_device_tree', return_value='a' * 40):
            policy['inputs'] = retention.android_device_inputs(read, policy['source'])
        proof = fixture()
        suite = proof['lanes']['rust'][0]
        proof['lanes'] = {'android_device': [suite]}
        proof.update(version=2, source_commit='d' * 40, command_units={
            'version': 1, 'source_commit': 'd' * 40,
            'logs': {log(raw)['sha256']: log(raw)}, 'commands': []})
        suite.update(name='device', ids=[policy['id']], inventory=log(policy['id'] + '\n'),
                     environment=copy.deepcopy(policy['environment']))
        record = suite['records'][0]
        record.update(source_commit=policy['source'], environment=copy.deepcopy(policy['environment']),
            format='instrumentation-command-5172', outcomes={policy['id']: 'pass'}, log=log(raw),
            origin={'kind': 'authenticated-ci', 'identity': policy['identity'],
                    'command': policy['command']})
        def validate_command(value, reader=read):
            with patch.object(retention, 'ANDROID_DEVICE_5172', policy), \
                    patch.object(retention, 'android_device_tree', return_value='a' * 40), \
                    patch('validation.qualification_unit_continuation.validate', return_value={}):
                result = retention.validate(value, 'd' * 40, source=reader,
                                            differences=lambda before, after: set())
                return result[0] if value['version'] == 2 else result
        result = validate_command(proof)['android_device']
        self.assertEqual((result['passed'], result['commands_passed'], result['reported_tests']),
                         (1, 1, 78))
        self.assertEqual(result['accounting'], 'whole-command-success')
        for field in ('named_body_outcomes', 'passed_body_count', 'ignored_body_count'):
            self.assertIsNone(result[field])
        for mutate in (
            lambda s, r: s.update(name='other'),
            lambda s, r: r.update(source_commit='e' * 40),
            lambda s, r: r['origin'].update(kind='attested-local'),
            lambda s, r: r['origin'].update(identity='run:5172/job:51192'),
            lambda s, r: r['origin'].update(command=policy['command'] + ' -e class Example'),
            lambda s, r: r.update(log=log(raw + 'OK (78 tests)\n')),
            lambda s, r: r.update(log=log(raw.replace('78 tests', '77 tests'))),
            lambda s, r: r.update(log=log(raw.replace('[command]/usr/bin/sh -c ', ''))),
            lambda s, r: r.update(outcomes={'Example/test': 'pass'}),
            lambda s, r: s['records'].append(copy.deepcopy(r)),
            lambda s, r: s['environment']['source_scope'].update(test_declarations=77),
            lambda s, r: r['environment']['toolchain'].update(apk_sha256='a' * 64),
            lambda s, r: s.update(aggregate={'total': 78, 'failed': 0, 'baseline_sequence': 1}),
        ):
            changed = copy.deepcopy(proof)
            changed_suite = changed['lanes']['android_device'][0]
            mutate(changed_suite, changed_suite['records'][0])
            with self.subTest(mutation=mutate), self.assertRaises(receipts.ReceiptError):
                validate_command(changed)
        for path in policy['inputs']:
            if path.startswith('workflow-') or path.startswith('make-'):
                changed_path = ('.github/workflows/ci.yml' if path.startswith('workflow-')
                                else 'Makefile')
            else:
                changed_path = path
            with self.subTest(consumed_input=path), self.assertRaises(receipts.ReceiptError):
                if path in ('clients/android', '.github/actions/android-emulator-ready'):
                    with patch.object(retention, 'ANDROID_DEVICE_5172', policy), \
                            patch.object(retention, 'android_device_tree', return_value='e' * 40):
                        retention.android_device_record_contract('android_device', suite, record,
                                                                 'd' * 40, read)
                else:
                    validate_command(proof, lambda c, p: b'changed' if p == changed_path else read(c, p))
        additive_make = sources['Makefile'] + b'\nsubtitle-browser-check:\n\tfixture\n'
        def additive_read(commit, path):
            return additive_make if commit != policy['source'] and path == 'Makefile' else read(commit, path)
        with patch.object(retention, 'ANDROID_DEVICE_5172_CURRENT_MAKE',
                          {retention.digest(additive_make)}):
            self.assertEqual(validate_command(proof, additive_read)['android_device']['commands_passed'], 1)
        with self.assertRaises(receipts.ReceiptError):
            validate_command(proof, additive_read)
        old_version = copy.deepcopy(proof)
        old_version.update(version=1)
        del old_version['command_units']
        with self.assertRaises(receipts.ReceiptError):
            validate_command(old_version)
        for path, suffix in (
            ('Makefile', b'\noverride ANDROID_IMAGE := changed\n'),
            ('Makefile', b'\nandroid-instrumentation-run:\n\tother-runner\n'),
            ('.github/workflows/ci.yml', b'\nenv:\n  ANDROID_IMAGE: changed\n'),
            ('.github/workflows/ci.yml', b'\ndefaults:\n  run:\n    shell: other\n'),
            ('.github/workflows/ci.yml', b"\n'env':\n  ANDROID_IMAGE: changed\n"),
            ('.github/workflows/ci.yml', b'\ndefaults :\n  run:\n    shell: other\n'),
            ('.github/workflows/ci.yml', b"\n  other:\n    name: other\n  'android_device':\n    name: shadow\n"),
        ):
            with self.subTest(shadow=path, suffix=suffix), self.assertRaises(receipts.ReceiptError):
                validate_command(proof, lambda c, p: read(c, p) + suffix if p == path else read(c, p))
        job = {'name': 'Android instrumented UI tests', 'status': 'success', 'attempt': 1}
        retention.authenticate_android_device_command(5172, 51191, job)
        for run_id, job_id, altered in (
            (5173, 51191, job), (5172, 51192, job),
            (5172, 51191, {**job, 'status': 'failure'}),
            (5172, 51191, {**job, 'attempt': 2}),
            (5172, 51191, {**job, 'attempt': True}),
            (5172, 51191, {**job, 'name': 'other'}),
        ):
            with self.assertRaises(receipts.ReceiptError):
                retention.authenticate_android_device_command(run_id, job_id, altered)

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
