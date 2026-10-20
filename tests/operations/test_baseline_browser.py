"""Frozen browser continuation keeps source identity and independent execution."""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / '.github/workflows/baseline-browser.yml'


def embedded(marker):
    text = WORKFLOW.read_text()
    return textwrap.dedent(text.split("          python3 - <<'" + marker + "'\n", 1)[1]
                           .split('\n          ' + marker, 1)[0])


class BaselineBrowserCase(unittest.TestCase):
    def fixture(self, directory, cell='seek'):
        base = Path(directory)
        checkout = base / 'checkout'
        checkout.mkdir()
        (checkout / '.gitignore').write_text('/target\n')
        (checkout / 'source.txt').write_text('frozen\n')
        for args in [('init', '-q'), ('add', '.'),
                     ('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
                      '-c', 'commit.gpgsign=false', 'commit', '-qm', 'fixture')]:
            subprocess.run(['git', *args], cwd=checkout, check=True, capture_output=True)
        sha = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=checkout, text=True).strip()
        runtime = base / 'runtime'
        runtime.mkdir()
        environment_file = base / 'github-env'
        environment = os.environ | dict(EXPECTED_SHA=sha, DIAGNOSTIC_BASELINE='true',
            WORKFLOW_TRANSPORT_SHA='b' * 40, RUNNER_TEMP=str(runtime),
            CONTINUATION_CELL=cell, GITHUB_RUN_ID='fixture', GITHUB_RUN_ATTEMPT='1',
            GITHUB_ENV=str(environment_file), PYTHONDONTWRITEBYTECODE='1')
        return checkout, environment, environment_file

    def bind(self, checkout, environment, environment_file):
        result = subprocess.run([sys.executable, '-c', embedded('PYBIND')], cwd=checkout,
                                env=environment, capture_output=True, text=True)
        if environment_file.exists():
            environment |= dict(line.split('=', 1) for line in environment_file.read_text().splitlines())
        return result

    def test_exact_native_commands_and_transport_are_bound_separately(self):
        for cell in ('seek', 'cinema'):
            with self.subTest(cell=cell), tempfile.TemporaryDirectory() as directory:
                checkout, environment, environment_file = self.fixture(directory, cell)
                result = self.bind(checkout, environment, environment_file)
                self.assertEqual(result.returncode, 0, result.stderr)
                output = checkout / environment['PLURX_BASELINE_DIR']
                provenance = json.loads((output / 'continuation-provenance.json').read_text())
                self.assertEqual(provenance['tested_sha'], environment['EXPECTED_SHA'])
                self.assertEqual(provenance['workflow_transport_sha'], 'b' * 40)
                manifest = json.loads((output / 'command-manifest.json').read_text())
                command = manifest['cells'][0]
                self.assertEqual(command['id'], cell)
                self.assertEqual(command['timeout_seconds'], 240 if cell == 'seek' else 600)
                if cell == 'cinema':
                    self.assertEqual(command['argv'], ['make', 'cinema-remote-browser-check'])
                else:
                    self.assertEqual(command['argv'][:3], ['python3', '-m', 'validation.baseline_python'])
                    context = json.loads((output / 'context-seek-layout.json').read_text())
                    self.assertEqual(context['test_files'], ['test_seek_controls_layout.py'])
                    self.assertEqual(context['suite_dir'], 'tests/web')
                self.assertFalse(Path(environment['PLAYWRIGHT_BROWSERS_PATH']).is_relative_to(checkout))

    def test_untracked_tooling_fails_without_relaxing_purity_and_records_names(self):
        with tempfile.TemporaryDirectory() as directory:
            checkout, environment, environment_file = self.fixture(directory)
            cache = checkout / '.runner-tool-cache/lane'
            cache.mkdir(parents=True)
            (cache / '.last-used').touch()
            result = self.bind(checkout, environment, environment_file)
            self.assertNotEqual(result.returncode, 0)
            receipt = json.loads((checkout / 'target/full-suite-baseline/fixture-1-browser-seek/source-initial.json').read_text())
            self.assertIn('.runner-tool-cache/lane/.last-used', receipt['status'])
            self.assertIn('Initial checkout must be clean', result.stderr)

    def test_external_tooling_is_required_and_wrong_candidate_rejected(self):
        for variable in ('RUNNER_TEMP', 'EXPECTED_SHA', 'DIAGNOSTIC_BASELINE'):
            with self.subTest(variable=variable), tempfile.TemporaryDirectory() as directory:
                checkout, environment, environment_file = self.fixture(directory)
                environment[variable] = {'RUNNER_TEMP': str(checkout), 'EXPECTED_SHA': 'c' * 40,
                                         'DIAGNOSTIC_BASELINE': 'false'}[variable]
                result = self.bind(checkout, environment, environment_file)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(environment_file.exists())

    def test_after_check_fails_on_generated_file_and_preserves_status(self):
        with tempfile.TemporaryDirectory() as directory:
            checkout, environment, environment_file = self.fixture(directory)
            result = self.bind(checkout, environment, environment_file)
            self.assertEqual(result.returncode, 0, result.stderr)
            before = subprocess.run([sys.executable, '-c', embedded('PYSTATUS')], cwd=checkout,
                                    env=environment, capture_output=True, text=True)
            self.assertEqual(before.returncode, 0, before.stderr)
            (checkout / 'unowned-generated.txt').write_text('unexpected')
            after = subprocess.run([sys.executable, '-c', embedded('PYAFTER')], cwd=checkout,
                                   env=environment, capture_output=True, text=True)
            self.assertNotEqual(after.returncode, 0)
            receipt = json.loads((checkout / environment['PLURX_BASELINE_DIR'] / 'source-after.json').read_text())
            self.assertIn('unowned-generated.txt', receipt['status'])

    def test_independent_cells_provision_early_and_always_retain_original_exit_receipts(self):
        source = WORKFLOW.read_text()
        self.assertIn('workflow_dispatch:', source)
        self.assertIn('ref: ${{ inputs.expected_sha }}', source)
        self.assertIn('fail-fast: false', source)
        self.assertIn('cell: [seek, cinema]', source)
        self.assertNotIn('needs:', source)
        self.assertNotIn('cargo-cache', source)
        self.assertNotIn('rust-toolchain', source)
        self.assertNotIn('git clean', source)
        self.assertNotIn('docker push', source)
        self.assertLess(source.index('Provision headed Cinema browser dependencies before tests'),
                        source.index('Execute exact frozen browser command'))
        self.assertIn('install -y --no-install-recommends xvfb xauth', source)
        self.assertIn('python3 -m validation.baseline_commands --manifest', source)
        for name in ('Assert pristine source after test execution',
                     'Finish candidate-bound resource recording',
                     'Restore persistent runner workspace ownership',
                     'Preserve frozen browser continuation evidence'):
            step = source.split('      - name: ' + name, 1)[1].split('      - name:', 1)[0]
            self.assertIn('if: always()', step)
        execute_step = source.split('      - name: Execute exact frozen browser command', 1)[1].split('      - name:', 1)[0]
        self.assertNotIn('continue-on-error:', execute_step)
        self.assertIn('PLAYWRIGHT_BROWSERS_PATH', source)
        self.assertIn('PYTHONDONTWRITEBYTECODE: "1"', source)


if __name__ == '__main__':
    unittest.main()
