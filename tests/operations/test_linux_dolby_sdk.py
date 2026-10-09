"""Source-staging contracts; declarations never qualify a generated SDK/runtime."""
import hashlib
import importlib.machinery
import importlib.util
import io
import json
import os
import shutil
import subprocess
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


    def test_distribution_multiarch_pc_projection_keeps_authentic_bytes_and_private_prefix(self):
        module = helper()
        sdk = self.tmp_path / "sdk"
        sysroot = sdk / "sysroot"
        source_name = "usr/lib/x86_64-linux-gnu/pkgconfig/opus.pc"
        # Synthetic contract fixture with the same installed /usr layout as
        # authenticated Debian metadata; this is not a source-offer claim.
        contents = b"prefix=/usr\nincludedir=${prefix}/include\nName: Opus\nDescription: test\nVersion: 1.3.1\nCflags: -I${includedir}/opus\n"
        original = sysroot / source_name
        original.parent.mkdir(parents=True)
        original.write_bytes(contents)
        consumed = {"sha256": module.digest(contents), "bytes": len(contents), "kind": "pkg-config"}
        records = {"libopus-dev": {"archive_sha256": "a" * 64, "consumed": {source_name: consumed}}}
        projected = module.project_distribution_pkgconfig(sysroot, {source_name: contents}, {source_name: ["libopus-dev"]}, records)
        destination = "usr/lib/pkgconfig/opus.pc"
        assert original.read_bytes() == (sysroot / destination).read_bytes() == contents
        inventory = {name: {"sha256": module.digest(contents), "bytes": len(contents)} for name in [source_name, destination]}
        facts = {"pkg_config_projections": projected, "sysroot_members": inventory, "roles": records}
        module.validate_distribution_pkgconfig_projections(facts)
        private_pc = sdk / "lib/pkgconfig/private.pc"
        private_pc.parent.mkdir(parents=True)
        private_pc.write_text("prefix=/original/private\nName: Private\nDescription: test\nVersion: 1\nCflags: -I${prefix}/include/private\n")
        tool = shutil.which("pkg-config")
        with self.subTest("actual pkg-config relocation"):
            if tool is None:
                self.skipTest("pkg-config unavailable; structural projection checks remain independent")
            environment = {"PATH": os.environ.get("PATH", ""), "PKG_CONFIG_LIBDIR": os.pathsep.join([str(private_pc.parent), str((sysroot / destination).parent)]), "PKG_CONFIG_PATH": ""}
            result = subprocess.run([tool, "--define-prefix", "--cflags", "opus", "private"], env=environment, capture_output=True, text=True, timeout=10, check=True)
            assert str(sysroot / "usr/include/opus") in result.stdout
            assert str(sdk / "include/private") in result.stdout
            assert str(sysroot / "usr/lib/include/opus") not in result.stdout
        facts["sysroot_members"][destination] = {"sha256": "0" * 64, "bytes": len(contents)}
        with self.assertRaisesRegex(ValueError, "authentic member bytes"):
            module.validate_distribution_pkgconfig_projections(facts)
        facts["sysroot_members"][destination] = dict(inventory[source_name])
        projected[destination]["source_roles"]["libopus-dev"] = "b" * 64
        with self.assertRaisesRegex(ValueError, "source association"):
            module.validate_distribution_pkgconfig_projections(facts)
        with self.assertRaisesRegex(ValueError, "must not replace"):
            module.project_distribution_pkgconfig(sysroot, {source_name: contents}, {source_name: ["libopus-dev"]}, records)

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


def test_normalized_source_tree_preserves_bytes_executability_and_link_targets():
    module = helper()
    def archive(prefix, execute=True, link="main.py"):
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w") as output:
            path = (prefix + "/" if prefix else "")
            row = tarfile.TarInfo(path + "main.py")
            row.mode = 0o755 if execute else 0o644
            contents = b"source"
            row.size = len(contents)
            output.addfile(row, io.BytesIO(contents))
            row = tarfile.TarInfo(path + "link")
            row.type = tarfile.SYMTYPE
            row.linkname = link
            row.mode = 0o777
            output.addfile(row)
        return buffer.getvalue()
    original = module.normalized_source_tree(archive(""), ".")
    assert original == module.normalized_source_tree(archive("provider-prefix"), "provider-prefix")
    assert original != module.normalized_source_tree(archive("", execute=False), ".")
    assert original != module.normalized_source_tree(archive("", link="other.py"), ".")


