"""Nightly tests keep shipping media and advisory mutation outcomes distinct."""
import os
import re
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class NightlyRuntimeCase(unittest.TestCase):
    def test_both_nightly_jobs_use_shipping_media_on_the_non_voter_pool(self):
        source = (ROOT / '.github/workflows/validation-nightly.yml').read_text()
        for name, command in (
            ('deep-validation', 'python3 scripts/release-readiness --memory-gb 8 --host-network make validate-nightly'),
            ('mutation', 'python3 scripts/release-readiness --memory-gb 8 bash scripts/nightly-mutation'),
        ):
            with self.subTest(job=name):
                block = re.split(r'(?m)^  [\w-]+:\n', source.split('\n  '+name+':\n', 1)[1], maxsplit=1)[0]
                self.assertIn('runs-on: [incus, ci-nonvoter-heavy-amd64]', block)
                self.assertIn(command, block)
                self.assertNotIn('uses: ./.github/actions/ffmpeg', block)
                self.assertIn("if: ${{ !inputs.fuzz_only }}", block)
                self.assertIn('uses: ./.github/actions/cargo-cache-finalize', block)
        for path in (ROOT / '.github/workflows').glob('*.yml'):
            self.assertNotIn('ffmpeg-6', path.read_text(), str(path))
        self.assertNotIn('ffmpeg-6', (ROOT / 'validation/runner-fleet.toml').read_text())
        self.assertNotIn('ffmpeg-6', (ROOT / 'deploy/runner-recovery/runner-vms.example.json').read_text())

    def mutation(self, *, history='crates/a.rs\ncrates/a.rs\nREADME.md\n', install=0, outcome=0):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools, cache = root / 'image-bin', root / 'cargo'
            tools.mkdir(); (cache / 'bin').mkdir(parents=True)
            def executable(path, script):
                path.write_text('#!/bin/sh\n'+script); path.chmod(0o755)
            executable(tools / 'git', "printf '%s' \"$HISTORY\"\n")
            executable(tools / 'ffmpeg', 'echo shipping-runtime\n')
            executable(cache / 'bin/ffmpeg', 'echo wrong-cached-runtime\n')
            executable(tools / 'cargo', '''if [ "$1" = install ]; then
  touch installed
  exit "$INSTALL"
fi
exec cargo-mutants "$@"
''')
            executable(cache / 'bin/cargo-mutants', '''ffmpeg > selected-runtime
printf '%s\\n' "$@" > mutation-args
echo MISSED > target/mutants/outcome
exit "$OUTCOME"
''')
            executable(tools / 'timeout', 'shift; shift; shift; exec "$@"\n')
            result = subprocess.run(['bash', str(ROOT / 'scripts/nightly-mutation')], cwd=root,
                                    env={**os.environ, 'PATH': str(tools)+':'+os.environ['PATH'],
                                         'CARGO_HOME': str(cache), 'HISTORY': history,
                                         'INSTALL': str(install), 'OUTCOME': str(outcome)},
                                    text=True, capture_output=True)
            files = {str(p.relative_to(root)): p.read_text() for p in root.rglob('*') if p.is_file()}
            return result, files

    def test_mutation_installed_tool_is_found_without_overriding_shipping_ffmpeg(self):
        result, files = self.mutation(outcome=2)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(files['selected-runtime'], 'shipping-runtime\n')
        self.assertEqual(files['mutation-args'].count('crates/a.rs'), 1)
        self.assertIn('exit status: 2', files['target/mutants/summary.md'])
        self.assertIn('Surviving or timed-out', files['target/mutants/summary.md'])

    def test_mutation_installation_failure_is_not_report_only(self):
        result, files = self.mutation(install=23)
        self.assertEqual(result.returncode, 23)
        self.assertNotIn('mutation-args', files)

    def test_no_recent_rust_changes_needs_no_mutation_install(self):
        result, files = self.mutation(history='README.md\n')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn('installed', files)
        self.assertIn('no Rust files changed', files['target/mutants/summary.md'])

    def test_campaign_timeout_is_reported_as_partial_evidence(self):
        result, files = self.mutation(outcome=124)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Campaign time budget reached', files['target/mutants/summary.md'])

    def test_command_launch_failures_are_not_report_only(self):
        for status in (1, 4, 5, 6, 70, 125, 126, 127):
            with self.subTest(status=status):
                result, files = self.mutation(outcome=status)
                self.assertEqual(result.returncode, status)
                self.assertIn('not a mutation result', files['target/mutants/summary.md'])

    def test_killed_campaign_does_not_claim_deadline_expired(self):
        result, files = self.mutation(outcome=137)
        self.assertEqual(result.returncode, 137, result.stderr)
        self.assertIn('cause is unproven', files['target/mutants/summary.md'])
        self.assertNotIn('time budget reached', files['target/mutants/summary.md'])
