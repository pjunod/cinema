"""Offline contract checks; these do not qualify native coverage execution."""
import argparse
import hashlib
import io
import json
import os
import subprocess
import sys
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import campaign


class CollectorGuards(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='unit-coverage-guards-')
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.root = self.directory / 'source'
        self.root.mkdir()
        (self.root / 'main.rs').write_text('fn main() {}\n')
        self.commit = 'a' * 40
        self.archive = self.directory / 'source.tgz'
        with tarfile.open(self.archive, 'w:gz', format=tarfile.PAX_FORMAT,
                          pax_headers={'comment': self.commit}) as archive:
            archive.add(self.root / 'main.rs', arcname='main.rs')
        self.sha = campaign.digest(self.archive)

    def verify(self, *, allow_generated_fixtures=False):
        return campaign.verify_source(self.root, self.archive, self.sha, self.commit,
                                      allow_generated_fixtures=allow_generated_fixtures)

    def test_exact_archive_and_commit(self):
        self.assertEqual(self.verify()['files_verified'], 1)

    def test_changed_source_refuses(self):
        (self.root / 'main.rs').write_text('fn main() { panic!() }\n')
        with self.assertRaisesRegex(ValueError, 'source differs'):
            self.verify()

    def test_extra_source_refuses(self):
        (self.root / 'injected.rs').write_text('injected')
        with self.assertRaisesRegex(ValueError, 'unarchived'):
            self.verify()

    def test_only_post_test_verification_accepts_the_known_completed_fixtures(self):
        for relative in campaign.GENERATED_FIXTURE_PATHS:
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b'synthetic generated media, not source')
        with self.assertRaisesRegex(ValueError, 'unarchived'):
            self.verify()
        result = self.verify(allow_generated_fixtures=True)
        self.assertEqual(result['files_verified'], 1)
        self.assertEqual(result['generated_fixture_files'],
                         sorted(map(str, campaign.GENERATED_FIXTURE_PATHS)))
        self.assertEqual(len(result['generated_fixture_files']), 16)

    def test_generated_fixture_exception_never_accepts_injected_source_or_other_paths(self):
        for relative in ('target/fixtures/injected.rs', 'target/fixtures/injected.sql',
                         'target/fixtures/injected.toml', 'target/fixtures/unknown.mkv',
                         'target/fixtures/h264.mkv.1.0.tmp',
                         'target/fixtures/nested/h264.mkv', 'target/injected.rs'):
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b'injected')
            with self.subTest(path=relative), self.assertRaisesRegex(ValueError, 'unarchived'):
                self.verify(allow_generated_fixtures=True)
            path.unlink()

    def test_generated_fixture_exception_still_rejects_links_and_nonregular_entries(self):
        path = self.root / 'target/fixtures/h264.mkv'
        path.parent.mkdir(parents=True)
        path.symlink_to(self.root / 'main.rs')
        with self.assertRaisesRegex(ValueError, 'source link'):
            self.verify(allow_generated_fixtures=True)
        path.unlink()
        if hasattr(os, 'mkfifo'):
            os.mkfifo(path)
            with self.assertRaisesRegex(ValueError, 'non-regular generated fixture'):
                self.verify(allow_generated_fixtures=True)

    def test_generated_fixture_exception_keeps_every_archived_byte_checked(self):
        path = self.root / 'target/fixtures/h264.mkv'
        path.parent.mkdir(parents=True)
        path.write_bytes(b'archived fixture')
        with tarfile.open(self.archive, 'w:gz', format=tarfile.PAX_FORMAT,
                          pax_headers={'comment': self.commit}) as archive:
            archive.add(self.root / 'main.rs', arcname='main.rs')
            archive.add(path, arcname='target/fixtures/h264.mkv')
        self.sha = campaign.digest(self.archive)
        verified = self.verify(allow_generated_fixtures=True)
        self.assertEqual(verified['files_verified'], 2)
        self.assertEqual(verified['generated_fixture_files'], [])
        path.write_bytes(b'changed archived fixture')
        with self.assertRaisesRegex(ValueError, 'source differs'):
            self.verify(allow_generated_fixtures=True)
        path.write_bytes(b'archived fixture')
        (self.root / 'main.rs').write_text('fn injected() {}')
        with self.assertRaisesRegex(ValueError, 'source differs'):
            self.verify(allow_generated_fixtures=True)

    def test_commit_and_archive_digest_refuse(self):
        for sha, commit in [('0' * 64, self.commit), (self.sha, 'b' * 40)]:
            with self.assertRaises(ValueError):
                campaign.verify_source(self.root, self.archive, sha, commit)

    def test_symlink_and_git_directory_refuse(self):
        (self.root / 'other').symlink_to(self.directory)
        with self.assertRaises(ValueError):
            self.verify()
        (self.root / 'other').unlink()
        (self.root / '.git').mkdir()
        with self.assertRaises(ValueError):
            self.verify()

    def test_unsafe_archive_member_refuses(self):
        with tarfile.open(self.archive, 'w:gz', format=tarfile.PAX_FORMAT,
                          pax_headers={'comment': self.commit}) as archive:
            item = tarfile.TarInfo('../escape.rs'); item.size = 1
            archive.addfile(item, io.BytesIO(b'x'))
        self.sha = campaign.digest(self.archive)
        with self.assertRaisesRegex(ValueError, 'unsafe'):
            self.verify()

    def test_show_env_supports_pinned_wrapper_and_plain_flags(self):
        for text in [
            'export RUSTFLAGS="-C instrument-coverage --cfg=coverage"\n',
            'export __CARGO_LLVM_COV_RUSTC_WRAPPER_RUSTFLAGS="-C\x1finstrument-coverage\x1f--cfg=coverage"\nexport RUSTC_WRAPPER="/cache/bin/cargo-llvm-cov"\n',
        ]:
            self.assertTrue(campaign.parse_show_env(text))
        for text in ['export RUSTFLAGS="-C instrument-coverage"\n',
                     'export RUSTFLAGS=x\nexport RUSTFLAGS=y\n', 'echo injected\n']:
            with self.assertRaises(ValueError):
                campaign.parse_show_env(text)

    def test_integer_counters_only(self):
        def report(count, covered):
            return {'data': [{'totals': {'lines': {'count': count, 'covered': covered}}}]}
        self.assertEqual(campaign.validated_lines(report(100, 85))['percent'], 85)
        for count, covered in [(0, 0), (1, 2), (True, 1), (10, 8.5)]:
            with self.assertRaises(ValueError):
                campaign.validated_lines(report(count, covered))

    def test_success_inputs_absent_on_failure(self):
        output = self.directory / 'output'
        argv = ['--source-archive', str(self.archive), '--source-sha256', self.sha,
                '--source-commit', self.commit, '--source-epoch', '0', '--output', str(output)]
        with patch.dict(os.environ, {'CARGO_HOME': '/opt/coverage-cache/cargo-home',
                                    'RUSTUP_HOME': '/opt/coverage-cache/rustup',
                                    'CARGO_TARGET_DIR': '/opt/coverage-cache/coverage/target'}), \
             patch.object(campaign.Campaign, 'collect', side_effect=RuntimeError('required phase failed')):
            self.assertEqual(campaign.main(argv), 1)
        self.assertFalse((output / 'cov.json').exists())
        self.assertFalse((output / 'receipt.json').exists())
        self.assertEqual(json.loads((output / 'failure.json').read_text())['status'], 'failed')

    def test_failed_sharing_preflight_prevents_expensive_default_execution(self):
        collector = object.__new__(campaign.Campaign)
        collector.root = self.root
        collector.output = self.directory / 'early-failure'
        collector.output.mkdir()
        collector.args = argparse.Namespace(source_archive=self.archive,
            source_sha256=self.sha, source_commit=self.commit, source_epoch=0, native=False)
        phases = []
        def fail_sharing(*_args):
            phases.append('sharing')
            raise RuntimeError('sharing preflight failed')
        with patch.object(collector, 'bootstrap', side_effect=lambda: phases.append('bootstrap')), \
             patch.object(collector, 'build', return_value=({}, {})), \
             patch.object(collector, 'verify_normal_bins'), \
             patch.object(collector, 'preflight', side_effect=lambda *_: phases.append('preflight')), \
             patch.object(collector, 'sharing', side_effect=fail_sharing), \
             patch.object(collector, 'run') as expensive:
            with self.assertRaisesRegex(RuntimeError, 'sharing preflight failed'):
                collector.collect()
            expensive.assert_not_called()
        self.assertEqual(phases, ['bootstrap', 'preflight', 'sharing'])
        self.assertFalse((collector.output / 'cov.json').exists())
        self.assertFalse((collector.output / 'receipt.json').exists())

    def test_runtime_escrow_failure_cannot_export_or_publish(self):
        collector = object.__new__(campaign.Campaign)
        collector.root = self.root
        collector.output = self.directory / 'escrow-failure'
        collector.output.mkdir()
        collector.args = argparse.Namespace(source_archive=self.archive,
            source_sha256=self.sha, source_commit=self.commit, source_epoch=0, native=False)
        executed = []
        def run(name, *_args, **_kwargs):
            executed.append(name)
            return 0
        with patch.object(collector, 'bootstrap'), \
             patch.object(collector, 'build', return_value=({}, {})), \
             patch.object(collector, 'verify_normal_bins'), \
             patch.object(collector, 'preflight'), \
             patch.object(collector, 'sharing'), \
             patch.object(collector, 'augmented', return_value=(collector.output / 'expanded', 'synthetic')), \
             patch.object(collector, 'escrow_runtime_outputs', side_effect=ValueError('foreign profile')), \
             patch.object(collector, 'run', side_effect=run):
            with self.assertRaisesRegex(ValueError, 'foreign profile'):
                collector.collect()
        self.assertEqual(executed, ['default-tests'])
        self.assertFalse((collector.output / 'cov.json').exists())
        self.assertFalse((collector.output / 'receipt.json').exists())

    def test_completed_campaign_records_generated_media_without_treating_it_as_source(self):
        collector = object.__new__(campaign.Campaign)
        collector.root = self.root
        collector.output = self.directory / 'completed'
        collector.output.mkdir()
        collector.args = argparse.Namespace(source_archive=self.archive,
            source_sha256=self.sha, source_commit=self.commit, source_epoch=0, native=False)
        collector.tools = {'synthetic': 'offline protocol test, not native qualification'}
        collector.llvm = self.directory / 'synthetic-tools'
        def generate_fixture(*_args):
            fixture = self.root / 'target/fixtures/h264.mkv'
            fixture.parent.mkdir(parents=True)
            fixture.write_bytes(b'synthetic generated media')
        def run(name, *_args, **_kwargs):
            if name == 'export':
                report = collector.output / 'report/coverage.json'
                report.parent.mkdir()
                report.write_text(json.dumps({'data': [{'totals': {'lines': {
                    'count': 10, 'covered': 9, 'percent': 90}}}]}))
            return 0
        with patch.object(collector, 'bootstrap'), \
             patch.object(collector, 'build', return_value=({}, {})), \
             patch.object(collector, 'verify_normal_bins'), \
             patch.object(collector, 'preflight'), \
             patch.object(collector, 'sharing', side_effect=generate_fixture), \
             patch.object(collector, 'escrow_runtime_outputs'), \
             patch.object(collector, 'augmented', return_value=(collector.output / 'expanded',
                 {'mode': 'synthetic offline test'})), \
             patch.object(collector, 'run', side_effect=run):
            collector.collect()
        initial = json.loads((collector.output / 'source-verification.json').read_text())
        final = json.loads((collector.output / 'source-post-verification.json').read_text())
        receipt = json.loads((collector.output / 'receipt.json').read_text())
        self.assertEqual(initial['generated_fixture_files'], [])
        self.assertEqual(final['generated_fixture_files'], ['target/fixtures/h264.mkv'])
        self.assertEqual(receipt['generated_fixture_files'], final['generated_fixture_files'])
        self.assertEqual(receipt['files_verified'], 1)

    def test_no_native_threshold_or_driver_corpus(self):
        self.assertEqual(len(campaign.NATIVE), 12)
        self.assertEqual(len(campaign.AUGMENTED), 9)
        self.assertEqual(campaign.EXCLUDED, 'plurx-cluster-check')
        self.assertIn(('plurx-core', 'store_contract', 'test', 1), campaign.AUGMENTED)



