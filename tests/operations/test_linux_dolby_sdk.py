"""Source-staging contracts; declarations never qualify a generated SDK/runtime."""
import hashlib
import importlib.machinery
import importlib.util
import io
from pathlib import Path
import tarfile

import tempfile
import unittest


def helper():
    path = Path(__file__).resolve().parents[2] / "scripts/prepare-linux-dolby-sdk"
    loader = importlib.machinery.SourceFileLoader("linux_dolby_sdk", str(path))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def source_lock(tmp_path):
    members = {"sdk/COPYING": b"Original license\n", "sdk/foo.pc.in": b"prefix=@prefix@\nVersion: @version@\n"}
    archive = tmp_path / "original.tar"
    with tarfile.open(archive, "w") as output:
        for name, contents in members.items():
            row = tarfile.TarInfo(name)
            row.size = len(contents)
            output.addfile(row, io.BytesIO(contents))
    recipe = b"git clone -b v1.0 https://example.org/public/foo.git\n"
    lock = {"schema_version": 1, "sources": {"foo": {
        "kind": "git", "url": "https://example.org/public/foo.tar.gz",
        "ref": "v1.0", "commit": "a" * 40,
        "archive": str(archive), "archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
        "license_members": ["sdk/COPYING"], "configure_flags": ["--enable-libfoo"],
        "recipe_binding": {"fragment": recipe.decode(), "sha256": hashlib.sha256(recipe).hexdigest()},
        "files": {"lib/pkgconfig/foo.pc": {"member": "sdk/foo.pc.in", "sha256": hashlib.sha256(members["sdk/foo.pc.in"]).hexdigest()}},
    }}}
    return lock, recipe, members


class LinuxDolbySdkCase(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.tmp_path = Path(self.directory.name)

    def test_immutable_inputs_do_not_claim_generated_sdk(self):
        tmp_path = self.tmp_path
        module = helper()
        lock, recipe, members = source_lock(tmp_path)
        facts = module.stage_sources(lock, tmp_path / "sdk", tmp_path / "provenance", ["--enable-libfoo"], recipe)
        role = facts["foo"]
        assert role["generation"] == {"recipe_fragment_sha256": hashlib.sha256(recipe).hexdigest(), "generated_outputs": {}, "link_ready": False}
        assert not (tmp_path / "sdk/lib/pkgconfig/foo.pc").exists()
        assert (tmp_path / "provenance/consumed-sources/foo/pending-inputs/lib/pkgconfig/foo.pc").read_bytes() == members["sdk/foo.pc.in"]


    def test_tampered_source_and_unbound_recipe_are_refused(self):
        tmp_path = self.tmp_path
        module = helper()
        lock, recipe, _ = source_lock(tmp_path)
        lock["sources"]["foo"]["archive_sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "checksum"):
            module.stage_sources(lock, tmp_path / "sdk", tmp_path / "provenance", ["--enable-libfoo"], recipe)
        lock, recipe, _ = source_lock(tmp_path)
        with self.assertRaisesRegex(ValueError, "upstream recipe"):
            module.stage_sources(lock, tmp_path / "sdk", tmp_path / "other-provenance", ["--enable-libfoo"], b"unrelated recipe")


    def test_missing_configure_role_and_credential_url_are_refused(self):
        tmp_path = self.tmp_path
        module = helper()
        lock, recipe, _ = source_lock(tmp_path)
        with self.assertRaisesRegex(ValueError, "missing consumed SDK roles"):
            module.stage_sources(lock, tmp_path / "sdk", tmp_path / "provenance", ["--enable-libbar"], recipe)
        lock["sources"]["foo"]["url"] = "https://user:password@example.org/source.tar.gz"
        with self.assertRaisesRegex(ValueError, "credential-free"):
            module.stage_sources(lock, tmp_path / "sdk", tmp_path / "other-provenance", ["--enable-libfoo"], recipe)
