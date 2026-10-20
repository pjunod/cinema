"""The repeat Android capture retains runtime XML, including missing evidence."""
from __future__ import annotations

import json
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / '.github/workflows/baseline-android-jvm.yml'


def collector():
    source = WORKFLOW.read_text()
    body = source.split("          python3 - <<'PYREPORTS'\n", 1)[1].split('\n          PYREPORTS', 1)[0]
    return textwrap.dedent(body)


class BaselineAndroidJvmCase(unittest.TestCase):
    def execute_collector(self, root):
        output = root / 'capture'
        environment = os.environ | {'PLURX_BASELINE_DIR': str(output),
                                    'PLURX_TEST_CANDIDATE_SHA': 'a' * 40,
                                    'PLURX_TEST_CANDIDATE_TREE': 'b' * 40}
        result = subprocess.run([sys.executable, '-c', collector()], cwd=root, env=environment,
                                text=True, capture_output=True, check=False)
        receipt = output / 'android-jvm-reports/collection.json'
        self.assertTrue(receipt.is_file(), result.stderr)
        return result, json.loads(receipt.read_text()), output / 'android-jvm-reports'

    def test_standalone_repeat_binds_transport_separately_and_never_publishes(self):
        source = WORKFLOW.read_text()
        self.assertIn('workflow_dispatch:', source)
        self.assertIn('default: "true"', source)
        self.assertIn('ref: ${{ inputs.expected_sha }}', source)
        self.assertIn("os.environ['DIAGNOSTIC_BASELINE'] == 'true'", source)
        self.assertIn('WORKFLOW_TRANSPORT_SHA: ${{ github.sha }}', source)
        self.assertIn('tested_sha=actual, tested_tree=tree, workflow_transport_sha=transport', source)
        self.assertIn("observation_kind='repeat-runtime-outcome-capture'", source)
        self.assertIn('primary_run_id=5620', source)
        self.assertIn('Preserve primary run 5620 timing as the first observation.', source)
        self.assertIn('make android-test', source)
        self.assertNotIn('needs:', source)
        self.assertNotIn('needs.scope', source)
        self.assertNotIn('docker push', source)
        self.assertNotIn('android-unit-compile-lint', source)
        self.assertLess(source.index('Preserve runtime JUnit XML'),
                        source.index('Finish diagnostic resource recording'))
        self.assertLess(source.index('Preserve runtime JUnit XML'),
                        source.index('Preserve diagnostic inventory and resource evidence'))
        collector_step = source.split('      - name: Preserve runtime JUnit XML', 1)[1].split('      - name:', 1)[0]
        self.assertIn('if: always()', collector_step)
        self.assertIn('retention-days: 14', source)

    def test_copies_xml_and_html_and_counts_observed_cases_not_suite_header(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            junit = root / 'clients/android/app/build/test-results/testDebugUnitTest'
            junit.mkdir(parents=True)
            raw = ('<testsuite tests="999"><testcase classname="Example" name="passes" time="0.1"/>'
                   '<testcase classname="Example" name="fails" time="0.2"><failure>assertion</failure>'
                   '</testcase></testsuite>')
            (junit / 'TEST-Example.xml').write_text(raw)
            html = root / 'clients/android/app/build/reports/tests/testDebugUnitTest'
            html.mkdir(parents=True)
            (html / 'index.html').write_text('<html>runtime report</html>')
            result, report, output = self.execute_collector(root)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(report['testcase_rows'], 2)
            self.assertEqual(report['xml_files'], 1)
            self.assertEqual([row['status'] for row in report['outcomes']], ['passed', 'failed'])
            self.assertEqual((output / 'junit/TEST-Example.xml').read_text(), raw)
            self.assertTrue((output / 'html/index.html').is_file())
            self.assertEqual(report['tested_sha'], 'a' * 40)
            self.assertTrue(report['reports'][0]['sha256'])
            self.assertIn('not independent discovery', report['limitation'])

    def test_test_only_init_preserves_compile_cache_records_digest_and_removes_only_itself(self):
        source = WORKFLOW.read_text()
        install = textwrap.dedent(source.split("          python3 - <<'PYINIT'\n", 1)[1].split('\n          PYINIT', 1)[0])
        cleanup = textwrap.dedent(source.split("          python3 - <<'PYCLEAN'\n", 1)[1].split('\n          PYCLEAN', 1)[0])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            base = root / 'capture'
            base.mkdir()
            (base / 'repeat-provenance.json').write_text('{}')
            environment = os.environ | {'PLURX_BASELINE_DIR': str(base), 'GITHUB_RUN_ID': '123',
                                        'GITHUB_RUN_ATTEMPT': '2', 'GITHUB_ENV': str(root / 'env')}
            result = subprocess.run([sys.executable, '-c', install], cwd=root, env=environment,
                                    text=True, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            init = root / 'clients/android/.gradle-validation/init.d/plurx-baseline-jvm-repeat-123-2.gradle'
            expected = ('allprojects { tasks.withType(org.gradle.api.tasks.testing.Test).configureEach '
                        '{ outputs.upToDateWhen { false }; outputs.cacheIf { false } } }\n')
            self.assertEqual(init.read_text(), expected)
            provenance = json.loads((base / 'repeat-provenance.json').read_text())
            self.assertEqual(provenance['test_execution_override']['sha256'],
                             hashlib.sha256(expected.encode()).hexdigest())
            self.assertTrue(provenance['test_execution_override']['preserves_compile_cache'])
            self.assertEqual((base / 'gradle-test-task-freshness.gradle').read_text(), expected)
            other = init.with_name('unrelated.gradle')
            other.write_text('unrelated configuration')
            environment['PLURX_BASELINE_TEST_INIT'] = str(init.relative_to(root))
            result = subprocess.run([sys.executable, '-c', cleanup], cwd=root, env=environment,
                                    text=True, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(init.exists())
            self.assertTrue(other.exists())
            self.assertNotIn('--rerun-tasks', source)
            cleanup_step = source.split('      - name: Remove only this repeat JVM Test init script', 1)[1].split('      - name:', 1)[0]
            self.assertIn('if: always()', cleanup_step)

    def test_absent_xml_is_explicit_missing_evidence_and_fails_collection(self):
        with tempfile.TemporaryDirectory() as directory:
            result, report, _ = self.execute_collector(Path(directory))
            self.assertEqual(result.returncode, 1)
            self.assertEqual(report['status'], 'missing-or-invalid')
            self.assertEqual(report['testcase_rows'], 0)
            self.assertIn('unavailable', report['errors'][0])

    def test_malformed_xml_is_retained_and_does_not_become_zero_test_success(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            junit = root / 'clients/android/app/build/test-results/testDebugUnitTest'
            junit.mkdir(parents=True)
            (junit / 'TEST-broken.xml').write_text('<testsuite>')
            result, report, output = self.execute_collector(root)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(report['status'], 'missing-or-invalid')
            self.assertTrue((output / 'junit/TEST-broken.xml').is_file())
            self.assertIn('invalid XML', report['errors'][0])


if __name__ == '__main__':
    unittest.main()
