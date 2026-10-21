"""The repair gate predeclares every stability run and retains first failures."""
import ast
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import tempfile
import textwrap
import unittest
from unittest.mock import patch

from validation.rust_repair_evidence import load_policy

ROOT = Path(__file__).resolve().parents[2]
SOURCE = (ROOT / '.github/workflows/rust-repair.yml').read_text()


def embedded(marker):
    start = SOURCE.index("<<'" + marker + "'\n") + len("<<'" + marker + "'\n")
    return textwrap.dedent(SOURCE[start:SOURCE.index('\n          ' + marker, start)])


def controller():
    namespace = {'__name__': 'repair_workflow_contract'}
    with patch.dict(os.environ, {'CONTINUATION_CELL': 'rust'}):
        exec(compile(embedded('PYPHASE'), '<repair phase controller>', 'exec'), namespace)
    return namespace


class RepairWorkflowCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.api = controller()
        self.policy = load_policy(ROOT / 'validation/rust_repair_cases.json')
        self.inventory = {'discovery_complete': True, 'binaries': []}
        rows = self.policy['cases'] + self.policy.get('additional_repeated_cases', [])
        for target in ['plurx_core', 'plurxd']:
            executable = Path(self.temp.name) / (target + '-aabbccdd')
            executable.write_text(target)
            self.inventory['binaries'].append(dict(executable=str(executable),
                sha256=hashlib.sha256(executable.read_bytes()).hexdigest(),
                target={'name': target, 'src_path': str(ROOT / ('crates/plurx-core/src/lib.rs'
                    if target == 'plurx_core' else 'crates/plurxd/src/main.rs'))},
                cases=[{'name': row['name'], 'ignored': False} for row in rows if row['target'] == target]))

    def test_only_manual_exact_source_and_original_observer_safeguards(self):
        self.assertIn('  workflow_dispatch:', SOURCE)
        self.assertNotRegex(SOURCE, r'(?m)^  (push|pull_request|schedule):')
        self.assertIn('ref: ${{ inputs.expected_sha }}', SOURCE)
        self.assertIn('assert actual == expected', embedded('PYBIND'))
        self.assertIn('sys.stdlib_module_names', embedded('PYCOPY'))
        self.assertIn('not temporary.is_relative_to(checkout)', embedded('PYCOPY'))
        for marker in ['PYCOPY', 'PYBIND', 'PYPHASE']:
            ast.parse(embedded(marker))

    def test_ten_predeclared_rounds_preserve_all_original_bodies(self):
        cells, bindings = self.api['cohort'](self.inventory)
        self.assertEqual(len(cells), 20)
        self.assertEqual({row['repetition'] for row in bindings.values()}, set(range(1, 11)))
        declared = {(row['target'], row['name']) for row in
                    self.policy['cases'] + self.policy.get('additional_repeated_cases', [])}
        for repetition in range(1, 11):
            observed = {(row['binary']['target']['name'], name) for row in bindings.values()
                        if row['repetition'] == repetition for name in row['names']}
            self.assertEqual(declared, observed)
        for cell in cells:
            self.assertEqual(cell['timeout_seconds'], 180)
            self.assertIn('--exact', cell['argv'])
            self.assertEqual(cell['argv'][-1], '--test-threads=22')

    def test_missing_ignored_or_duplicate_binding_blocks_before_execution(self):
        for mutation in ['missing', 'ignored', 'duplicate']:
            inventory = copy.deepcopy(self.inventory)
            if mutation == 'missing':
                inventory['binaries'][0]['cases'].pop()
            elif mutation == 'ignored':
                inventory['binaries'][0]['cases'][0]['ignored'] = True
            else:
                inventory['binaries'].append(inventory['binaries'][0])
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                self.api['cohort'](inventory)

    def test_policy_cannot_replace_a_failure_or_reduce_repetitions(self):
        for mutation in ['replace', 'rounds']:
            policy = copy.deepcopy(self.policy)
            if mutation == 'replace':
                policy['cases'][0]['name'] = 'another_existing_but_not_original_test'
            else:
                policy['required_repetitions'] = 1
            path = Path(self.temp.name) / 'policy.json'
            path.write_text(json.dumps(policy))
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                load_policy(path)

    def test_bounds_allow_every_declared_cell_and_keep_job_reserve(self):
        cells, _ = self.api['cohort'](self.inventory)
        self.assertEqual(sum(cell['timeout_seconds'] for cell in cells), self.api['BUDGETS']['cohort'])
        bounds = [int(x) for x in re.findall(r'(?m)^        timeout-minutes: (\d+)$', SOURCE)]
        self.assertLessEqual(sum(bounds), 318)
        self.assertIn('    timeout-minutes: 360', SOURCE)
        self.assertIn('Preserve cohort evidence immediately', SOURCE)
        self.assertLess(SOURCE.index('Execute cohort phase'), SOURCE.index('Execute units phase'))

    def test_failed_body_can_preserve_evidence_but_unsafe_cleanup_blocks(self):
        report = {'errors': [], 'blocked_cells': [], 'success': False,
                  'cells': [{'returncode': 101, 'cleanup_complete': True} for _ in range(20)]}
        self.assertTrue(self.api['safe'](report, 20))
        self.assertFalse(report['success'])
        report['cells'][0]['cleanup_complete'] = False
        self.assertFalse(self.api['safe'](report, 20))
        self.assertIn("all(report.get('success') and report.get('safe_to_continue')", embedded('PYPHASE'))


if __name__ == '__main__':
    unittest.main()