def test_submodule_composition_rejects_parent_gitlink_mismatch_and_existing_bytes():
    module = helper()
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        archive = root / "module.tar"
        with tarfile.open(archive, "w") as output:
            for name, contents in {"dlg/LICENSE": b"Original license", "dlg/src/dlg/dlg.c": b"original source"}.items():
                row = tarfile.TarInfo(name)
                row.size = len(contents)
                output.addfile(row, io.BytesIO(contents))
        tree = {"tree": [{"path": "subprojects/dlg", "mode": "160000", "type": "commit", "sha": "b" * 40}], "truncated": False}
        proof = root / "tree.json"
        proof.write_text(json.dumps(tree))
        child = {"path": "subprojects/dlg", "repository_url": "https://github.com/nyorain/dlg.git", "url": "https://codeload.github.com/nyorain/dlg/tar.gz/" + "b" * 40, "commit": "b" * 40, "archive": archive.name, "archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(), "source_root": "dlg", "license_members": ["dlg/LICENSE"]}
        row = {"commit": "a" * 40, "generator": {"source_root": "."}, "submodules": [child], "submodule_tree_evidence": {"archive": proof.name, "sha256": hashlib.sha256(proof.read_bytes()).hexdigest(), "parent_commit": "a" * 40, "url": "https://api.github.com/repos/freetype/freetype/git/trees/" + "a" * 40 + "?recursive=1"}}
        parent = {".gitmodules": b'[submodule "dlg"]\n path = subprojects/dlg\n url = https://github.com/nyorain/dlg.git\n'}
        offer = root / "offer"
        records = module.stage_submodules(row, parent, offer, root)
        project = root / "project"
        project.mkdir()
        module.compose_submodules(offer, project, root / "work", records)
        assert (project / "subprojects/dlg/src/dlg/dlg.c").read_bytes() == b"original source"
        assert not (project / ".git").exists()
        with unittest.TestCase().assertRaisesRegex(ValueError, "existing parent bytes"):
            module.compose_submodules(offer, project, root / "second-work", records)
        child["commit"] = "c" * 40
        with unittest.TestCase().assertRaisesRegex(ValueError, "parent gitlink"):
            module.stage_submodules(row, parent, root / "mismatch", root)


def test_cold_source_urls_are_downloadable_archives_and_required_submodules_are_pinned():
    root = Path(__file__).resolve().parents[2]
    lock = json.loads((root / "scripts/linux-video-ffmpeg-sdk-sources.json").read_text())
    for row in lock["sources"].values():
        assert not row["url"].endswith(".git")
        if row.get("historical_archive_equivalence"):
            assert row["source_tree_sha256"] == row["historical_archive_equivalence"]["normalized_tree_sha256"]
    assert {module["path"] for module in lock["sources"]["freetype"]["submodules"]} == {"subprojects/dlg"}
    placebo = lock["sources"]["libplacebo"]
    assert {module["path"] for module in placebo["submodules"]} == {"3rdparty/glad", "3rdparty/jinja", "3rdparty/markupsafe", "3rdparty/fast_float", "3rdparty/Vulkan-Headers"}
    assert all("nuklear" not in module["path"] for module in placebo["submodules"])


def test_isolated_cmake_install_cannot_redirect_into_sdk_or_host():
    module = helper()
    private = ["cmake", "--install", "{build}", "--prefix", "{build}/installed"]
    generator = {
        "source_root": ".", "commands": [{"cwd": "build", "argv": private}],
        "outputs": {"share/cmake/VulkanHeaders/VulkanHeadersConfig.cmake": {
            "root": "build", "path": "installed/share/cmake/VulkanHeaders/VulkanHeadersConfig.cmake"
        }}
    }
    module.validate_generator(generator)
    for prefix in ["{sdk}", "/usr", "/tmp/host-sdk", "{build}/../outside"]:
        redirected = {**generator, "commands": [{"cwd": "build", "argv": private[:-1] + [prefix]}]}
        with unittest.TestCase().assertRaises(ValueError):
            module.validate_generator(redirected)
    for argv in [["make", "install"], ["cmake", "--install", "{source}", "--prefix", "{build}/installed"]]:
        with unittest.TestCase().assertRaises(ValueError):
            module.validate_generator({**generator, "commands": [{"cwd": "build", "argv": argv}]})


def test_cmake_exports_reject_build_paths_and_existing_sdk_bytes():
    module = helper()
    name = "share/cmake/VulkanHeaders/VulkanHeadersConfig.cmake"
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory).resolve()
        build, sdk = root / "build", root / "sdk"
        origin = build / "installed" / name
        origin.parent.mkdir(parents=True)
        relative = b'set(_IMPORT_PREFIX "${CMAKE_CURRENT_LIST_DIR}/../../..")\n'
        origin.write_bytes(relative)
        selected = {"root": "build", "path": "installed/" + name}
        result = module.publish_generated_output(sdk, name, build, selected)
        assert (sdk / name).read_bytes() == relative
        assert result["sha256"] == hashlib.sha256(relative).hexdigest()
        origin.write_bytes(b"changed valid bytes\n")
        with unittest.TestCase().assertRaises(ValueError):
            module.publish_generated_output(sdk, name, build, selected)
        assert (sdk / name).read_bytes() == relative
        (sdk / name).unlink()
        origin.write_bytes(('set(VulkanHeaders_DIR "' + str(build) + '/installed")\n').encode())
        with unittest.TestCase().assertRaises(ValueError):
            module.publish_generated_output(sdk, name, build, selected)
        assert not (sdk / name).exists()


