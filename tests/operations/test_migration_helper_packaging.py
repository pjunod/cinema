"""Package actual bounded native describe output with the same executable bytes."""
import argparse
import hashlib
import importlib.machinery
import importlib.util
import json
import signal
from pathlib import Path
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock

from validation.migration_helper_release import executable_digest, read_description

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('helper_packager', str(ROOT / 'scripts/package-storage-migration-helper'))
spec = importlib.util.spec_from_loader(loader.name, loader)
PACKAGER = importlib.util.module_from_spec(spec)
loader.exec_module(PACKAGER)


class MigrationHelperPackagingTests(unittest.TestCase):
    def setUp(self):
        owned = tempfile.TemporaryDirectory(prefix='helper-packaging-')
        self.addCleanup(owned.cleanup)
        self.root = Path(owned.name)
        self.description = dict(version=1, build=dict(commit='a' * 40, tree='b' * 40,
            target='x86_64-unknown-linux-gnu'), source_cohorts=[dict(auth_version=v,
            canonical_layout_fingerprint=str(v - 80) * 64) for v in (81, 82)],
            converter_sha256='d' * 64, native_schema_version=124, manifest_version=1, transition_version=1)
        self.args = argparse.Namespace(binary=self.root / 'helper', output=self.root / 'dist',
            version='1.2.3', commit='a' * 40, tree='b' * 40,
            target='x86_64-unknown-linux-gnu', source_date_epoch=1792400000)
        self.write_helper('print(' + repr(json.dumps(self.description)) + ')\n')

    def write_helper(self, body):
        self.args.binary.write_text('#!' + sys.executable + '\nimport sys\nassert sys.argv[1:] == ["describe"]\n' + body)
        self.args.binary.chmod(0o700)

    def artifacts(self):
        return sorted(path for path in self.args.output.glob('*') if not path.name.startswith('.'))

    def test_actual_describe_and_archive_bind_exact_bytes_and_are_reproducible(self):
        expected = hashlib.sha256(self.args.binary.read_bytes()).hexdigest()
        receipt = PACKAGER.package(self.args)
        bundle, description = sorted(self.artifacts(), key=lambda path: path.suffix != '.gz')
        self.assertEqual(executable_digest(bundle), expected)
        self.assertEqual(read_description(description), self.description)
        self.assertEqual(receipt['executable_sha256'], expected)
        self.assertEqual(receipt['bundle_sha256'], hashlib.sha256(bundle.read_bytes()).hexdigest())
        first = bundle.read_bytes()
        self.args.output = self.root / 'repeat'
        self.assertEqual(PACKAGER.package(self.args), receipt)
        self.assertEqual(next(self.args.output.glob('*.tar.gz')).read_bytes(), first)

    def test_wrong_build_target_malformed_description_and_nonzero_refuse_before_publish(self):
        for body in ["print('{}')\n", "print('not json')\n", 'raise SystemExit(7)\n',
                     'print(' + repr(json.dumps(self.description).replace('a' * 40, 'c' * 40)) + ')\n',
                     'print(' + repr(json.dumps(self.description).replace('x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu')) + ')\n']:
            with self.subTest(body=body):
                self.write_helper(body)
                with self.assertRaises((ValueError, json.JSONDecodeError)):
                    PACKAGER.package(self.args)
                self.assertEqual(self.artifacts(), [])

    def test_describe_is_bounded_by_bytes_on_both_pipes_and_wall_time(self):
        for body in ["sys.stdout.write('x' * 70000)\n", "sys.stderr.write('x' * 17000)\n",
                     'import time\ntime.sleep(20)\n',
                     'import os, time\nos.close(1)\nos.close(2)\ntime.sleep(20)\n']:
            with self.subTest(body=body):
                self.write_helper(body)
                started = time.monotonic()
                with self.assertRaisesRegex(ValueError, 'bound'):
                    PACKAGER.describe(self.args.binary, timeout=0.2)
                self.assertLess(time.monotonic() - started, 3)

    def test_reaped_or_unprovable_child_never_authorizes_a_mutating_signal(self):
        for result in [ChildProcessError(), (12345, 0), (98765, 0)]:
            with self.subTest(result=result):
                child = argparse.Namespace(pid=12345, returncode=None)
                with mock.patch.object(PACKAGER.os, 'waitpid') as wait, \
                     mock.patch.object(PACKAGER.os, 'kill') as kill, \
                     mock.patch.object(PACKAGER.os, 'killpg') as killpg:
                    if isinstance(result, Exception):
                        wait.side_effect = result
                    else:
                        wait.return_value = result
                    if result == (12345, 0):
                        PACKAGER.reap_owned_direct_child(child)
                        self.assertEqual(child.returncode, 0)
                    else:
                        with self.assertRaisesRegex(ValueError, 'no signal sent'):
                            PACKAGER.reap_owned_direct_child(child)
                    kill.assert_not_called()
                    killpg.assert_not_called()

    def test_inherited_ignored_sigchld_refuses_before_spawn_or_cleanup_signal(self):
        with mock.patch.object(PACKAGER.signal, 'getsignal', return_value=signal.SIG_IGN), \
             mock.patch.object(PACKAGER.subprocess, 'Popen') as spawn, \
             mock.patch.object(PACKAGER.os, 'kill') as kill, \
             mock.patch.object(PACKAGER.os, 'killpg') as killpg:
            with self.assertRaisesRegex(ValueError, 'default SIGCHLD'):
                PACKAGER.describe(self.args.binary)
            spawn.assert_not_called()
            child = argparse.Namespace(pid=12345, returncode=None)
            with self.assertRaisesRegex(ValueError, 'default SIGCHLD'):
                PACKAGER.reap_owned_direct_child(child)
            kill.assert_not_called()
            killpg.assert_not_called()

    def test_worker_thread_and_extra_parent_thread_refuse_before_spawn_or_signal(self):
        with mock.patch.object(PACKAGER.subprocess, 'Popen') as spawn, \
             mock.patch.object(PACKAGER.os, 'kill') as kill, \
             mock.patch.object(PACKAGER.os, 'killpg') as killpg, \
             mock.patch.object(PACKAGER.os, 'waitpid') as wait:
            failures = []
            def worker_entry():
                try:
                    PACKAGER.describe(self.args.binary)
                except ValueError as error:
                    failures.append(str(error))
            worker = threading.Thread(target=worker_entry)
            worker.start()
            worker.join(timeout=2)
            self.assertFalse(worker.is_alive())
            self.assertEqual(len(failures), 1)
            self.assertIn('main thread', failures[0])
            release = threading.Event()
            present = threading.Event()
            def extra_thread():
                present.set()
                release.wait(timeout=3)
            extra = threading.Thread(target=extra_thread)
            extra.start()
            self.assertTrue(present.wait(timeout=2))
            try:
                with self.assertRaisesRegex(ValueError, 'exclusive parent thread'):
                    PACKAGER.describe(self.args.binary)
                with self.assertRaisesRegex(ValueError, 'exclusive parent thread'):
                    PACKAGER.reap_owned_direct_child(argparse.Namespace(pid=12345, returncode=None))
            finally:
                release.set()
                extra.join(timeout=2)
            spawn.assert_not_called()
            wait.assert_not_called()
            kill.assert_not_called()
            killpg.assert_not_called()

    def test_mutating_described_executable_refuses_and_original_input_is_untouched(self):
        self.write_helper('with open(__file__, "a") as changed:\n changed.write("# changed\\n")\nprint(' + repr(json.dumps(self.description)) + ')\n')
        original = self.args.binary.read_bytes()
        with self.assertRaisesRegex(ValueError, 'changed'):
            PACKAGER.package(self.args)
        self.assertEqual(self.args.binary.read_bytes(), original)
        self.assertEqual(self.artifacts(), [])

    def test_symlink_input_and_existing_outputs_are_never_adopted_or_overwritten(self):
        actual = self.root / 'actual'
        self.args.binary.rename(actual)
        self.args.binary.symlink_to(actual)
        with self.assertRaises(OSError):
            PACKAGER.package(self.args)
        self.args.binary.unlink()
        actual.rename(self.args.binary)
        PACKAGER.package(self.args)
        existing = {path: path.read_bytes() for path in self.artifacts()}
        with self.assertRaisesRegex(ValueError, 'replace'):
            PACKAGER.package(self.args)
        self.assertEqual({path: path.read_bytes() for path in self.artifacts()}, existing)

    def test_release_job_has_separate_closed_four_target_helper_cohort(self):
        workflow = (ROOT / '.github/workflows/github-release.yml').read_text()
        job = workflow.split('\n  storage_helpers:\n', 1)[1].split('\n  macos_source_parser:', 1)[0]
        self.assertIn('name: release-storage-helper-${{ matrix.target }}', job)
        self.assertEqual(job.count('cargo build --locked --release -p plurx-storage-migrate --target "$TARGET"'), 2)
        self.assertIn('git -C release-source rev-parse HEAD^{tree}', job)
        self.assertIn('PLURX_BUILD_TREE: ${{ needs.resolve.outputs.source_tree }}', job)
        self.assertIn('trusted/scripts/package-storage-migration-helper', job)
        for target in ['x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'x86_64-apple-darwin', 'aarch64-apple-darwin']:
            self.assertEqual(job.count('target: ' + target), 1)
            self.assertEqual(workflow.count('"plurx-storage-migrate-${VERSION}-' + target + '.tar.gz"'), 2)
        self.assertEqual(workflow.count('helpers+=(--migration-helper-description'), 2)
        self.assertNotIn('receipt.json\n          if-no-files-found', job)


if __name__ == '__main__':
    unittest.main()
