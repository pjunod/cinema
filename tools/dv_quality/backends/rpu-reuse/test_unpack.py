"""Targeted archive/path/hash refusal controls for retained prerequisites."""
import hashlib
import io
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
import unpack_prerequisites as loader

ROOT = Path(sys.argv.pop(1))


class UnpackControls(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="reuse-unpack-", dir=ROOT)
        self.root = Path(self.temporary.name)
        self.archive = self.root / "input.tar"
        self.destination = self.root / "new"

    def tearDown(self):
        self.temporary.cleanup()

    def make(self, names=("inputs/input.dat",), kind=tarfile.REGTYPE):
        payload = b"public synthetic prerequisite"
        with tarfile.open(self.archive, "w") as output:
            for name in names:
                info = tarfile.TarInfo(name)
                info.type = kind
                if kind == tarfile.REGTYPE:
                    info.size = len(payload)
                    output.addfile(info, io.BytesIO(payload))
                else:
                    info.linkname = "/outside"
                    output.addfile(info)
        return {"archive_bytes": self.archive.stat().st_size,
                "archive_sha256": hashlib.sha256(self.archive.read_bytes()).hexdigest(),
                "image_id": "sha256:" + "0" * 64,
                "files": {name: hashlib.sha256(payload).hexdigest() for name in names}}

    def test_regular_exact_unpack(self):
        record = self.make()
        result = loader.unpack(self.archive, self.destination, record)
        self.assertEqual(result["verified_files"], 1)

    def test_existing_destination(self):
        record = self.make()
        self.destination.mkdir()
        with self.assertRaisesRegex(ValueError, "new prerequisite directory"):
            loader.unpack(self.archive, self.destination, record)

    def test_hash_mismatch(self):
        record = self.make()
        record["archive_sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "archive identity"):
            loader.unpack(self.archive, self.destination, record)
        self.assertFalse(self.destination.exists())

    def test_member_hash_mismatch(self):
        record = self.make()
        record["files"]["inputs/input.dat"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "extracted prerequisite identity"):
            loader.unpack(self.archive, self.destination, record)

    def test_traversal(self):
        record = self.make(("inputs/../../outside",))
        with self.assertRaisesRegex(ValueError, "unsafe prerequisite member"):
            loader.unpack(self.archive, self.destination, record)
        self.assertFalse(self.destination.exists())

    def test_absolute_member(self):
        record = self.make(("/outside",))
        with self.assertRaisesRegex(ValueError, "unsafe prerequisite member"):
            loader.unpack(self.archive, self.destination, record)

    def test_symlink_member(self):
        record = self.make(kind=tarfile.SYMTYPE)
        with self.assertRaisesRegex(ValueError, "unsafe prerequisite member"):
            loader.unpack(self.archive, self.destination, record)

    def test_duplicate_member(self):
        record = self.make(("inputs/input.dat", "inputs/input.dat"))
        with self.assertRaisesRegex(ValueError, "duplicate prerequisite member"):
            loader.unpack(self.archive, self.destination, record)

    def test_unexpected_member(self):
        record = self.make()
        record["files"] = {}
        with self.assertRaisesRegex(ValueError, "member coverage"):
            loader.unpack(self.archive, self.destination, record)

    def test_archive_symlink(self):
        record = self.make()
        alias = self.root / "alias.tar"
        alias.symlink_to(self.archive)
        with self.assertRaisesRegex(ValueError, "bounded regular prerequisite archive"):
            loader.unpack(alias, self.destination, record)


if __name__ == "__main__":
    unittest.main()
