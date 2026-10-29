"""Platform coverage and source-package regressions without host GPU access."""
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]


def load(name):
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), str(ROOT / "scripts" / name))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


class MediaSupportMatrixTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which("autoconf") and shutil.which("make"),
                         "requires autoconf and make")
    def test_cold_generation_tools_build_through_symlinked_temporary_directory(self):
        media = load("build-macos-source-media")
        with tempfile.TemporaryDirectory() as work:
            root = Path(work).resolve()
            fixture = root / "fixture"
            fixture.mkdir()
            # Exercise Automake's real Autoconf link handling, without downloading
            # upstream archives or building the full media dependency tree.
            (fixture / "configure.ac").write_text(
                "AC_INIT([generation-tool-fixture], [1])\n"
                "AC_CONFIG_SRCDIR([GNUmakefile])\n"
                "AC_CONFIG_LINKS([GNUmakefile:GNUmakefile])\n"
                "AC_OUTPUT\n")
            makefile = "all:\n\t@touch built\ninstall: all\n\t@touch installed\n"
            (fixture / "GNUmakefile").write_text(makefile)
            subprocess.run(["autoconf"], cwd=fixture, check=True, capture_output=True)
            physical = root / "physical"
            physical.mkdir()
            alias = root / "alias"
            alias.symlink_to(physical, target_is_directory=True)
            prepared = alias / "prepared"
            prepared.mkdir()

            def extract_fixture(archive, destination):
                destination.mkdir()
                for name in ("configure", "GNUmakefile"):
                    shutil.copyfile(fixture / name, destination / name)

            helper = mock.Mock()
            helper.extract.side_effect = extract_fixture
            with mock.patch.object(media, "recipe", return_value=helper):
                media.source_generation_tools(prepared, dict(os.environ))

            sources = list((prepared / "source-generation-tools").glob("*/installed"))
            self.assertEqual(len(sources), 2, "both generation tools must install")
            for installed in sources:
                source = installed.parent
                self.assertTrue((source / "built").is_file())
                self.assertFalse((source / "GNUmakefile").is_symlink())
                self.assertEqual((source / "GNUmakefile").read_text(), makefile)

    @unittest.skipUnless(shutil.which("aclocal") and shutil.which("autoconf"),
                         "requires aclocal and autoconf")
    def test_source_aclocal_discovers_homebrew_and_inherited_macro_directories(self):
        media = load("build-macos-source-media")
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            brew = root / "brew"
            brew_macros = brew / "share/aclocal"
            inherited = root / "caller-aclocal"
            empty_system = root / "source-prefix-aclocal"
            project = root / "project"
            prepared = root / "prepared"
            for directory in [brew_macros, inherited, empty_system, project, prepared]:
                directory.mkdir(parents=True)
            for directory, name in [(brew_macros, "BREW"), (inherited, "CALLER")]:
                (directory / (name.lower() + ".m4")).write_text(
                    "AC_DEFUN([AC_PLURX_" + name + "_MACRO], "
                    "[AC_DEFINE([PLURX_" + name + "_VISIBLE], [1], [macro found])])\n")
            (project / "configure.ac").write_text(
                "AC_INIT([aclocal-path-fixture], [1])\n"
                "AC_PLURX_BREW_MACRO\nAC_PLURX_CALLER_MACRO\nAC_OUTPUT\n")
            env = dict(os.environ, ACLOCAL_PATH=str(inherited))
            with mock.patch.object(media, "BREW_PREFIXES", [brew, root / "absent-brew"]), \
                    mock.patch.object(media, "recipe", return_value=mock.Mock()), \
                    mock.patch.object(media, "run"):
                media.source_generation_tools(prepared, env)
            # Reproduce source-built aclocal's empty third-party system prefix:
            # only the environment can expose the two external macro owners.
            subprocess.run([shutil.which("aclocal"), "--system-acdir=" + str(empty_system)],
                           cwd=project, env=env, check=True, capture_output=True)
            subprocess.run([shutil.which("autoconf")], cwd=project, env=env,
                           check=True, capture_output=True)
            subprocess.run(["/bin/sh", "./configure"], cwd=project, env=env,
                           check=True, capture_output=True)
            configured = (project / "config.log").read_text()
            self.assertIn("#define PLURX_BREW_VISIBLE 1", configured)
            self.assertIn("#define PLURX_CALLER_VISIBLE 1", configured)

    def test_cold_build_discovers_homebrew_and_new_tools_with_system_only_path(self):
        media = load("build-macos-source-media")
        with tempfile.TemporaryDirectory() as work:
            prefix = Path(work) / "homebrew"
            (prefix / "bin").mkdir(parents=True)
            executable = prefix / "bin/fixture-build-tool"
            brew = prefix / "bin/brew"
            brew.write_text("#!/bin/sh\n[ \"$1\" = install ] && [ \"$2\" = fixtureformula ] || exit 2\n"
                            + f"printf '#!/bin/sh\\nexit 0\\n' > '{executable}'\nchmod 755 '{executable}'\n")
            brew.chmod(0o755)
            with mock.patch.dict(os.environ, {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin"}, clear=True), \
                    mock.patch.object(media, "BREW_PREFIXES", [prefix]), \
                    mock.patch.object(media, "TOOLS", {"fixture-build-tool": "fixtureformula"}):
                env = media.build_tool_environment()
            self.assertEqual(shutil.which("brew", path=env["PATH"]), str(brew))
            self.assertEqual(shutil.which("fixture-build-tool", path=env["PATH"]), str(executable))
            self.assertIn(str(prefix / "sbin"), env["PATH"].split(os.pathsep))

    def test_matrix_binds_current_jellyfin_source_and_every_plurx_patch(self):
        matrix = json.loads((ROOT / "scripts/ffmpeg-support-matrix.json").read_text())
        media = load("build-macos-video-ffmpeg")
        self.assertEqual(matrix["source_commit"], media.COMMIT)
        self.assertEqual(matrix["source_sha256"], media.SOURCE_SHA256)
        self.assertEqual(matrix["macos_patches"], {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                         for path in (ROOT / "scripts/macos-video-ffmpeg-patches").glob("*.patch")})
        rows = {row["id"]: row for row in matrix["rows"]}
        self.assertEqual(rows["metal"]["profile"], "macos")
        self.assertIn("reference_white", rows["metal"]["options"]["tonemap_videotoolbox"])
        self.assertEqual(rows["qsv"]["profile"], "intel")
        self.assertEqual(rows["vaapi"]["profile"], "vaapi")
        self.assertEqual(rows["nvenc"]["profile"], "nvidia")
        self.assertEqual(rows["cuda"]["profile"], "nvidia")


class SourceMediaPackageTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.package = self.root / "media"
        self.package.mkdir()
        self.destination = self.root / "runtime"
        self.destination.mkdir()
        self.runtime = load("build-macos-source-runtime")
        self.media = load("build-macos-video-ffmpeg")
        self.manifest = {"mode": "compiled-pinned-jellyfin", "binaries": {}, "compiled_source": {"local_patches": {
            path.name: self.runtime.digest(path) for path in (ROOT / "scripts/macos-video-ffmpeg-patches").glob("*.patch")}}}
        for name in ["ffmpeg", "ffprobe", "fc-list", "fc-conflist", "plurx-source-parser.wasm", "fontconfig/fonts.conf", "provenance/source.txt"]:
            path = self.package / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(name.encode())
            path.chmod(0o755 if name in ["ffmpeg", "ffprobe", "fc-list", "fc-conflist"] else 0o644)
        self.manifest["binaries"] = {name: {"architecture": "arm64"} for name in ["ffmpeg", "ffprobe"]}
        self.write_manifest()
        self.filters = set(self.media.REQUIRED["filters"]) | {"libplacebo", "tonemap_opencl", "subtitles_vt_images", "overlay_videotoolbox", "bwdif_videotoolbox"}
        self.help = {
            "filter=tonemapx": " require_dovi <boolean> ...\n",
            "filter=tonemap_videotoolbox": "Filter tonemap_videotoolbox\n require_dovi <boolean> ...\n reference_white <double> ... (from 1 to 10000) (default 203)\n",
            "filter=subtitles_vt_images": "Filter subtitles_vt_images\n font_provider <string> ...\n fontconfig 1\n",
            "filter=subtitles": "Filter subtitles\n font_provider <string> ...\n fontconfig 1\n",
            "filter=overlay_videotoolbox": "Filter overlay_videotoolbox\n ass <string> ...\n bitmap <string> ...\n",
            "decoder=hevc": " -strict_dovi <boolean> ...\n",
        }
        native = mock.Mock()
        patch = mock.patch.object(self.runtime, "load_recipe", side_effect=lambda name: self.media if name == "build-macos-video-ffmpeg" else native)
        patch.start(); self.addCleanup(patch.stop)
        patch = mock.patch.object(self.media, "validate_font_tools")
        patch.start(); self.addCleanup(patch.stop)
        patch = mock.patch.object(self.runtime, "output", side_effect=self.inventory)
        patch.start(); self.addCleanup(patch.stop)
        patch = mock.patch.object(self.media, "run", side_effect=lambda args: self.help[args[-1]])
        patch.start(); self.addCleanup(patch.stop)

    def write_manifest(self):
        (self.package / "provenance/manifest.json").write_text(json.dumps(self.manifest))

    def inventory(self, args, **kwargs):
        if args[-1] == "-filters":
            return "\n".join(" .. " + name + " V->V" for name in self.filters)
        if args[-1] == "-hwaccels":
            return "Hardware acceleration methods:\nvideotoolbox\nvulkan\n"
        return "require_hardware"

    def test_stock_jellyfin_archive_is_rejected_before_staging(self):
        self.manifest["mode"] = "official-portable-package"
        self.write_manifest()
        with self.assertRaisesRegex(ValueError, "patched Jellyfin"):
            self.runtime.stage_media_package(self.package, self.destination, "arm64")
        self.assertEqual(list(self.destination.iterdir()), [])

    def test_package_missing_current_patch_is_rejected(self):
        self.manifest["compiled_source"]["local_patches"].pop("0005-match-metal-hable-reference-white.patch")
        self.write_manifest()
        with self.assertRaisesRegex(ValueError, "current Plurx FFmpeg patches"):
            self.runtime.validate_media_package(self.package, "arm64")

    def test_filter_declaration_without_reference_white_is_rejected(self):
        self.help["filter=tonemap_videotoolbox"] = "Filter tonemap_videotoolbox\n require_dovi <boolean> ...\n"
        with self.assertRaisesRegex(ValueError, "reference_white"):
            self.runtime.validate_media_package(self.package, "arm64")

    def test_missing_libplacebo_is_rejected_before_staging(self):
        self.filters.remove("libplacebo")
        with self.assertRaisesRegex(ValueError, "libplacebo"):
            self.runtime.stage_media_package(self.package, self.destination, "arm64")
        self.assertEqual(list(self.destination.iterdir()), [])

    def test_verified_media_staging_keeps_fonts_parser_and_source_provenance(self):
        (self.package / "plurxd").write_text("old daemon must not be copied")
        self.runtime.stage_media_package(self.package, self.destination, "arm64")
        for name in ["ffmpeg", "ffprobe", "fc-list", "fc-conflist", "fontconfig/fonts.conf", "provenance/source.txt", "provenance/manifest.json", "plurx-source-parser.wasm"]:
            self.assertEqual((self.destination / name).read_bytes(), (self.package / name).read_bytes())
        self.assertFalse((self.destination / "plurxd").exists())


class ColdMediaSourceClosureTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.prepared = self.root / "prepared"
        self.prepared.mkdir()
        (self.prepared / "prepared-source.json").write_text('{}')
        self.package = self.root / "package"
        (self.package / "provenance").mkdir(parents=True)
        self.manifest = {"compiled_source": {"local_patches": {"retained.patch": "digest"},
                         "dependency_sources": {"observed": {"commit": "actual"}}}}
        self.manifest_path = self.package / "provenance/manifest.json"
        self.manifest_path.write_text(json.dumps(self.manifest))
        (self.package / "plurx-source-parser.wasm").write_bytes(b"bound-parser")
        self.media = load("build-macos-source-media")
        self.offers, self.runtime = mock.Mock(), mock.Mock()
        self.offers.retain_compiled_snapshot.return_value = {
            "compiled_source_sha256": "snapshot", "compiled_source_index_sha256": "index"}
        patch = mock.patch.object(self.media, "recipe", side_effect=lambda name:
                                  self.offers if name == "macos-video-dependency-offers.py" else self.runtime)
        patch.start(); self.addCleanup(patch.stop)

    def test_cold_git_svn_build_retains_complete_source_before_package_validation(self):
        self.media.finalize_package(self.prepared, self.package)
        self.offers.retain_compiled_snapshot.assert_called_once_with(
            self.prepared, self.package / "provenance", self.prepared / "source-generation-tools")
        result = json.loads(self.manifest_path.read_text())["compiled_source"]
        self.assertEqual(result["mode"], "git-svn-recipe-build")
        self.assertEqual(result["compiled_source_sha256"], "snapshot")
        self.assertEqual(result["compiled_source_index_sha256"], "index")
        self.assertEqual(result["dependency_sources"], self.manifest["compiled_source"]["dependency_sources"])
        self.assertEqual((self.package / "plurx-source-parser.wasm").read_bytes(), b"bound-parser")
        self.assertFalse((self.prepared / "dependency-source-consumption.jsonl").exists())
        self.runtime.validate_media_package.assert_called_once()

    def test_cold_source_closure_failure_prevents_validation_and_publication(self):
        self.offers.retain_compiled_snapshot.side_effect = ValueError("unverified support source")
        original = self.manifest_path.read_bytes()
        with self.assertRaisesRegex(ValueError, "unverified support"):
            self.media.finalize_package(self.prepared, self.package)
        self.runtime.validate_media_package.assert_not_called()
        self.assertEqual(self.manifest_path.read_bytes(), original)

    def test_archive_recipe_keeps_its_actual_existing_consumption_lineage(self):
        (self.prepared / "prepared-source.json").write_text(json.dumps({"dependency_source_inputs": {}}))
        original = self.manifest_path.read_bytes()
        self.media.finalize_package(self.prepared, self.package)
        self.offers.retain_compiled_snapshot.assert_not_called()
        self.assertEqual(self.manifest_path.read_bytes(), original)
        self.runtime.validate_media_package.assert_called_once()


class StaticVulkanSourceTests(unittest.TestCase):
    def test_source_lock_includes_moltenvk_binary_source_and_static_shader_dependencies(self):
        lock = json.loads((ROOT / "scripts/macos-vulkan-sources.json").read_text())
        roles = {source["role"] for source in lock["sources"]}
        self.assertTrue({"moltenvk-binary", "moltenvk-source", "vulkan-headers", "shaderc", "glslang", "spirv-tools", "spirv-headers", "libplacebo"}.issubset(roles))
        for source in lock["sources"]:
            self.assertRegex(source["sha256"], r"^[0-9a-f]{64}$")
            self.assertTrue(source["url"].startswith("https://"))

    def test_vulkan_source_extraction_rejects_traversal_and_changed_archives(self):
        import io
        import tarfile
        helper = load("build-macos-vulkan")
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            archive = root / "unsafe.tar.gz"
            with tarfile.open(archive, "w:gz") as output:
                member = tarfile.TarInfo("source/../../escape")
                member.size = 1
                output.addfile(member, io.BytesIO(b"x"))
            with self.assertRaisesRegex(ValueError, "unsafe"):
                helper.extract(archive, root / "source")
            self.assertFalse((root.parent / "escape").exists())
            with self.assertRaisesRegex(ValueError, "checksum"):
                helper.acquire({"sha256": "0" * 64, "role": "fixture"}, archive)
