"""Pi runtime packaging contracts; no network or hardware required."""
import hashlib
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("pi_runtime", str(ROOT / "deploy/pi-runtime"))
spec = importlib.util.spec_from_loader(loader.name, loader)
runtime = importlib.util.module_from_spec(spec)
loader.exec_module(runtime)


class PiRuntimeTests(unittest.TestCase):
    def test_source_and_browser_pins_are_explicit_and_patch_matches(self):
        manifest = runtime.MANIFEST
        self.assertEqual(hashlib.sha256((runtime.ASSETS / "jellyfin-8.1.3-rpi.patch").read_bytes()).hexdigest(), manifest["patch_sha256"])
        for name in ("jellyfin", "rpi", "rustup"):
            self.assertRegex(manifest[name]["sha256"], r"^[0-9a-f]{64}$")
            self.assertTrue(manifest[name]["url"].startswith("https://"))
        self.assertEqual(len(manifest["browser"]["packages"]), 3)
        for package in manifest["browser"]["packages"]:
            self.assertIn(manifest["browser"]["tag"], package["url"])
            self.assertRegex(package["sha256"], r"^[0-9a-f]{64}$")

    def test_request_transfer_and_full_jellyfin_series_are_preserved(self):
        patch = (runtime.ASSETS / "jellyfin-8.1.3-rpi.patch").read_text()
        for name in ("libavcodec/v4l2_request_hevc.c", "libavutil/rpi_sand_fns.c", "libavutil/hwcontext_drm.c"):
            self.assertIn(name, patch)
        self.assertIn("v4l2_req_hevc_v4.o v4l2_fmt.o", patch)
        self.assertNotIn("tests/fate/filter-video.mak", patch)
        provider = (ROOT / "deploy/pi-runtime").read_text()
        self.assertIn('source / "debian/patches/series"', provider)
        self.assertIn('"--fuzz=0"', provider)
        for capability in ("ac4", "dovi_rpu", "apply_dovi"):
            self.assertIn(capability, provider)

    def test_uninstall_keeps_changed_and_unowned_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            prefix = Path(temporary) / "owned"
            prefix.mkdir()
            (prefix / "unchanged").write_text("owned")
            (prefix / "changed").write_text("original")
            runtime.ownership_receipt(prefix, [prefix / "unchanged", prefix / "changed"])
            (prefix / "changed").write_text("operator change")
            (prefix / "new").write_text("operator addition")
            result = runtime.uninstall(prefix)
            self.assertFalse((prefix / "unchanged").exists())
            self.assertTrue((prefix / "changed").exists())
            self.assertTrue((prefix / "new").exists())
            self.assertIn("changed", result["retained"])

    def test_uninstall_does_not_follow_replaced_parent_symlink(self):
        with tempfile.TemporaryDirectory() as temporary:
            prefix = Path(temporary) / "owned"
            directory = prefix / "runtime"
            directory.mkdir(parents=True)
            (directory / "binary").write_text("same")
            runtime.ownership_receipt(prefix, [directory / "binary"])
            (directory / "binary").unlink()
            directory.rmdir()
            outside = Path(temporary) / "outside"
            outside.mkdir()
            (outside / "binary").write_text("same")
            directory.symlink_to(outside, target_is_directory=True)
            result = runtime.uninstall(prefix)
            self.assertTrue((outside / "binary").exists())
            self.assertIn("runtime/binary", result["retained"])

    def test_upgrade_does_not_adopt_operator_additions_or_changed_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            prefix = Path(temporary) / "owned"
            downloads = prefix / "downloads"
            downloads.mkdir(parents=True)
            original = downloads / "original"
            original.write_text("installed")
            rewritten = downloads / "provider-rewritten"
            rewritten.write_text("old provider output")
            runtime.ownership_receipt(prefix, [original, rewritten])
            original.write_text("operator edit")
            added = downloads / "operator-added"
            added.write_text("operator addition")
            outside = prefix / "operator-directory"
            outside.mkdir()
            outside_file = outside / "keep"
            outside_file.write_text("operator addition")
            before = set(prefix.rglob("*"))
            upgrade = downloads / "new-provider-artifact"
            locations = [rewritten, upgrade]
            unchanged = runtime.unchanged_owned(prefix, locations)
            upgrade.write_text("new installed output")
            rewritten.write_text("new provider output")
            runtime.record_outputs(prefix, before, unchanged, locations)
            ledger = json.loads((prefix / ".plurx-runtime-owned.json").read_text())
            self.assertNotIn("downloads/operator-added", ledger["files"])
            self.assertEqual(ledger["files"]["downloads/original"]["sha256"], hashlib.sha256(b"installed").hexdigest())
            result = runtime.uninstall(prefix)
            self.assertTrue(original.exists())
            self.assertTrue(added.exists())
            self.assertTrue(outside_file.exists())
            self.assertFalse(upgrade.exists())
            self.assertFalse(rewritten.exists())
            self.assertIn("downloads/original", result["retained"])

    def test_upgrade_refuses_to_overwrite_changed_managed_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            prefix = Path(temporary) / "owned"
            prefix.mkdir()
            output = prefix / "Dockerfile.base"
            output.write_text("installed")
            runtime.ownership_receipt(prefix, [output])
            output.write_text("operator edit")
            with self.assertRaisesRegex(RuntimeError, "refusing overwrite"):
                runtime.unchanged_owned(prefix, [output])
            self.assertEqual(output.read_text(), "operator edit")

    def test_docker_preserves_standard_assets_and_private_runtime(self):
        dockerfile = (ROOT / "Dockerfile.pi").read_text()
        self.assertIn("FROM ${BASE_IMAGE}", dockerfile)
        self.assertIn("USER plurx", dockerfile)
        self.assertIn("PLURX_FFMPEG=/opt/plurx-runtime/ffmpeg-8.1.3-pi-", dockerfile)
        self.assertNotIn("PLURX_BOUND_FFPROBE=", dockerfile)
        self.assertNotIn("--privileged", dockerfile)


if __name__ == "__main__":
    unittest.main()
