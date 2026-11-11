"""Source-bound refusal controls for incremental local binary continuation."""
import base64
import copy
import hashlib
import json
import shlex
import unittest
from unittest.mock import patch

from validation import qualification_unit_continuation as q
from validation import python_unit_receipts as receipts
from validation.incremental_binary_control import INCREMENTAL_TEST_PROFILE_0

SOURCE = 'b' * 40
FEATURES = ['cluster-read-cost-validation', 'hiqlite-contract-tests', 'hiqlite-store']
PROFILE = {'opt_level': '0', 'debuginfo': 0, 'debug_assertions': True,
           'overflow_checks': True, 'test': True}
COMMAND = ('RUST_MIN_STACK=8388608 $(CARGO) test --locked -p plurx-core '
           '--features cluster-read-cost-validation,hiqlite-contract-tests '
           '--test store_contract -- --test-threads=1')
BUILD = ['cargo', 'test', '--locked', '-p', 'plurx-core', '--features',
         'cluster-read-cost-validation,hiqlite-contract-tests', '--test',
         'store_contract', '--no-run', '--message-format=json']
SOURCE_BINARY = '/owned/debug/deps/store_contract-deadbeef'
CONTROL_BINARY = '/control/debug/deps/store_contract-deadbeef'
SOURCE_FP = '/owned/debug/.fingerprint/plurx-core-deadbeef/test-integration-test-store_contract.json'
CONTROL_FP = '/control/debug/.fingerprint/plurx-core-deadbeef/test-integration-test-store_contract.json'


def encoded(value):
    raw = value.encode() if isinstance(value, str) else value
    return {'base64': base64.b64encode(raw).decode(), 'sha256': hashlib.sha256(raw).hexdigest()}


def fixture():
    environment = {'platform': 'linux-x64', 'toolchain': 'rust1.97.1', 'features': FEATURES,
                   'runtime': {'test_threads': 1, 'rust_min_stack': 8388608,
                               'target': 'x86_64-unknown-linux-gnu',
                               'rustc_version': q.RUSTC_VERSION, 'profile': PROFILE}}
    source_artifact = {'reason': 'compiler-artifact',
                       'package_id': 'path+file:///owned/src/crates/plurx-core#0.8.0',
                       'manifest_path': '/owned/src/crates/plurx-core/Cargo.toml',
                       'target': {'name': 'store_contract', 'kind': ['test'],
                                  'src_path': '/owned/src/crates/plurx-core/tests/store_contract.rs'},
                       'executable': SOURCE_BINARY, 'features': FEATURES, 'profile': PROFILE}
    control_artifact = {**source_artifact, 'executable': CONTROL_BINARY}
    fingerprint = {'rustc': q.RUSTC_FINGERPRINT, 'compile_kind': 0, 'rustflags': [],
                   'features': json.dumps(FEATURES),
                   'profile': q.TEST_PROFILE_FINGERPRINTS[0]}
    fingerprint_raw = json.dumps(fingerprint, separators=(',', ':'))
    build_stdout = (json.dumps(control_artifact) + '\n'
                    + json.dumps({'reason': 'build-finished', 'success': True}) + '\n')
    source_list = 'a: test\n'
    control = {
        'version': 1, 'source_commit': SOURCE, 'archive_sha256': 'd' * 64,
        'source_manifest_sha256': 'e' * 64,
        'compiler': q.RUSTC_VERSION + '\nhost: x86_64-unknown-linux-gnu\n',
        'cwd': '/owned/src',
        'environment': {'CARGO_INCREMENTAL': '0', 'CARGO_BUILD_JOBS': '1',
                        'CARGO_TARGET_DIR': '/control', 'RUSTFLAGS': None,
                        'CARGO_ENCODED_RUSTFLAGS': None, 'CARGO_PROFILE_TEST_DEBUG': None,
                        'RUSTC_WRAPPER': None, 'RUSTC_WORKSPACE_WRAPPER': None},
        'build_argv': BUILD, 'build_exit': 0, 'build_stdout': build_stdout,
        'build_stdout_sha256': hashlib.sha256(build_stdout.encode()).hexdigest(),
        'build_stdout_bytes': len(build_stdout.encode()),
        'build_stderr_sha256': 'f' * 64,
        'artifact': control_artifact, 'binary_path': CONTROL_BINARY,
        'binary_sha256_before': '1' * 64, 'binary_sha256_after': '1' * 64,
        'fingerprint_path': CONTROL_FP,
        'fingerprint_sha256': hashlib.sha256(fingerprint_raw.encode()).hexdigest(),
        'fingerprint_raw': fingerprint_raw, 'fingerprint': fingerprint,
        'list_argv': [CONTROL_BINARY, '--list', '--format=terse'], 'list_exit': 0,
        'list_stdout': source_list,
        'list_stdout_sha256': hashlib.sha256(source_list.encode()).hexdigest(),
        'source_inventory_sha256': hashlib.sha256(source_list.encode()).hexdigest(),
    }
    logs = {}
    for raw in (source_list, json.dumps(control)):
        value = encoded(raw)
        logs[value['sha256']] = value
    argv = [SOURCE_BINARY, 'a', '--exact', '--test-threads=1']
    raw = ('COMMAND ' + json.dumps(argv) + '\nrunning 1 test\n'
           'test a ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n'
           'NATIVE_EXIT 0\n')
    witness = {'mode': 'local-binary', 'first_line': 1,
               'build_invocation': shlex.join(BUILD), 'archive_sha256': 'd' * 64,
               'binary_sha256_before': 'c' * 64, 'binary_sha256_after': 'c' * 64,
               'features': FEATURES,
               'fingerprint': {**fingerprint, 'profile': INCREMENTAL_TEST_PROFILE_0},
               'binary_path': SOURCE_BINARY, 'fingerprint_path': SOURCE_FP,
               'source_artifact': source_artifact,
               'source_list_log': {'sha256_ref': hashlib.sha256(source_list.encode()).hexdigest()},
               'incremental_control': {'sha256_ref': hashlib.sha256(json.dumps(control).encode()).hexdigest()}}
    record = {'sequence': 1, 'source_commit': SOURCE, 'environment': environment,
              'origin': {'kind': 'attested-local', 'identity': 'actual owned executor',
                         'command': shlex.join(argv), 'execution': witness},
              'changes': {}, 'log': {}, 'coverage': {'mode': 'rust', 'first_line': 1,
              'last_line': len(raw.splitlines()), 'format': 'rust-serial',
              'outcomes': {'a': 'pass'}}, 'exit_code': 0}
    item = {'name': 'store_contract:1', 'invocation': COMMAND,
            'inventory': {'source_commit': SOURCE, 'ids': ['a'],
                          'list_log': encoded(source_list),
                          'binary_sha256': 'c' * 64, 'features': FEATURES}}
    return record, item, raw, {'logs': logs}, control