def test_bootstrap_package_rejects_changed_binary_wrong_abi_and_link_alias():
    import struct
    module = helper()
    binary = bytearray(64)
    binary[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<H", binary, 18, 62)
    binary = bytes(binary)
    row = {"version": "3.1-1", "architecture": "amd64", "executable_member": "usr/bin/gperf",
           "executable_sha256": hashlib.sha256(binary).hexdigest(),
           "copyright_member": "usr/share/doc/gperf/copyright"}
    regular = {"usr/bin/gperf": binary, "usr/share/doc/gperf/copyright": b"original license"}
    control = b"Package: gperf\nVersion: 3.1-1\nArchitecture: amd64\n"
    assert module.bootstrap_tool_components(regular, {}, control, row)["gperf"] == binary
    with unittest.TestCase().assertRaises(ValueError):
        module.bootstrap_tool_components({**regular, "usr/bin/gperf": binary + b"changed"}, {}, control, row)
    other = bytearray(binary)
    struct.pack_into("<H", other, 18, 183)
    wrong = bytes(other)
    with unittest.TestCase().assertRaises(ValueError):
        module.bootstrap_tool_components({**regular, "usr/bin/gperf": wrong}, {}, control,
                                         {**row, "executable_sha256": hashlib.sha256(wrong).hexdigest()})
    with unittest.TestCase().assertRaises(ValueError):
        module.bootstrap_tool_components(regular, {"usr/bin/gperf": "/usr/bin/host-gperf"}, control, row)
    with unittest.TestCase().assertRaises(ValueError):
        module.bootstrap_tool_components(regular, {}, control.replace(b"3.1-1", b"3.2-1"), row)


def test_private_bootstrap_fence_rejects_tool_license_and_source_offer_substitution():
    import struct
    module = helper()
    binary = bytearray(64)
    binary[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<H", binary, 18, 62)
    binary = bytes(binary)
    package = b"authenticated package fixture"
    control = b"Package: gperf\nVersion: 3.1-1\nArchitecture: amd64\n"
    regular = {"usr/bin/gperf": binary, "usr/share/doc/gperf/copyright": b"original license"}
    row = {"version": "3.1-1", "architecture": "amd64", "executable_member": "usr/bin/gperf",
           "executable_sha256": hashlib.sha256(binary).hexdigest(),
           "copyright_member": "usr/share/doc/gperf/copyright",
           "archive_sha256": hashlib.sha256(package).hexdigest(),
           "source_offer": "provenance/distribution-sources/gperf",
           "launcher": "provenance/build-tools/gperf/gperf"}
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory).resolve()
        base = root / "provenance/build-tools/gperf"
        base.mkdir(parents=True)
        offered = root / row["source_offer"] / "original-package.deb"
        offered.parent.mkdir(parents=True)
        offered.write_bytes(package)
        contents = {"gperf": binary, "copyright": b"original license", "control": control,
                    "original-package.deb": package}
        for name, data in contents.items():
            (base / name).write_bytes(data)
        (base / "gperf").chmod(0o555)
        row["files"] = {name: hashlib.sha256(data).hexdigest() for name, data in contents.items()}
        # Mock only archive decoding; byte/hash/fence checks still inspect real files.
        with patch.object(module, "package_members", return_value=((regular, {}), ({"control": control}, {}))):
            module.verify_build_tools(root, {"gperf": row})
            for name in ["gperf", "copyright"]:
                path = base / name
                path.chmod(0o644)
                path.write_bytes(contents[name] + b"changed")
                if name == "gperf":
                    path.chmod(0o555)
                with unittest.TestCase().assertRaises(ValueError):
                    module.verify_build_tools(root, {"gperf": row})
                path.chmod(0o644)
                path.write_bytes(contents[name])
                if name == "gperf":
                    path.chmod(0o555)
            offered.write_bytes(package + b"changed")
            with unittest.TestCase().assertRaises(ValueError):
                module.verify_build_tools(root, {"gperf": row})


def test_bootstrap_claims_normalize_staged_license_path_without_losing_source_identity():
    module = helper()
    declared = {"meson": {"version": "1.12.1", "url": "https://example.org/meson.tar.gz",
                          "archive_sha256": "a" * 64, "archive": "meson-1.12.1.tar.gz",
                          "consumed_by": "minimum supported upstream version",
                          "license_member": "meson-1.12.1/COPYING"},
                "gperf": {"version": "3.1-1", "executable_sha256": "b" * 64}}
    observed = {"meson": {"version": "1.12.1", "url": "https://example.org/meson.tar.gz",
                          "archive_sha256": "a" * 64,
                          "license_member": "source/meson-1.12.1/COPYING"},
                "gperf": dict(declared["gperf"])}
    module.validate_build_tool_claims(declared, observed)
    for key in ["version", "url", "archive_sha256", "license_member"]:
        tampered = {**observed, "meson": {**observed["meson"], key: "substituted"}}
        with unittest.TestCase().assertRaises(ValueError):
            module.validate_build_tool_claims(declared, tampered)
    with unittest.TestCase().assertRaises(ValueError):
        module.validate_build_tool_claims(declared, {"meson": observed["meson"]})
    with unittest.TestCase().assertRaises(ValueError):
        module.validate_build_tool_claims(declared, {**observed, "gperf": {**observed["gperf"], "executable_sha256": "c" * 64}})
