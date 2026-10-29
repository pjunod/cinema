"""Offline contract checks; these do not qualify native coverage execution."""
import argparse
import hashlib
import io
import json
import os
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


if __name__ == '__main__':
    unittest.main()
