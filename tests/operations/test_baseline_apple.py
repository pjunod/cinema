"""No Apple execution: verify exact frozen command selection and simulator ownership."""
from __future__ import annotations

import json
import os
import re
from pathlib import Path
import subprocess
import tempfile
import textwrap
from types import SimpleNamespace
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / '.github/workflows/baseline-apple.yml'
FROZEN = '86f1385005b27122e8a8d3a3d5bd9501eb2fdb16'
UDID = '12345678-1234-1234-1234-123456789ABC'


def embedded(marker):
    return textwrap.dedent(WORKFLOW.read_text().split("          python3 - <<'" + marker + "'\n", 1)[1]
                           .split('\n          ' + marker, 1)[0])


class BaselineAppleCase(unittest.TestCase):
    def environment(self, directory, cell='iphone'):
        root = Path(directory)
        output = root / 'capture'
        output.mkdir(exist_ok=True)
        return dict(PLURX_BASELINE_DIR=str(output), PLURX_TEST_CANDIDATE_SHA='a' * 40,
                    PLURX_TEST_CANDIDATE_TREE='b' * 40, CONTINUATION_CELL=cell,
                    GITHUB_RUN_ID='42', GITHUB_RUN_ATTEMPT='3', GITHUB_ENV=str(root / 'env'),
                    APPLE_IOS_SIM='platform=iOS Simulator,id=' + UDID,
                    APPLE_IPAD_SIM='platform=iOS Simulator,id=' + UDID,
                    APPLE_TVOS_SIM='platform=tvOS Simulator,id=' + UDID)

    def frozen_dry_run(self, directory):
        source = subprocess.check_output(['git', 'show', FROZEN + ':Makefile'], cwd=ROOT, text=True)
        path = Path(directory) / 'FrozenMakefile'
        path.write_text(source)
        return subprocess.check_output(['make', '--no-print-directory', '-f', str(path), '-n', 'apple-test'], cwd=ROOT, text=True)

    def test_each_mobile_destination_uses_original_recipe_pair_and_owns_build_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            dry = self.frozen_dry_run(directory)
            for cell, scheme, test_argument in [('iphone', 'plurx-iOS', 'APPLE_IOS_SIM'),
                                                 ('ipad', 'plurx-iOS', 'APPLE_IPAD_SIM'),
                                                 ('tvos', 'plurx-tvOS', 'APPLE_TVOS_SIM')]:
                with self.subTest(cell=cell):
                    environment = self.environment(directory, cell)
                    with mock.patch.dict(os.environ, environment), mock.patch.object(subprocess, 'check_output', return_value=dry):
                        exec(compile(embedded('PYPLAN'), 'plan', 'exec'), {})
                    output = Path(environment['PLURX_BASELINE_DIR'])
                    manifest = json.loads((output / 'units.json').read_text())
                    command = manifest['cells'][0]
                    self.assertEqual(command['timeout_seconds'], 1200)
                    shell = command['argv'][-1]
                    self.assertEqual(shell.count('build-for-testing'), 1)
                    self.assertEqual(shell.count('test-without-building'), 1)
                    self.assertEqual(shell.count(scheme), 2)
                    self.assertIn(test_argument, shell)
                    self.assertNotIn('apple-tv-navigation', shell)
                    self.assertEqual((output / 'frozen-make-dry-run.txt').read_text(), dry)
                    self.assertEqual(json.loads((output / 'project.json').read_text())['cells'][0]['timeout_seconds'], 120)

    def test_recipe_drift_refuses_native_launch_planning(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.dict(os.environ, self.environment(directory)), \
             mock.patch.object(subprocess, 'check_output', return_value='xcodegen generate\n'):
            with self.assertRaisesRegex(AssertionError, 'recipe inventory changed'):
                exec(compile(embedded('PYPLAN'), 'plan', 'exec'), {})
            self.assertFalse((Path(directory) / 'capture/units.json').exists())

    def test_desktop_build_and_units_are_independent_bounded_native_make_targets(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.dict(os.environ, self.environment(directory, 'desktop')):
            exec(compile(embedded('PYPLAN'), 'plan', 'exec'), {})
            root = Path(directory) / 'capture'
            build = json.loads((root / 'build.json').read_text())['cells'][0]
            units = json.loads((root / 'units.json').read_text())['cells'][0]
            self.assertEqual(build['argv'], ['make', 'desktop-macos-build'])
            self.assertEqual(units['argv'], ['make', 'desktop-macos-test'])
            self.assertEqual(build['timeout_seconds'], 600)
            self.assertEqual(units['timeout_seconds'], 1200)
            self.assertFalse((root / 'project.json').exists())

    def test_ipad_simulator_receipt_precedes_boot_and_both_ios_commands_use_owned_id(self):
        with tempfile.TemporaryDirectory() as directory:
            environment = self.environment(directory, 'ipad')
            root = Path(environment['PLURX_BASELINE_DIR'])
            def boot(command, **kwargs):
                self.assertTrue((root / 'owned-simulator.json').is_file())
                self.assertEqual(command[:2], ['xcrun', 'simctl'])
                self.assertIn(UDID, command)
                return SimpleNamespace(returncode=0)
            with mock.patch.dict(os.environ, environment):
                exec(compile(embedded('PYSIMPLAN'), 'sim-plan', 'exec'), {})
                setup = (root / 'owned-simulator-setup.py').read_text()
                with mock.patch.object(subprocess, 'check_output', return_value=UDID + '\n') as create, \
                     mock.patch.object(subprocess, 'run', side_effect=boot):
                    exec(compile(setup, 'owned-setup', 'exec'), {})
            self.assertEqual(create.call_args.args[0][2], 'create')
            self.assertIn('com.apple.CoreSimulator.SimDeviceType.iPad-Pro-13-inch-M4-8GB', create.call_args.args[0])
            exports = Path(environment['GITHUB_ENV']).read_text().splitlines()
            self.assertEqual(len(exports), 2)
            for key in ('APPLE_IOS_SIM', 'APPLE_IPAD_SIM'):
                self.assertIn(key + '=platform=iOS Simulator,id=' + UDID, exports)

    def test_cleanup_refuses_foreign_device_and_deletes_only_created_id(self):
        for actual_name in ('foreign-user-simulator', 'plurx baseline 42-3-iphone'):
            with self.subTest(name=actual_name), tempfile.TemporaryDirectory() as directory:
                environment = self.environment(directory)
                output = Path(environment['PLURX_BASELINE_DIR'])
                owned = dict(udid=UDID, name='plurx baseline 42-3-iphone', cell='iphone', run_id='42', run_attempt='3')
                (output / 'owned-simulator.json').write_text(json.dumps(owned))
                devices = json.dumps(dict(devices={'runtime': [dict(udid=UDID, name=actual_name),
                                                               dict(udid='OTHER', name='personal')]}))
                with mock.patch.dict(os.environ, environment), mock.patch.object(subprocess, 'check_output', return_value=devices), \
                     mock.patch.object(subprocess, 'run', return_value=SimpleNamespace(returncode=0)) as mutate:
                    if actual_name != owned['name']:
                        with self.assertRaisesRegex(AssertionError, 'refusing delete'):
                            exec(compile(embedded('PYCLEAN'), 'cleanup', 'exec'), {})
                        mutate.assert_not_called()
                    else:
                        exec(compile(embedded('PYCLEAN'), 'cleanup', 'exec'), {})
                        self.assertEqual([call.args[0] for call in mutate.call_args_list],
                                         [['xcrun', 'simctl', 'shutdown', UDID], ['xcrun', 'simctl', 'delete', UDID]])

    def test_unqualified_build_cleanup_blocks_next_native_cell_but_build_failure_does_not(self):
        for error in (False, True):
            with self.subTest(cleanup_error=error), tempfile.TemporaryDirectory() as directory:
                environment = self.environment(directory, 'desktop')
                path = Path(environment['PLURX_BASELINE_DIR']) / 'build'
                path.mkdir()
                cell = dict(returncode=7, success=False)
                if error:
                    cell['cleanup_error'] = 'unqualified'
                (path / 'results.json').write_text(json.dumps(dict(cells=[cell], errors=[])))
                with mock.patch.dict(os.environ, environment):
                    if error:
                        with self.assertRaisesRegex(AssertionError, 'cleanup unqualified'):
                            exec(compile(embedded('PYSAFE'), 'safe', 'exec'), {})
                    else:
                        exec(compile(embedded('PYSAFE'), 'safe', 'exec'), {})

    def test_each_branch_step_caps_leave_at_least_five_minutes_for_job_overhead(self):
        source = WORKFLOW.read_text()
        job_cap = int(re.search(r"^    timeout-minutes: (\d+)$", source, re.M)[1])
        steps = re.split(r"(?m)^      - (?:name|uses): ", source.split('    steps:\n', 1)[1])[1:]
        for cell in ('desktop', 'iphone', 'ipad', 'tvos'):
            with self.subTest(cell=cell):
                total = 0
                for block in steps:
                    cap = re.search(r"^        timeout-minutes: (\d+)$", block, re.M)
                    self.assertIsNotNone(cap, block.splitlines()[0])
                    condition = re.search(r"^        if: (.+)$", block, re.M)
                    if condition:
                        expression = condition[1].replace('${{', '').replace('}}', '').strip()
                        expression = expression.replace('!cancelled()', 'True').replace('always()', 'True')
                        expression = expression.replace('matrix.cell', repr(cell))
                        expression = expression.replace('env.PLURX_BASELINE_DIR', repr('capture'))
                        expression = re.sub(r'steps\.\w+\.outcome', repr('success'), expression)
                        expression = expression.replace('&&', ' and ').replace('||', ' or ')
                        if not eval(expression, {'__builtins__': {}}):
                            continue
                    total += int(cap[1])
                self.assertLessEqual(total, job_cap - 5, (cell, total, job_cap))
        checkout = steps[0]
        self.assertTrue(checkout.startswith('https://data.forgejo.org/actions/checkout@'))
        self.assertIn('timeout-minutes: 3', checkout)

    def test_workflow_serializes_cells_and_uploads_before_next_long_phase(self):
        source = WORKFLOW.read_text()
        self.assertIn('cell: [desktop, iphone, ipad, tvos]', source)
        self.assertIn('fail-fast: false', source)
        self.assertIn('max-parallel: 1', source)
        self.assertIn('group: plurx-apple-silicon-heavy', source)
        self.assertIn('timeout-minutes: 60', source)
        self.assertIn('ref: ${{ inputs.expected_sha }}', source)
        self.assertIn('workflow_transport_sha=transport', source)
        self.assertIn('primary3601s timeout', source)
        self.assertNotIn('apple-tv-navigation-test', source)
        self.assertNotIn('stale_ids', source)
        self.assertNotIn('simctl delete all', source)
        self.assertNotIn('Cache DerivedData', source)
        self.assertLess(source.index('Preserve native setup evidence'), source.index('Generate only the mobile unit project'))
        self.assertLess(source.index('Preserve native project evidence'), source.index('Run fresh exact native unit bodies'))
        self.assertLess(source.index('Preserve native build evidence'), source.index('Run fresh exact native unit bodies'))
        self.assertLess(source.index('Preserve native units evidence'), source.index('Delete only the simulator'))
        for phase in ('setup', 'project', 'build', 'units', 'final'):
            block = source.split('      - name: Preserve native ' + phase + ' evidence', 1)[1].split('      - name:', 1)[0]
            self.assertIn('if: always()', block)
        self.assertIn('steps.safe.outcome ==', source)


if __name__ == '__main__':
    unittest.main()
