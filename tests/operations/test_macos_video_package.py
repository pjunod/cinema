"""Package provenance integrity fences; no executable or hardware is launched."""
import hashlib
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import tempfile
import struct
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("mac_video_package", str(ROOT / "scripts/build-macos-video-ffmpeg"))
spec = importlib.util.spec_from_loader(loader.name, loader)
TOOL = importlib.util.module_from_spec(spec)
loader.exec_module(TOOL)


class MacosVideoPackageCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.package = self.root / "parser"
        (self.package / "bin").mkdir(parents=True)
        (self.package / "provenance").mkdir()
        self.stage = self.root / "stage"
        self.stage.mkdir()
        self.evidence = self.stage / "provenance"
        self.evidence.mkdir()
        self.artifact = self.package / "bin/plurx-source-parser.wasm"
        self.artifact.write_bytes(b"\0asm\1\0\0\0")
        self.manifest = {"source_sha256": TOOL.SOURCE_SHA256, "source_commit": TOOL.COMMIT,
                         "parser_abi": "plurx-source-wasi-p1-v1-wasmtime-50.0.0",
                         "module_sha256": hashlib.sha256(self.artifact.read_bytes()).hexdigest(), "module_bytes": 8}
        self.write_manifest()

    def write_manifest(self):
        (self.package / "provenance/manifest.json").write_text(json.dumps(self.manifest))

    def test_parser_companion_is_hash_bound_to_native_source_and_retained_provenance(self):
        facts = TOOL.attach_source_parser(self.package, self.stage, self.evidence)
        self.assertEqual((self.stage / "plurx-source-parser.wasm").read_bytes(), self.artifact.read_bytes())
        self.assertEqual(facts["sha256"], self.manifest["module_sha256"])
        self.assertIn("manifest.json", facts["provenance_sha256"])
        self.assertEqual(facts["qualification"], "package-integrity-only")

    def test_parser_from_other_source_or_abi_is_rejected(self):
        for key in ("source_sha256", "source_commit", "parser_abi"):
            original = self.manifest[key]
            self.manifest[key] = "wrong"
            self.write_manifest()
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "source/ABI"):
                TOOL.attach_source_parser(self.package, self.stage, self.evidence)
            self.manifest[key] = original

    def test_corrupted_parser_companion_is_rejected_before_publication(self):
        self.artifact.write_bytes(b"\0asm\1\0\0\1")
        with self.assertRaisesRegex(ValueError, "does not match"):
            TOOL.attach_source_parser(self.package, self.stage, self.evidence)
        self.assertFalse((self.stage / "plurx-source-parser.wasm").exists())

    def test_parser_provenance_cannot_include_symlinks_or_git_credentials(self):
        unsafe = self.package / "provenance/outside"
        unsafe.symlink_to(self.artifact)
        with self.assertRaisesRegex(ValueError, "unsafe member"):
            TOOL.attach_source_parser(self.package, self.stage, self.evidence)
        unsafe.unlink()
        git = self.package / "provenance/.git"
        git.mkdir()
        (git / "config").write_text("not a source artifact")
        with self.assertRaisesRegex(ValueError, "Git checkout"):
            TOOL.attach_source_parser(self.package, self.stage, self.evidence)

    def test_sealed_parser_rejects_build_without_compiled_zlib(self):
        loader = importlib.machinery.SourceFileLoader("mac_parser_build", str(ROOT / "scripts/build-macos-source-parser"))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        builder = importlib.util.module_from_spec(spec)
        loader.exec_module(builder)
        config = self.root / "config.h"
        for text in ["#define CONFIG_ZLIB 0\n", "", "/* #define CONFIG_ZLIB 1 */\n", "#define CONFIG_ZLIB 10\n"]:
            config.write_text(text)
            with self.subTest(text=text), self.assertRaisesRegex(ValueError, "requires pinned zlib"):
                builder.require_zlib(config)
        config.write_text("#define CONFIG_ZLIB 1\n")
        builder.require_zlib(config)

    def test_sealed_parser_refuses_substituted_zlib_before_compilation(self):
        loader = importlib.machinery.SourceFileLoader("mac_parser_zlib", str(ROOT / "scripts/build-macos-source-parser"))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        builder = importlib.util.module_from_spec(spec)
        loader.exec_module(builder)
        archive = self.root / "substituted.tar.gz"
        archive.write_bytes(b"unauthenticated source")
        with patch.object(builder.subprocess, "run") as compiler:
            with self.assertRaises(ValueError):
                builder.build_zlib(self.root, self.evidence, {}, archive, 2)
            compiler.assert_not_called()

    def test_parser_companion_retains_nested_zlib_source_and_license(self):
        zlib = self.package / "provenance/zlib"
        zlib.mkdir()
        (zlib / "source.tar.gz").write_bytes(b"authenticated pinned source")
        (zlib / "LICENSE").write_bytes(b"retained Zlib license")
        facts = TOOL.attach_source_parser(self.package, self.stage, self.evidence)
        for name in ["source.tar.gz", "LICENSE"]:
            self.assertIn("zlib/" + name, facts["provenance_sha256"])
            retained = self.evidence / "source-parser/zlib" / name
            self.assertEqual(retained.read_bytes(), (zlib / name).read_bytes())

    def test_prepared_hable_white_is_source_bound_and_checks_bounds_and_default(self):
        with patch.object(TOOL, "run") as query:
            self.assertFalse(TOOL.verify_prepared_hable_white(Path("ffmpeg"), self.evidence, {}))
            query.assert_not_called()
        patches = {"0005-match-metal-hable-reference-white.patch": "source-bound"}
        valid = "Filter tonemap_videotoolbox\n   reference_white <double> ..FV....... Reference white in nits for Hable (from 1 to 10000) (default 203)\n"
        with patch.object(TOOL, "run", return_value=valid):
            self.assertTrue(TOOL.verify_prepared_hable_white(Path("ffmpeg"), self.evidence, patches))
        for text in ["Filter tonemap_videotoolbox\n", valid.replace("default 203", "default 100"), valid.replace("from 1 to 10000", "from 0 to 10000"), valid.replace("Filter tonemap_videotoolbox", "Filter unrelated")]:
            with self.subTest(text=text), patch.object(TOOL, "run", return_value=text), self.assertRaisesRegex(ValueError, "Hable reference_white"):
                TOOL.verify_prepared_hable_white(Path("ffmpeg"), self.evidence, patches)

    def test_prepared_subtitle_contract_is_source_bound_and_official_baseline_stays_supported(self):
        with patch.object(TOOL, "run") as query:
            self.assertFalse(TOOL.verify_prepared_subtitle_options(Path("ffmpeg"), self.evidence, {}))
            query.assert_not_called()
        patches = {"0004-prepare-and-composite-videotoolbox-subtitles.patch": "source-bound"}
        answers = {"filter=subtitles_vt_images": "Filter subtitles_vt_images\n  font_provider <int>\n    fontconfig 3\n",
                   "filter=subtitles": "Filter subtitles\n  font_provider <int>\n    fontconfig 3\n",
                   "filter=overlay_videotoolbox": "Filter overlay_videotoolbox\n  ass <boolean>\n  bitmap <boolean>\n"}
        with patch.object(TOOL, "run", side_effect=lambda argv: answers[argv[-1]]):
            self.assertTrue(TOOL.verify_prepared_subtitle_options(Path("ffmpeg"), self.evidence, patches))
            for key in answers:
                original = answers[key]
                answers[key] = "Filter " + key.removeprefix("filter=") + "\n"
                with self.subTest(filter=key), self.assertRaisesRegex(ValueError, "filter/options"):
                    TOOL.verify_prepared_subtitle_options(Path("ffmpeg"), self.evidence, patches)
                answers[key] = original
            answers["filter=subtitles"] = "Filter subtitles\n  font_provider <int>\n"
            with self.assertRaisesRegex(ValueError, "Fontconfig authority"):
                TOOL.verify_prepared_subtitle_options(Path("ffmpeg"), self.evidence, patches)

    def test_font_tools_bind_pinned_role_bytes_configuration_and_provenance(self):
        policy = json.loads((ROOT / "scripts/macos-video-dependency-sources.json").read_text())["fc"]
        config = self.stage / "fontconfig"
        config.mkdir()
        (config / "fonts.conf").write_bytes(b"relocatable configuration")
        retained = self.evidence / "font-tools"
        retained.mkdir()
        (retained / "manifest.json").write_bytes(b"actual staged source facts")
        facts = {"schema_version": 1, "source_role": "fc", "source_commit": policy["commit"], "source_sha256": policy["sha256"],
                 "configuration": {"fonts.conf": TOOL.digest(config / "fonts.conf")},
                 "provenance_sha256": {"manifest.json": TOOL.digest(retained / "manifest.json")}, "binaries": {}}
        for name in ["fc-list", "fc-conflist"]:
            (self.stage / name).write_bytes(struct.pack("<8I", 0xFEEDFACF, 0x0100000C, 0, 2, 0, 0, 0, 0) + name.encode())
            facts["binaries"][name] = {"sha256": TOOL.digest(self.stage / name), "bytes": (self.stage / name).stat().st_size,
                                       "architecture": "arm64", "linked_libraries": ["/usr/lib/libSystem.B.dylib"]}
        def query(argv):
            return "arm64" if "-archs" in argv else "binary:\n  /usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n"
        with patch.object(TOOL, "run", side_effect=query), patch.object(TOOL.platform, "machine", return_value="arm64"):
            TOOL.validate_font_tools(self.stage, facts)
            for path in [self.stage / "fc-list", config / "fonts.conf", retained / "manifest.json"]:
                original = path.read_bytes()
                path.write_bytes(original + b"swapped")
                with self.subTest(path=path.name), self.assertRaises(ValueError):
                    TOOL.validate_font_tools(self.stage, facts)
                path.write_bytes(original)
            (config / "unexpected.conf").write_bytes(b"unbound")
            with self.assertRaises(ValueError):
                TOOL.validate_font_tools(self.stage, facts)
            (config / "unexpected.conf").unlink()
            facts["source_sha256"] = "0" * 64
            with self.assertRaises(ValueError):
                TOOL.validate_font_tools(self.stage, facts)

    def test_compiled_strict_options_require_decoder_and_both_renderer_boundaries(self):
        (self.evidence / "full-help.txt").write_text("     require_hardware .D.V. verified decoder")
        answers = {"filter=tonemapx": "   require_dovi <boolean>",
                   "filter=tonemap_videotoolbox": "   require_dovi <boolean>",
                   "decoder=hevc": "  -strict_dovi <boolean>"}
        with patch.object(TOOL, "run", side_effect=lambda argv: answers[argv[-1]]):
            TOOL.verify_strict_options(Path("ffmpeg"), self.evidence)
            for boundary in answers:
                original = answers[boundary]
                answers[boundary] = "missing"
                with self.subTest(boundary=boundary), self.assertRaisesRegex(ValueError, "enforcement"):
                    TOOL.verify_strict_options(Path("ffmpeg"), self.evidence)
                answers[boundary] = original
            (self.evidence / "full-help.txt").write_text("no verified hardware option")
            with self.assertRaisesRegex(ValueError, "hardware decoder enforcement"):
                TOOL.verify_strict_options(Path("ffmpeg"), self.evidence)


