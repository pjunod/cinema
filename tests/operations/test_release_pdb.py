from pathlib import Path
import os
import shutil
import struct
import subprocess
import tempfile
import unittest
import zipfile

from validation.release_pdb import MSF7, verify_pdb_pair


ROOT = Path(__file__).resolve().parents[2]
GUID = bytes(range(16))


def pair(directory: Path, *, guid: bytes = GUID, age: int = 1, name: bytes = b"plurxd.pdb") -> None:
    codeview = b"RSDS" + GUID + struct.pack("<I", 1) + name + b"\0"
    debug = struct.pack("<IIHH4I", 0, 0, 0, 0, 2, len(codeview), 0x101C, 540)
    executable = bytearray(540 + len(codeview)); executable[:2] = b"MZ"
    struct.pack_into("<I", executable, 60, 64)
    executable[64:68] = b"PE\0\0"
    struct.pack_into("<H", executable, 68, 0x8664)
    struct.pack_into("<H", executable, 70, 1)
    struct.pack_into("<H", executable, 84, 240)
    struct.pack_into("<H", executable, 88, 0x20B)
    struct.pack_into("<II", executable, 88 + 160, 0x1000, 28)
    struct.pack_into("<4I", executable, 328 + 8, len(debug + codeview), 0x1000, len(debug + codeview), 512)
    executable[512:540] = debug; executable[540:] = codeview
    (directory / "plurxd.exe").write_bytes(executable)
    pdb = bytearray(512 * 5); pdb[:32] = MSF7
    struct.pack_into("<6I", pdb, 32, 512, 4, 5, 16, 0, 1)
    struct.pack_into("<I", pdb, 512, 2)
    struct.pack_into("<4I", pdb, 1024, 2, 0, 28, 3)
    struct.pack_into("<3I", pdb, 1536, 20000404, 42, age)
    pdb[1548:1564] = guid
    (directory / "plurxd.pdb").write_bytes(pdb)


class PdbPairCase(unittest.TestCase):
    def test_codeview_basename_is_retained_even_when_the_build_output_was_renamed(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw); pair(root, name=b"plurxd-012345abcdef.pdb")
            self.assertEqual(verify_pdb_pair(root / "plurxd.exe", root / "plurxd.pdb"),
                             "plurxd-012345abcdef.pdb")
            pair(root, name=b"unrelated.pdb")
            with self.assertRaisesRegex(ValueError, "unrelated"):
                verify_pdb_pair(root / "plurxd.exe", root / "plurxd.pdb")

    def test_codeview_guid_and_age_must_match_the_retained_pdb(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw); pair(root)
            verify_pdb_pair(root / "plurxd.exe", root / "plurxd.pdb")
            for guid, age in ((bytes(reversed(GUID)), 1), (GUID, 2)):
                pair(root, guid=guid, age=age)
                with self.assertRaisesRegex(ValueError, "GUID or age"):
                    verify_pdb_pair(root / "plurxd.exe", root / "plurxd.pdb")

    def test_missing_symlink_and_torn_pdb_fail_closed(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw); pair(root)
            (root / "plurxd.pdb").unlink()
            with self.assertRaises(ValueError):
                verify_pdb_pair(root / "plurxd.exe", root / "plurxd.pdb")
            pair(root); (root / "other").write_bytes((root / "plurxd.pdb").read_bytes())
            (root / "plurxd.pdb").unlink(); (root / "plurxd.pdb").symlink_to(root / "other")
            with self.assertRaises(ValueError):
                verify_pdb_pair(root / "plurxd.exe", root / "plurxd.pdb")
            (root / "plurxd.pdb").unlink(); (root / "plurxd.pdb").write_bytes(b"torn")
            with self.assertRaises(ValueError):
                verify_pdb_pair(root / "plurxd.exe", root / "plurxd.pdb")

    def test_windows_archive_retains_the_verified_pair_and_refuses_missing_pdb(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "scripts").mkdir(); (root / "validation").mkdir()
            (root / "deploy/windows").mkdir(parents=True)
            shutil.copy2(ROOT / "scripts/package-windows", root / "scripts/package-windows")
            shutil.copy2(ROOT / "validation/release_pdb.py", root / "validation/release_pdb.py")
            (root / "validation/__init__.py").touch()
            (root / "plurx.example.toml").write_text("# own fixture\n")
            (root / "deploy/windows/README.md").write_text("own fixture\n")
            release = root / "build/x86_64-pc-windows-msvc/release"; release.mkdir(parents=True)
            pair(release)
            environment = {**os.environ, "CARGO_TARGET_DIR": str(root / "build")}
            command = [str(root / "scripts/package-windows")]
            subprocess.run(command, env=environment, check=True, capture_output=True)
            archive = root / "target/windows-package/plurxd-windows-x86_64.zip"
            with zipfile.ZipFile(archive) as zipped:
                names = set(zipped.namelist())
                self.assertIn("plurx-windows-x86_64/plurxd.exe", names)
                self.assertIn("plurx-windows-x86_64/plurxd.pdb", names)
            (release / "plurxd.pdb").unlink()
            result = subprocess.run(command, env=environment, capture_output=True)
            self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
