"""Refuse substituted dependencies/source evidence before invoking Docker."""
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
import prerequisite_identity as identity

class PrerequisiteTest(unittest.TestCase):

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name) / 'dependencies'
        self.root.mkdir()
        self.review = Path(self.temp.name) / 'review'
        self.review.mkdir()
        for directory in ('prefix', 'include', 'lib'):
            shutil.copytree(SOURCE / directory, self.root / directory, symlinks=True)
        lock = json.loads(identity.LOCK_PATH.read_text())
        for name in lock['source_evidence']:
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(SOURCE / name, target)
        for name in lock['reviewed_evidence']:
            shutil.copyfile(REVIEW / name, self.review / name)

    def tearDown(self):
        self.temp.cleanup()

    def refused(self):
        with self.assertRaises(ValueError):
            identity.validate(self.root, self.review)

    def corrupt(self, name):
        path = self.root / name
        path.write_bytes(path.read_bytes() + b'changed')
        self.refused()

    def test_reviewed_set_and_after_copy_pass(self):
        self.assertEqual(identity.validate(self.root, self.review)['status'], 'passed')
        self.assertEqual(identity.validate(self.root)['status'], 'passed')

    def test_substituted_static_library_refused(self):
        self.corrupt('lib/libdovi.a')

    def test_substituted_shared_library_refused(self):
        self.corrupt('prefix/lib/aarch64-linux-gnu/libplacebo.so.374')

    def test_changed_capi_header_refused(self):
        self.corrupt('include/libdovi/rpu_parser.h')

    def test_changed_placebo_header_refused(self):
        self.corrupt('prefix/include/libplacebo/colorspace.h')

    def test_changed_source_archive_refused(self):
        self.corrupt('dovi-source.tar.gz')

    def test_changed_cargo_lock_refused(self):
        self.corrupt('dovi_tool-83e1fdad6dcd5995556235946e7c5c0f9010d5a1/Cargo.lock')

    def test_changed_reviewed_revision_refused(self):
        path = self.review / 'receipt.json'
        data = json.loads(path.read_text())
        data['libdovi_revision'] = '0' * 40
        path.write_text(json.dumps(data))
        self.refused()

    def test_extra_dependency_refused(self):
        (self.root / 'lib/unreviewed.a').write_bytes(b'unreviewed')
        self.refused()

    def test_changed_link_refused(self):
        path = self.root / 'prefix/lib/aarch64-linux-gnu/libplacebo.so'
        path.unlink()
        path.symlink_to('/unreviewed/library')
        self.refused()

    def test_after_copy_substitution_refused(self):
        identity.validate(self.root, self.review)
        path = self.root / 'lib/libdovi.a'
        path.write_bytes(path.read_bytes() + b'changed after validation')
        with self.assertRaises(ValueError):
            identity.validate(self.root)
if __name__ == '__main__':
    SOURCE = Path(sys.argv[1]).resolve()
    REVIEW = Path(sys.argv[2]).resolve()
    unittest.main(argv=[sys.argv[0]])
