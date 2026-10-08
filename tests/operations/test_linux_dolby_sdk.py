"""Source-staging contracts; declarations never qualify a generated SDK/runtime."""
import hashlib
import importlib.machinery
import importlib.util
import io
import json
from pathlib import Path
import tarfile

import tempfile
import unittest
from unittest.mock import patch


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

    def test_absolute_debian_alias_resolves_only_authenticated_cross_role_bytes(self):
        module = helper()
        endpoint = "lib/x86_64-linux-gnu/libz.so.1.2.13"
        alias = "usr/lib/x86_64-linux-gnu/libz.so"
        original_target = "/" + endpoint
        contents = b"authenticated public runtime bytes"
        files, links = {endpoint: contents}, {alias: original_target}
        records = {
            "zlib-dev": {"archive_sha256": "a" * 64, "archive_aliases": links},
            "zlib-runtime": {"archive_sha256": "b" * 64, "consumed": {
                endpoint: {"sha256": hashlib.sha256(contents).hexdigest()}}}}
        sysroot = self.tmp_path / "namespace"
        missing, witness = module.materialize_distribution_aliases(sysroot, files, links, records,
            {endpoint: ["zlib-runtime"]}, {alias: ["zlib-dev"]})
        self.assertEqual(missing, {})
        self.assertEqual((sysroot / alias).read_bytes(), contents)
        self.assertFalse((sysroot / alias).is_symlink())
        self.assertEqual(witness[alias]["archive_target"], original_target)
        self.assertEqual(witness[alias]["source_roles"], {"zlib-dev": "a" * 64})
        self.assertEqual(witness[alias]["target_roles"], {"zlib-runtime": "b" * 64})
        with self.assertRaisesRegex(ValueError, "content changed"):
            module.materialize_distribution_aliases(self.tmp_path / "substitution",
                {endpoint: b"substituted target"}, links, records,
                {endpoint: ["zlib-runtime"]}, {alias: ["zlib-dev"]})
        missing, witness = module.materialize_distribution_aliases(self.tmp_path / "missing", {}, links,
            records, {}, {alias: ["zlib-dev"]})
        self.assertEqual(missing, links)
        self.assertEqual(witness, {})
        self.assertFalse((self.tmp_path / "missing" / alias).exists())
        with self.assertRaisesRegex(ValueError, "cycle"):
            module.materialize_distribution_aliases(self.tmp_path / "cycle", {},
                {alias: "/" + endpoint, endpoint: "/" + alias}, records, {}, {})

    def test_absolute_alias_inventory_keeps_default_refusal_and_rejects_escape(self):
        module = helper()
        for target in ["/lib/x86_64-linux-gnu/libz.so.1", "/../../outside"]:
            output = io.BytesIO()
            with tarfile.open(fileobj=output, mode="w") as archive:
                link = tarfile.TarInfo("usr/lib/x86_64-linux-gnu/libz.so")
                link.type = tarfile.SYMTYPE
                link.linkname = target
                archive.addfile(link)
            with self.assertRaisesRegex(ValueError, "external archive link"):
                module.tar_members(output.getvalue())
            if ".." in target:
                with self.assertRaisesRegex(ValueError, "external archive link"):
                    module.tar_members(output.getvalue(), allow_unresolved=True)
            else:
                regular, links = module.tar_members(output.getvalue(), allow_unresolved=True)
                self.assertEqual(regular, {})
                self.assertEqual(next(iter(links.values())), target)

    def test_private_meson_provider_binds_authentic_modules_and_isolated_launcher(self):
        module = helper()
        original = io.BytesIO()
        members = {"meson-1.12.1/setup.cfg": b"python_requires = >= 3.10\n",
                   "meson-1.12.1/COPYING": b"original upstream license\n",
                   "meson-1.12.1/mesonbuild/mesonmain.py": b"# authentic synthetic module vector\n"}
        with tarfile.open(fileobj=original, mode="w") as archive:
            for name, contents in members.items():
                entry = tarfile.TarInfo(name)
                entry.size = len(contents)
                archive.addfile(entry, io.BytesIO(contents))
        source = self.tmp_path / "meson-source.tar"
        source.write_bytes(original.getvalue())
        source_sha = hashlib.sha256(original.getvalue()).hexdigest()
        lock = {"build_tools": {"meson": {"archive": source.name, "archive_sha256": source_sha,
                                         "version": "1.12.1", "url": "https://example.org/source"}}}
        root = self.tmp_path / "staged"
        provenance = root / "provenance"
        provenance.mkdir(parents=True)
        with patch.object(module, "MESON_SOURCE_SHA", source_sha):
            tools = module.stage_build_tools(lock, provenance, self.tmp_path)
            module.verify_build_tools(root, tools)
            self.assertIn("-I -S -B", (root / tools["meson"]["launcher"]).read_text())
            path = provenance / "build-tools/meson/source/meson-1.12.1/mesonbuild/mesonmain.py"
            path.chmod(0o644)
            path.write_bytes(b"substituted host module")
            with self.assertRaisesRegex(ValueError, "tree changed"):
                module.verify_build_tools(root, tools)
            # Rehashing a substituted module and the declared tree cannot
            # replace the independently pinned original source archive.
            tools["meson"]["files"][str(path.relative_to(provenance / "build-tools/meson"))] = module.digest(path.read_bytes())
            tools["meson"]["tree_sha256"] = module.digest(json.dumps(tools["meson"]["files"],
                sort_keys=True, separators=(",", ":")).encode())
            with self.assertRaisesRegex(ValueError, "authentic source"):
                module.verify_build_tools(root, tools)