class MetalBindingContract(unittest.TestCase):
    def test_bwdif_parameter_buffer_index_must_match_shader_not_texture_slots(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            directory = source / "libavfilter"
            (directory / "metal").mkdir(parents=True)
            host = directory / "vf_bwdif_videotoolbox.m"
            shader = directory / "metal/vf_bwdif_videotoolbox.metal"
            shader.write_text("constant params& p [[buffer(0)]]")
            host.write_text("[encoder setTexture:next atIndex:3]; [encoder setBuffer:s->mtlParamsBuffer offset:0 atIndex:0];")
            self.assertEqual(TOOL.verify_bwdif_metal_binding(source), 0)
            host.write_text("[encoder setTexture:next atIndex:3]; [encoder setBuffer:s->mtlParamsBuffer offset:0 atIndex:4];")
            with self.assertRaisesRegex(ValueError, "disagrees with shader"):
                TOOL.verify_bwdif_metal_binding(source)

    def test_svn_source_offer_uses_local_versioned_inventory_and_excludes_generated_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary).resolve()
            tracked = source / "source.c"
            generated = source / "native.o"
            tracked.write_text("original source")
            generated.write_bytes(b"native object")
            document = f'<status><target><entry path="{tracked}"><wc-status item="normal"/></entry><entry path="{generated}"><wc-status item="unversioned"/></entry></target></status>'
            with patch.object(TOOL.subprocess, "check_output", return_value=document) as query:
                self.assertEqual(TOOL.local_svn_source_files(source, source / "private-config"), [(tracked, "source.c")])
                self.assertIn("status", query.call_args.args[0])
                self.assertNotIn("list", query.call_args.args[0])
                self.assertNotIn("--show-updates", query.call_args.args[0])
