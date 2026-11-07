"""An unprinted native Make command remains bounded by source and real output."""
import copy
import unittest
from unittest.mock import patch
from validation import native_wal_openraft as native
from validation import qualification_unit_continuation as commands
from validation import unit_openraft_package as package
from validation.python_unit_receipts import ReceiptError

BEFORE = '$(CARGO) test --locked -p prior before --lib -- --exact'
AFTER = '$(CARGO) test --locked -p next after --lib -- --exact'
EXTERNAL = '$(CARGO) test --locked --manifest-path $$OPENRAFT_MANIFEST --features generic-snapshot-data ' + package.SELECTOR + ' --lib -- --exact'
RAW = (BEFORE.replace('$(CARGO)', 'cargo') + '\n'
       'test before ... ok\n'
       'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n'
       '   Compiling openraft v0.9.25 (/cache/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/openraft-0.9.25)\n'
       '     Running unittests src/lib.rs (target/openraft-regression/debug/deps/openraft-1234567890abcdef)\n'
       'running 1 test\n'
       'test ' + package.SELECTOR + ' ... ok\n'
       'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 196 filtered out; finished in 0.01s\n'
       + AFTER.replace('$(CARGO)', 'cargo') + '\n')


def fixture():
    return {'source_commit': 'a' * 40, 'exit_code': 0, 'origin': {
        'kind': 'authenticated-ci', 'identity': 'run:1/job:2', 'command': 'make cluster-wal-check',
        'execution': {'job_status': 'success', 'first_line': 5, 'historical_name': 'wal:2'}}}


class NativeWalOpenraftCase(unittest.TestCase):
    def execute(self, record, raw):
        with patch.object(package, 'source_descriptor', return_value={}), patch.object(commands, 'historical_recipe', return_value=[BEFORE, EXTERNAL, AFTER]), patch.object(commands, 'historical_canonical', return_value={'wal:2': EXTERNAL}):
            return native.execution(record, {'invocation': EXTERNAL}, raw)

    def test_native_source_recipe_keeps_actual_unprinted_outcome_scope(self):
        body = self.execute(fixture(), RAW)
        self.assertEqual(commands.retention.log_outcomes(body, 'rust'), {package.SELECTOR: 'pass'})
        self.assertNotIn('test before ... ok', body)
        self.assertNotIn('cargo test', body)

    def test_rejects_unbound_origin_neighbor_target_or_partial_terminal(self):
        record = fixture()
        for key, value in (('kind', 'attested-local'), ('command', 'echo make cluster-wal-check')):
            altered = copy.deepcopy(record); altered['origin'][key] = value
            with self.subTest(field=key), self.assertRaises(ReceiptError):
                self.execute(altered, RAW)
        for key, value in (('job_status', 'failure'), ('first_line', 6)):
            altered = copy.deepcopy(record); altered['origin']['execution'][key] = value
            with self.subTest(execution=key), self.assertRaises(ReceiptError):
                self.execute(altered, RAW)
        for altered in (RAW.replace('target/openraft-regression', 'target/other'),
                        RAW.replace('cargo test --locked -p prior before', 'cargo test --locked -p other before'),
                        RAW.replace('cargo test --locked -p next after', 'cargo test --locked -p other after'),
                        RAW.replace('Compiling openraft v0.9.25', 'Compiling openraft v0.9.26'),
                        RAW.replace(' ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 196', ' ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 196'),
                        RAW.replace('196 filtered out; finished in 0.01s', '0 filtered out; finished in 0.01s'),
                        RAW.replace(AFTER.replace('$(CARGO)', 'cargo'), '')):
            with self.subTest(log=altered), self.assertRaises(ReceiptError):
                self.execute(record, altered)


    def test_preceding_command_cannot_absorb_the_unprinted_openraft_result(self):
        raw = RAW.replace('test before ... ok',
            '     Running unittests src/lib.rs (/cache/debug/deps/prior-1234567890abcdef)\ntest before ... ok')
        record = fixture()
        record['origin']['command'] = BEFORE.replace('$(CARGO)', 'cargo')
        record['origin']['execution'].update(mode='cargo-command', first_line=1, historical_name='wal:1')
        record['coverage'] = {'format': 'rust', 'outcomes': {'before': 'pass'}}
        canonical = {'wal:1': BEFORE, 'wal:2': EXTERNAL, 'wal:3': AFTER}
        with patch.object(package, 'source_descriptor', return_value={}), patch.object(commands, 'historical_recipe', return_value=[BEFORE, EXTERNAL, AFTER]), patch.object(commands, 'historical_canonical', return_value=canonical), patch.object(commands, 'executor_terminal', return_value=None):
            body = commands.bound_execution(record, {'name': 'wal:1', 'invocation': BEFORE}, raw, lambda _: canonical)
        self.assertEqual(commands.retention.log_outcomes(body, 'rust'), {'before': 'pass'})
        self.assertNotIn('test ' + package.SELECTOR, body)


    def test_echoed_next_command_keeps_the_existing_boundary_path(self):
        raw = RAW.replace('test before ... ok',
            '     Running unittests src/lib.rs (/cache/debug/deps/prior-1234567890abcdef)\ntest before ... ok')
        raw = raw.replace('   Compiling openraft', EXTERNAL.replace('$(CARGO)', 'cargo') + '\n   Compiling openraft')
        record = fixture()
        record['origin']['command'] = BEFORE.replace('$(CARGO)', 'cargo')
        record['origin']['execution'].update(mode='cargo-command', first_line=1, historical_name='wal:1')
        record['coverage'] = {'format': 'rust', 'outcomes': {'before': 'pass'}}
        canonical = {'wal:1': BEFORE, 'wal:2': EXTERNAL, 'wal:3': AFTER}
        with patch.object(commands, 'historical_recipe', return_value=[BEFORE, EXTERNAL, AFTER]), patch.object(commands, 'executor_terminal', return_value=None), patch.object(native, 'interval', side_effect=AssertionError('Echoed recipe must use ordinary boundary')):
            body = commands.bound_execution(record, {'name': 'wal:1', 'invocation': BEFORE}, raw, lambda _: canonical)
        self.assertEqual(commands.retention.log_outcomes(body, 'rust'), {'before': 'pass'})
