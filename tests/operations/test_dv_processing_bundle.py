"""Packaging hash controls only; synthetic files never establish video authority."""
import contextlib
import io
import json
from pathlib import Path
import runpy
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
PACKAGING = runpy.run_path(str(ROOT / "tools/dv_processing/bundle-manifest.py"))
DARWIN_PACKAGING = runpy.run_path(str(ROOT / "tools/dv_processing/build-macos-bundle.py"))


class DvBundleManifestTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.bundle = Path(self.scratch.name) / "bundle"
        for folder in ("bin", "lib", "sources"):
            (self.bundle / folder).mkdir(parents=True)
        for name in ("segment_decode_render", "mux_rgb", "author_p81", "scan_title"):
            (self.bundle / "bin" / name).write_bytes(b"packaging-only synthetic artifact")
        (self.bundle / "lib/libplacebo.so.387").write_bytes(b"packaging-only library")
        (self.bundle / "sources/libdovi.a").write_bytes(b"packaging-only parser")
        (self.bundle / "sources/abi.json").write_text(json.dumps(
            {"libplacebo": 387, "avcodec": 63, "avformat": 63, "avutil": 61}))
        self.source = ROOT / "tools/dv_processing"
        with contextlib.redirect_stdout(io.StringIO()):
            PACKAGING["build"](self.source, self.bundle)

    def verify(self):
        with contextlib.redirect_stdout(io.StringIO()):
            PACKAGING["verify"](self.source, self.bundle)

    def test_installed_renderer_and_library_hash_tampering_is_refused(self):
        self.verify()
        for relative in ("bin/segment_decode_render", "bin/scan_title", "lib/libplacebo.so.387"):
            artifact = self.bundle / relative
            original = artifact.read_bytes()
            artifact.write_bytes(original + b"tampered")
            with self.assertRaises(AssertionError):
                self.verify()
            artifact.write_bytes(original)
        self.verify()

    def test_installed_artifact_escape_and_machine_icd_override_are_refused(self):
        path = self.bundle / "build-identity.json"
        original = json.loads(path.read_text())
        outside = Path(self.scratch.name) / "outside"
        outside.write_bytes(b"outside root")
        changed = json.loads(json.dumps(original))
        changed["libraries"][0] = {"path": "../outside", "sha256": PACKAGING["sha"](outside)}
        path.write_text(json.dumps(changed))
        with self.assertRaises(AssertionError):
            self.verify()
        changed = json.loads(json.dumps(original))
        changed["environment"]["VK_ICD_FILENAMES"] = "specific-machine.json"
        path.write_text(json.dumps(changed))
        with self.assertRaises(AssertionError):
            self.verify()

    def test_darwin_manifest_binds_the_relocated_library_closure_without_loader_env(self):
        sources = self.bundle / "lib"
        libraries = ["libdependency.1.dylib", "libplacebo.387.dylib"]
        for name in libraries:
            (sources / name).write_bytes(name.encode())
        with contextlib.redirect_stdout(io.StringIO()):
            PACKAGING["build"](self.source, self.bundle, darwin=True)
        manifest = json.loads((self.bundle / "build-identity.json").read_text())
        self.assertEqual(manifest["environment"], {})
        self.assertEqual([row["path"] for row in manifest["libraries"]], [f"lib/{name}" for name in libraries])
        (sources / libraries[0]).write_bytes(b"changed after signing")
        with self.assertRaises(AssertionError):
            PACKAGING["verify"](self.source, self.bundle, darwin=True)

    def test_darwin_graph_resolves_inherited_rpaths_and_refuses_ambiguous_dependencies(self):
        executable = self.bundle / "bin/segment_decode_render"
        prefix = Path(self.scratch.name) / "external"
        first = prefix / "first"
        second = prefix / "second"
        first.mkdir(parents=True)
        second.mkdir()
        renderer = first / "libplacebo.387.dylib"
        dependency = first / "libdependency.1.dylib"
        for image in (renderer, dependency):
            image.touch()
        graph_fn = DARWIN_PACKAGING["dependency_graph"]
        links = {
            executable: (["@rpath/libplacebo.387.dylib"], [str(first), str(second)], []),
            renderer: (["@rpath/libdependency.1.dylib"], [], [renderer.name]),
            dependency: (["/usr/lib/libSystem.B.dylib"], [], [dependency.name]),
        }
        with patch.dict(graph_fn.__globals__, {"macho_links": links.__getitem__}):
            graph, libraries = graph_fn([executable], [prefix])
            self.assertEqual(libraries, {renderer.name: renderer, dependency.name: dependency})
            self.assertEqual(graph[renderer], {"@rpath/libdependency.1.dylib": dependency})
            (second / dependency.name).touch()
            with self.assertRaisesRegex(ValueError, "ambiguous Mach-O dependency"):
                graph_fn([executable], [prefix])

    def test_vulkan_discovery_replaces_only_the_obsolete_intel_provider(self):
        package = Path(self.scratch.name) / "media-package"
        source = package / "share/vulkan/icd.d/intel_icd.x86_64.json"
        source.parent.mkdir(parents=True)
        library = package / "lib/libvulkan_intel.so"
        library.parent.mkdir()
        library.write_bytes(b"pinned-provider-control")
        source.write_text(json.dumps({"ICD": {"library_path": str(library)}}))
        discovery = Path(self.scratch.name) / "discovery"
        discovery.mkdir()
        for name in ("intel_icd.x86_64.json", "intel_hasvk_icd.x86_64.json", "lvp_icd.x86_64.json", "nvidia_icd.json"):
            (discovery / name).write_text(name)
        subprocess.run(["sh", str(ROOT / "tools/dv_processing/register-vulkan-driver.sh"), str(package), str(discovery)], check=True)
        self.assertEqual((discovery / source.name).read_bytes(), source.read_bytes())
        self.assertEqual(len(list(discovery.iterdir())), 4)
        for name in ("intel_hasvk_icd.x86_64.json", "lvp_icd.x86_64.json", "nvidia_icd.json"):
            self.assertEqual((discovery / name).read_text(), name)

    def test_vulkan_provider_relocation_refuses_relative_library_path(self):
        package = Path(self.scratch.name) / "media-package"
        source = package / "share/vulkan/icd.d/intel_icd.x86_64.json"
        source.parent.mkdir(parents=True)
        library = package / "lib/libvulkan_intel.so"
        library.parent.mkdir()
        library.write_bytes(b"pinned-provider-control")
        source.write_text(json.dumps({"ICD": {"library_path": "../../../lib/libvulkan_intel.so"}}))
        discovery = Path(self.scratch.name) / "discovery"
        discovery.mkdir()
        incumbent = discovery / source.name
        incumbent.write_text("incumbent")
        result = subprocess.run(["sh", str(ROOT / "tools/dv_processing/register-vulkan-driver.sh"), str(package), str(discovery)], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unexpected library path", result.stderr)
        self.assertEqual(incumbent.read_text(), "incumbent")


if __name__ == "__main__":
    unittest.main()
