"""Focused synthetic checks for pinned toolchain safety and offline resolution."""

import hashlib
import importlib.machinery
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[2] / "scripts/bootstrap-formal-toolchain"
ROOT = Path(__file__).resolve().parents[2]
TEST_SCRATCH = ROOT / "target/formal-tests"
LOADER = importlib.machinery.SourceFileLoader("bootstrap_formal_toolchain", str(SCRIPT))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
bootstrap = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(bootstrap)


class FormalToolchainTest(unittest.TestCase):
    def temporary_directory(self):
        TEST_SCRATCH.mkdir(parents=True, exist_ok=True)
        return tempfile.TemporaryDirectory(dir=TEST_SCRATCH)

    def test_platform_selection_rejects_unpinned_architectures(self):
        with mock.patch.object(bootstrap.platform, "system", return_value="Darwin"), mock.patch.object(
            bootstrap.platform, "machine", return_value="x86_64"
        ):
            with self.assertRaisesRegex(bootstrap.ToolchainError, "supported platforms"):
                bootstrap._current_platform()

    def test_tar_extraction_rejects_traversal_and_links(self):
        with self.temporary_directory() as directory:
            root = Path(directory)
            archive = root / "unsafe.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                member = tarfile.TarInfo("../escape")
                member.size = 1
                tar.addfile(member, io.BytesIO(b"x"))
            with self.assertRaisesRegex(bootstrap.ToolchainError, "unsafe archive member"):
                bootstrap._safe_extract_tar(archive, root / "extract")
            self.assertFalse((root / "escape").exists())
            internal = root / "internal.tar.gz"
            with tarfile.open(internal, "w:gz") as tar:
                payload = b"pinned file"
                regular = tarfile.TarInfo("jdk/bin/java")
                regular.mode = 0o755
                regular.size = len(payload)
                tar.addfile(regular, io.BytesIO(payload))
                alias = tarfile.TarInfo("jdk/bin/java-alias")
                alias.type = tarfile.SYMTYPE
                alias.linkname = "java"
                tar.addfile(alias)
            bootstrap._safe_extract_tar(internal, root / "extract-internal")
            self.assertEqual((root / "extract-internal/jdk/bin/java-alias").read_bytes(), payload)
            linked = root / "link.tar.gz"
            with tarfile.open(linked, "w:gz") as tar:
                member = tarfile.TarInfo("safe/link")
                member.type = tarfile.SYMTYPE
                member.linkname = "../../escape"
                tar.addfile(member)
            with self.assertRaisesRegex(bootstrap.ToolchainError, "archive link escapes extraction root"):
                bootstrap._safe_extract_tar(linked, root / "extract-link")

    def test_install_receipt_is_bound_to_verified_archive_java_bytes(self):
        with self.temporary_directory() as directory:
            root = Path(directory)
            archive = root / "jdk.tar.gz"
            java_bytes = (
                b"#!/bin/sh\n"
                b"echo 'openjdk version \"21.0.12\" LTS' >&2\n"
                b"echo 'OpenJDK Runtime Environment Temurin-21.0.12+8 (build 21.0.12+8-LTS)' >&2\n"
            )
            with tarfile.open(archive, "w:gz") as tar:
                member = tarfile.TarInfo("jdk-21/bin/java")
                member.mode = 0o755
                member.size = len(java_bytes)
                tar.addfile(member, io.BytesIO(java_bytes))
            jdk = {
                "version": "21.0.12+8",
                "sha256": bootstrap._sha256_file(archive),
            }
            runtime = bootstrap._install_jdk(archive, root, jdk, "linux", "x64")
            java, identity, digest = bootstrap._verify_runtime(
                runtime, archive, jdk, "linux", "x64"
            )
            self.assertEqual(digest, hashlib.sha256(java_bytes).hexdigest())
            self.assertIn("21.0.12+8", identity)
            java.write_bytes(java_bytes + b"# tampered\n")
            with self.assertRaisesRegex(bootstrap.ToolchainError, "differs from"):
                bootstrap._verify_runtime(runtime, archive, jdk, "linux", "x64")

    def test_offline_resolve_fails_when_pinned_tlc_is_missing(self):
        manifest = {
            "tlc": {
                "version": "1.7.4",
                "filename": "tla2tools-1.7.4.jar",
                "url": "https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar",
                "sha256": "0" * 64,
            },
            "jdks": [
                {
                    "os": "macos",
                    "arch": "arm64",
                    "version": "21.0.12+8",
                    "filename": "jdk.tar.gz",
                    "url": "https://github.com/adoptium/temurin21-binaries/releases/download/jdk.tar.gz",
                    "sha256": "1" * 64,
                }
            ],
        }
        with self.temporary_directory() as directory, mock.patch.object(
            bootstrap, "_current_platform", return_value=("macos", "arm64")
        ):
            with self.assertRaisesRegex(bootstrap.ToolchainError, "TLC jar is missing"):
                bootstrap._resolve(Path("manifest.json"), Path(directory), manifest, "2" * 64)


if __name__ == "__main__":
    unittest.main()