class IncrementalBinaryControlCase(unittest.TestCase):
    def bound(self, record, item, raw, proof):
        with patch.object(q, 'source_archive_digest', return_value='d' * 64):
            return q.bound_execution(record, item, raw, proof=proof)

    def test_measured_same_source_control_admits_incremental_test_binary(self):
        record, item, raw, proof, _ = fixture()
        self.assertIn('test a ... ok', self.bound(record, item, raw, proof))

    def test_unknown_or_modified_control_is_refused(self):
        for path, value in (
            (('source_commit',), '9' * 40),
            (('archive_sha256',), '9' * 64),
            (('environment', 'CARGO_INCREMENTAL'), '1'),
            (('environment', 'RUSTFLAGS'), '-C opt-level=3'),
            (('artifact', 'features'), []),
            (('artifact', 'package_id'), 'path+file:///other#0.8.0'),
            (('artifact', 'target', 'name'), 'other'),
            (('fingerprint', 'profile'), INCREMENTAL_TEST_PROFILE_0),
            (('fingerprint', 'rustflags'), ['--cfg', 'hidden']),
            (('binary_sha256_after',), '2' * 64),
            (('list_stdout',), 'other: test\n'),
            (('build_exit',), 1),
        ):
            with self.subTest(path=path):
                record, item, raw, proof, control = fixture()
                node = control
                for key in path[:-1]: node = node[key]
                node[path[-1]] = value
                altered = encoded(json.dumps(control))
                proof['logs'][altered['sha256']] = altered
                record['origin']['execution']['incremental_control'] = {'sha256_ref': altered['sha256']}
                with self.assertRaises(receipts.ReceiptError):
                    self.bound(record, item, raw, proof)

    def test_rekeyed_self_consistent_control_blob_is_refused(self):
        record, item, raw, proof, control = fixture()
        reference = record['origin']['execution']['incremental_control']['sha256_ref']
        control['build_stderr_sha256'] = '0' * 64
        # The value is internally well formed, but still lives under the old key.
        proof['logs'][reference] = encoded(json.dumps(control))
        with self.assertRaises(receipts.ReceiptError):
            self.bound(record, item, raw, proof)

    def test_matching_tampered_lists_cannot_omit_executed_case(self):
        record, item, raw, proof, control = fixture()
        other = 'other: test\n'
        source_blob = encoded(other)
        proof['logs'][source_blob['sha256']] = source_blob
        record['origin']['execution']['source_list_log'] = {'sha256_ref': source_blob['sha256']}
        control['list_stdout'] = other
        control['list_stdout_sha256'] = source_blob['sha256']
        control['source_inventory_sha256'] = source_blob['sha256']
        control_blob = encoded(json.dumps(control))
        proof['logs'][control_blob['sha256']] = control_blob
        record['origin']['execution']['incremental_control'] = {'sha256_ref': control_blob['sha256']}
        item['inventory']['ids'] = ['other']
        item['inventory']['list_log'] = source_blob
        with self.assertRaises(receipts.ReceiptError):
            self.bound(record, item, raw, proof)

    def test_uncontrolled_profile_or_extension_is_refused(self):
        for mutation in ('no_control', 'unknown_profile', 'canonical_with_extension',
                         'changed_source_inventory', 'changed_source_artifact'):
            with self.subTest(mutation=mutation):
                record, item, raw, proof, _ = fixture()
                witness = record['origin']['execution']
                if mutation == 'no_control': witness.pop('incremental_control')
                elif mutation == 'unknown_profile': witness['fingerprint']['profile'] = 123
                elif mutation == 'canonical_with_extension':
                    witness['fingerprint']['profile'] = q.TEST_PROFILE_FINGERPRINTS[0]
                elif mutation == 'changed_source_inventory':
                    other = encoded('other: test\n')
                    proof['logs'][other['sha256']] = other
                    witness['source_list_log'] = {'sha256_ref': other['sha256']}
                else: witness['source_artifact']['executable'] = '/owned/other'
                with self.assertRaises(receipts.ReceiptError):
                    self.bound(record, item, raw, proof)
