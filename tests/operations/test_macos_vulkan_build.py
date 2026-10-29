"""Exercise the native Vulkan source closure without compiling GPU libraries."""
import io
import json
from pathlib import Path
import shutil
import tarfile
import tempfile
import unittest
from unittest import mock

from tests.operations.test_ffmpeg_support_matrix import ROOT, load


class MacosVulkanBuildTests(unittest.TestCase):
    def test_fast_float_is_available_before_configuration_and_retained_on_retry(self):
        helper = load("build-macos-vulkan")
        media = load("build-macos-video-ffmpeg")
        with tempfile.TemporaryDirectory() as work:
            root = Path(work).resolve()
            prepared = root / "prepared"
            prepared.mkdir()
            (prepared / "prepared-source.json").write_text("{}")
            patch = prepared / "source/builder/patches/libplacebo/0001-libav-utils-fix-non-multiplane-fmt-bit_shift-in-pl_m.patch"
            patch.parent.mkdir(parents=True)
            patch.write_text("fixture patch\n")
            recipes = root / "recipes"
            recipes.mkdir()
            shutil.copyfile(ROOT / "scripts/build-macos-vulkan", recipes / "build-macos-vulkan")
            helper.__file__ = str(recipes / "build-macos-vulkan")
            media.__file__ = str(recipes / "build-macos-video-ffmpeg")
            cache = root / "cache"
            cache.mkdir()
            files = {
                "shaderc": {"CHANGES": "v1.2.3 fixture\n", "libshaderc/include/shaderc.h": "shaderc"},
                "vulkan-headers": {"include/vulkan.h": "vulkan", "registry/vk.xml": "registry"},
                "moltenvk-binary": {"MoltenVK/MoltenVK/static/MoltenVK.xcframework/macos-arm64_x86_64/libMoltenVK.a": "vulkan archive"},
                "fast-float": {"include/fast_float/fast_float.h": "pinned header", "LICENSE-MIT": "fixture license"},
            }
            facts = json.loads((ROOT / "scripts/macos-vulkan-sources.json").read_text())["sources"]
            for fact in facts:
                archive = cache / fact["archive"]
                with tarfile.open(archive, "w:gz") as output:
                    for name, text in files.get(fact["role"], {"fixture": fact["role"]}).items():
                        member = tarfile.TarInfo(name if fact["role"] == "moltenvk-binary" else "upstream/" + name)
                        data = text.encode()
                        member.size = len(data)
                        output.addfile(member, io.BytesIO(data))
                fact["sha256"] = helper.digest(archive)
            lock = recipes / "macos-vulkan-sources.json"
            lock.write_text(json.dumps({"sources": facts}))
            header = prepared / "native-vulkan/sources/libplacebo/3rdparty/fast_float/include/fast_float/fast_float.h"
            setups = []

            def run(argv, log, env=None, cwd=None):
                if argv[0] == "meson":
                    self.assertEqual(header.read_text(), "pinned header")
                    self.assertEqual((header.parents[2] / "LICENSE-MIT").read_text(), "fixture license")
                    setups.append(argv)
                    Path(argv[2]).mkdir(parents=True)
                    (Path(argv[2]) / "build.ninja").touch()
                elif argv[0] == "ninja" and argv[-1] == "shaderc_combined":
                    library = Path(argv[2]) / "libshaderc/libshaderc_combined.a"
                    library.parent.mkdir(parents=True, exist_ok=True)
                    library.write_text("shaderc archive")
                elif argv[0] == "ninja" and argv[-1] == "install":
                    self.assertEqual(header.read_text(), "pinned header")
                    (prepared / "prefix/lib/libplacebo.a").write_text("libplacebo archive")

            with mock.patch.object(helper.platform, "system", return_value="Darwin"), \
                    mock.patch.object(helper.platform, "machine", return_value="arm64"), \
                    mock.patch.object(helper.shutil, "which", return_value="fixture-tool"), \
                    mock.patch.object(helper, "run", side_effect=run):
                helper.build(prepared, 1, cache)
                self.assertEqual(len(setups), 1)
                # A warm build must restore the submodule even when Meson setup is skipped.
                shutil.rmtree(header.parents[2])
                manifest = helper.build(prepared, 1, cache)
                self.assertEqual(len(setups), 1)
            self.assertIn("fast-float", {fact["role"] for fact in manifest["sources"]})
            identity = {"vulkan_builder_sha256": helper.digest(recipes / "build-macos-vulkan"),
                        "vulkan_source_lock_sha256": helper.digest(lock)}
            evidence = root / "evidence"
            evidence.mkdir()
            retained = media.retain_native_vulkan(prepared, evidence, identity)
            fast_float = next(fact for fact in facts if fact["role"] == "fast-float")
            offer = evidence / "native-vulkan" / fast_float["archive"]
            self.assertEqual(retained["source_archives"][offer.name], helper.digest(offer))
            with tarfile.open(offer) as source:
                self.assertEqual(source.extractfile("upstream/LICENSE-MIT").read(), b"fixture license")
            archive = prepared / "native-vulkan/source-archives" / fast_float["archive"]
            archive.write_bytes(b"altered archive")
            rejected = root / "rejected"
            rejected.mkdir()
            with self.assertRaisesRegex(ValueError, "pinned checksum"):
                media.retain_native_vulkan(prepared, rejected, identity)