def test_debian_source_formats_require_descriptor_bound_archive_and_patch_identities():
    module = helper()
    def offer(format, patch_name):
        objects = {"libpciaccess_0.17.orig.tar.gz": b"original", patch_name: b"patch"}
        descriptor = "Format: " + format + "\nSource: libpciaccess\nVersion: 0.17-2\nChecksums-Sha256:\n"
        descriptor += "".join(" " + hashlib.sha256(data).hexdigest() + " " + str(len(data)) + " " + name + "\n" for name, data in objects.items())
        objects["libpciaccess_0.17-2.dsc"] = descriptor.encode()
        return objects
    facts = {"package": "libpciaccess", "version": "0.17-2"}
    valid = offer("1.0", "libpciaccess_0.17-2.diff.gz")
    assert module.validate_debian_source_offer(facts, valid) == "1.0"
    quilt = offer("3.0 (quilt)", "libpciaccess_0.17-2.debian.tar.xz")
    assert module.validate_debian_source_offer(facts, quilt) == "3.0 (quilt)"
    for bad in [offer("3.0 (quilt)", "libpciaccess_0.17-2.diff.gz"), offer("1.0", "arbitrary.gz"), dict(valid, **{"libpciaccess_0.17-2.diff.gz": b"substitution"})]:
        with unittest.TestCase().assertRaises(ValueError):
            module.validate_debian_source_offer(facts, bad)
    missing = dict(valid)
    del missing["libpciaccess_0.17-2.diff.gz"]
    with unittest.TestCase().assertRaises(ValueError):
        module.validate_debian_source_offer(facts, missing)


def test_fftw_float_pkgconfig_is_generated_by_its_upstream_make_target():
    root = Path(__file__).resolve().parents[2]
    lock = json.loads((root / "scripts/linux-video-ffmpeg-sdk-sources.json").read_text())
    generator = lock["sources"]["fftw3"]["generator"]
    assert generator["commands"][0]["argv"][0] == "./configure"
    assert "--enable-single" in generator["commands"][0]["argv"]
    assert generator["commands"][1] == {"cwd": "source", "argv": ["make", "-j{jobs}", "fftw3f.pc"]}
    assert generator["outputs"]["lib/pkgconfig/fftw3f.pc"] == {"root": "source", "path": "fftw3f.pc"}
    assert "--enable-threads" in generator["commands"][0]["argv"]