class RuntimeEscrowGuards(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='runtime-escrow-guards-')
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.root = self.base / 'source'
        self.root.mkdir()
        (self.root / 'main.rs').write_text('fn main() {}\n')
        self.archive = self.base / 'source.tgz'
        self.commit = 'a' * 40
        self.make_archive(['main.rs'])
        self.output = self.base / 'cohort'
        self.output.mkdir()
        self.profiles = self.output / 'profiles'
        self.profiles.mkdir()
        self.llvm = self.base / 'pinned-llvm'
        self.binary = self.base / 'exact-core-elf'
        self.binary.write_bytes(b'\x7fELFsynthetic exact object')
        self.binary_id = 'abcdef12' * 5
        self.profile_ids = 'Binary IDs:\n' + self.binary_id + '\n'
        self.manifests = [self.output / 'default.json', self.output / 'augmented.json']
        self.write_manifests()

    def make_archive(self, paths):
        with tarfile.open(self.archive, 'w:gz', format=tarfile.PAX_FORMAT,
                          pax_headers={'comment': self.commit}) as archive:
            for path in paths:
                archive.add(self.root / path, arcname=path)
        self.sha = campaign.digest(self.archive)

    def write_manifests(self):
        metadata = self.binary.stat()
        record = {'path': str(self.binary), 'sha256': campaign.digest(self.binary),
                  'device': metadata.st_dev, 'inode': metadata.st_ino,
                  'bytes': metadata.st_size, 'mtime_ns': metadata.st_mtime_ns}
        for manifest in self.manifests:
            manifest.write_text(json.dumps({'source_sha256': self.sha, 'objects': [record]}))

    def profile(self, directory='crates/plurx-core', name='default_123_0_456.profraw'):
        path = self.root / directory / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b'synthetic raw profile; real format tested separately')
        return path

    def tool(self, program, arguments, env):
        self.assertEqual(program.parent, self.llvm)
        if program.name == 'llvm-profdata':
            self.assertEqual(arguments[:2], ['show', '--binary-ids'])
            return 'Instrumentation level: Front-end\n' + self.profile_ids
        self.assertEqual(program.name, 'llvm-readobj')
        self.assertEqual(arguments[0], '--notes')
        return 'NoteSections [\n Build ID: ' + self.binary_id + '\n]\n'

    def escrow(self):
        with patch.object(campaign, 'llvm_output', side_effect=self.tool):
            return campaign.escrow_generated_outputs(self.root, self.archive, self.sha,
                self.manifests, self.output, self.profiles, self.llvm, {})

    def test_valid_profiles_and_exact_pipe_are_preserved_without_changing_archived_bytes(self):
        core = self.profile()
        desktop = self.profile('crates/plurx-desktop-control')
        pipe = self.root / campaign.PIPE_FIXTURE_PATH
        pipe.write_bytes(b'partial')
        hashes = {str(path.relative_to(self.root)): campaign.digest(path)
                  for path in (core, desktop, pipe)}
        receipt = self.escrow()
        self.assertEqual(len(receipt['files']), 3)
        for record in receipt['files']:
            self.assertTrue(record['moved'])
            self.assertEqual(campaign.digest(record['destination']), hashes[record['original_relative_path']])
            self.assertFalse(Path(record['source']).exists())
        self.assertEqual(len(list(self.profiles.glob('*.profraw'))), 2)
        self.assertEqual((self.root / 'main.rs').read_text(), 'fn main() {}\n')
        self.assertTrue(campaign.verify_source(self.root, self.archive, self.sha, self.commit))

    def test_identity_tool_output_is_bounded_while_reading_and_status_is_required(self):
        self.assertEqual(campaign.llvm_output(sys.executable, ['-c', 'print("Binary IDs:")'], {}),
                         'Binary IDs:\n')
        for channel in ('stdout', 'stderr'):
            script = 'import sys; sys.' + channel + '.write("x" * (128 * 1024 + 1))'
            with self.subTest(channel=channel), self.assertRaisesRegex(ValueError, 'output exceeds'):
                campaign.llvm_output(sys.executable, ['-c', script], {})
        with self.assertRaises(subprocess.CalledProcessError):
            campaign.llvm_output(sys.executable, ['-c', 'raise SystemExit(13)'], {})

    def test_malformed_absent_and_foreign_binary_ids_never_move_the_profile(self):
        path = self.profile()
        for ids in ('', 'Binary IDs:\n', 'Binary IDs:\nnot-hex\n',
                    'Binary IDs:\n' + self.binary_id + '\n' + self.binary_id + '\n',
                    'Binary IDs:\n' + 'f' * 40 + '\n',
                    'Binary IDs:\n' + self.binary_id + '\n' + 'f' * 40 + '\n'):
            with self.subTest(ids=ids):
                self.profile_ids = ids
                with self.assertRaises(ValueError):
                    self.escrow()
                self.assertTrue(path.exists())
                self.assertFalse(list(self.profiles.iterdir()))
                self.assertFalse((self.output / 'runtime-escrow.json').exists())

    def test_changed_object_or_another_archive_cannot_authorize_a_profile(self):
        path = self.profile()
        self.binary.write_bytes(b'\x7fELFdifferent object')
        with self.assertRaisesRegex(ValueError, 'object changed'):
            self.escrow()
        self.write_manifests()
        receipt = json.loads(self.manifests[1].read_text())
        receipt['source_sha256'] = 'f' * 64
        self.manifests[1].write_text(json.dumps(receipt))
        with self.assertRaisesRegex(ValueError, 'another source archive'):
            self.escrow()
        self.assertTrue(path.exists())
        self.assertFalse(list(self.profiles.iterdir()))

    def test_runtime_links_nonregular_empty_and_oversized_files_are_refused(self):
        path = self.profile()
        path.unlink()
        path.symlink_to(self.root / 'main.rs')
        with self.assertRaises(ValueError):
            self.escrow()
        path.unlink()
        if hasattr(os, 'mkfifo'):
            os.mkfifo(path)
            with self.assertRaises(ValueError):
                self.escrow()
            path.unlink()
        path.touch()
        with self.assertRaises(ValueError):
            self.escrow()
        path.write_bytes(b'bounded profile')
        for bound, value in [('MAX_ESCAPED_PROFILE_BYTES', 1),
                             ('MAX_ESCAPED_PROFILE_COUNT', 0),
                             ('MAX_ESCAPED_PROFILE_TOTAL', 1)]:
            with self.subTest(bound=bound), patch.object(campaign, bound, value), self.assertRaises(ValueError):
                self.escrow()
        self.assertTrue(path.exists())

    def test_wrong_pipe_bytes_are_not_an_output_allowance(self):
        pipe = self.root / campaign.PIPE_FIXTURE_PATH
        pipe.parent.mkdir(parents=True)
        for data in (b'', b'changed', b'partial plus extra'):
            with self.subTest(data=data):
                pipe.write_bytes(data)
                with self.assertRaises(ValueError):
                    self.escrow()
                self.assertEqual(pipe.read_bytes(), data)
        pipe.unlink()
        pipe.symlink_to(self.root / 'main.rs')
        with self.assertRaises(ValueError):
            self.escrow()

    def test_archived_runtime_named_file_is_never_removed(self):
        profile = self.profile()
        relative = str(profile.relative_to(self.root))
        self.make_archive(['main.rs', relative])
        self.write_manifests()
        before = profile.read_bytes()
        with self.assertRaisesRegex(ValueError, 'archived bytes'):
            self.escrow()
        self.assertEqual(profile.read_bytes(), before)

    def test_unknown_paths_and_generated_rust_are_not_escrow_exceptions(self):
        unknown = self.profile('crates/other')
        invalid_name = self.profile(name='default_unrecognized.profraw')
        generated = self.root / 'target/generated/bincode/EncKeys_Encode.rs'
        generated.parent.mkdir(parents=True)
        generated.write_text('injected source')
        receipt = self.escrow()
        self.assertEqual(receipt['files'], [])
        for path in (unknown, invalid_name, generated):
            self.assertTrue(path.exists())
        with self.assertRaisesRegex(ValueError, 'unarchived'):
            campaign.verify_source(self.root, self.archive, self.sha, self.commit,
                                   allow_generated_fixtures=True)

    def test_llvm_failure_or_input_mutation_cannot_move_a_profile(self):
        path = self.profile()
        with patch.object(campaign, 'llvm_output', side_effect=RuntimeError('LLVM refused input')):
            with self.assertRaises(RuntimeError):
                campaign.escrow_generated_outputs(self.root, self.archive, self.sha,
                    self.manifests, self.output, self.profiles, self.llvm, {})
        def mutate(program, arguments, env):
            result = self.tool(program, arguments, env)
            if program.name == 'llvm-profdata':
                path.write_bytes(b'replaced while LLVM inspected it')
            return result
        with patch.object(campaign, 'llvm_output', side_effect=mutate), self.assertRaisesRegex(ValueError, 'changed'):
            campaign.escrow_generated_outputs(self.root, self.archive, self.sha,
                self.manifests, self.output, self.profiles, self.llvm, {})
        self.assertTrue(path.exists())
        self.assertFalse(list(self.profiles.iterdir()))

    def test_existing_profile_destination_is_never_overwritten(self):
        path = self.profile()
        relative = str(path.relative_to(self.root))
        path_digest = hashlib.sha256(relative.encode()).hexdigest()[:16]
        destination = self.profiles / ('escaped-' + path_digest + '-' + campaign.digest(path) + '.profraw')
        destination.write_bytes(b'unrelated existing profile')
        with self.assertRaises(FileExistsError):
            self.escrow()
        self.assertTrue(path.exists())
        self.assertEqual(destination.read_bytes(), b'unrelated existing profile')
        self.assertFalse((self.output / 'runtime-escrow.json').exists())


if __name__ == '__main__':
    unittest.main()
