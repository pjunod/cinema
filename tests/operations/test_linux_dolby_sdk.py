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


    def test_actual_project_outputs_are_required_and_imported_libraries_cannot_be_replaced(self):
        module = helper()
        source = (self.tmp_path / "project").resolve()
        source.mkdir()
        sdk = self.tmp_path / "sdk"
        template = source / "foo.pc"
        template.write_text("prefix=@prefix@\nVersion: @version@\n")
        selected = {"root": "build", "path": "foo.pc"}
        with self.assertRaisesRegex(ValueError, "unconfigured"):
            module.publish_generated_output(sdk, "lib/pkgconfig/foo.pc", source, selected)
        # Represents the exact bytes produced by an actual project's configure
        # step; the publication owner neither synthesizes nor substitutes them.
        template.write_text("prefix=/private/sdk\nVersion: 1.0\nLibs: -lfoo\n")
        facts = module.publish_generated_output(sdk, "lib/pkgconfig/foo.pc", source, selected)
        assert facts["sha256"] == hashlib.sha256(template.read_bytes()).hexdigest()
        assert (sdk / "lib/pkgconfig/foo.pc").read_bytes() == template.read_bytes()
        with self.assertRaisesRegex(ValueError, "must not replace"):
            module.publish_generated_output(sdk, "lib/pkgconfig/foo.pc", source, selected)
        outside = self.tmp_path / "outside.h"
        outside.write_text("uncontrolled host header\n")
        (source / "escape.h").symlink_to(outside)
        with self.assertRaisesRegex(ValueError, "contained"):
            module.publish_generated_output(sdk, "include/foo.h", source, {"root": "source", "path": "escape.h"})

    def test_generator_contract_refuses_shell_wrappers_templates_and_missing_outputs(self):
        module = helper()
        generator = {"source_root": "source", "commands": [{"cwd": "build", "argv": ["cmake", "-DCMAKE_INSTALL_PREFIX={sdk}", "{source}"]}], "outputs": {"lib/pkgconfig/foo.pc": {"root": "build", "path": "foo.pc"}}}
        module.validate_generator(generator)
        generator["commands"][0]["argv"] = ["sh", "-c", "echo manufactured-header"]
        with self.assertRaisesRegex(ValueError, "without a shell"):
            module.validate_generator(generator)
        generator["commands"] = []
        generator["outputs"]["lib/pkgconfig/foo.pc"]["path"] = "foo.pc.in"
        with self.assertRaisesRegex(ValueError, "templates"):
            module.validate_generator(generator)
        generator["outputs"] = {}
        with self.assertRaisesRegex(ValueError, "explicit authentic"):
            module.validate_generator(generator)


    def test_only_rebuilt_ffmpeg_modules_are_excluded_from_official_external_imports(self):
        module = helper()
        for name in ["libavcodec.so.62", "libavfilter.so.11", "libavformat.so", "libavutil.so.60", "libswresample.so.6", "libswscale.so.9"]:
            assert module.ffmpeg_owned_library(name)
        for name in ["libavif.so.16", "libva.so.2", "libvpl.so.2", "dri/iHD_drv_video.so"]:
            assert not module.ffmpeg_owned_library(name)


    def test_development_package_cross_links_are_inventory_not_uncontrolled_contents(self):
        module = helper()
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w") as archive:
            link = tarfile.TarInfo("usr/lib/x86_64-linux-gnu/libx264.so")
            link.type = tarfile.SYMTYPE
            link.linkname = "libx264.so.164"
            archive.addfile(link)
        with self.assertRaisesRegex(ValueError, "unresolved"):
            module.tar_members(output.getvalue())
        regular, links = module.tar_members(output.getvalue(), allow_unresolved=True)
        assert not regular
        assert links == {"usr/lib/x86_64-linux-gnu/libx264.so": "usr/lib/x86_64-linux-gnu/libx264.so.164"}
        with self.assertRaises(KeyError):
            module.resolved(next(iter(links)), regular, links)
