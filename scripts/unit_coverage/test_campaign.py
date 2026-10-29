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

    def verify(self):
        return campaign.verify_source(self.root, self.archive, self.sha, self.commit)

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

    def test_no_native_threshold_or_driver_corpus(self):
        self.assertEqual(len(campaign.NATIVE), 12)
        self.assertEqual(len(campaign.AUGMENTED), 6)
        self.assertEqual(campaign.EXCLUDED, 'plurx-cluster-check')
        self.assertIn(('plurx-core', 'store_contract', 'test', 1), campaign.AUGMENTED)


if __name__ == '__main__':
    unittest.main()
