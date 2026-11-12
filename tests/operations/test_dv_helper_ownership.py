"""The shipped DV helper bundle is built from the tree whose daemon expects it.

plurxd compiles the identity of tools/dv_processing into itself and disables
DV processing for a bundle built from other sources. These are static
contracts on the recipes; the image builds prove the bytes.
"""
from importlib.machinery import SourceFileLoader
import hashlib
import importlib.util
from pathlib import Path
import re
import runpy
import shutil
import subprocess
import unittest

from validation.release_dockerfile import _stages, render

ROOT = Path(__file__).resolve().parents[2]
DOCKERFILE = ROOT / "Dockerfile"
MEDIA_DOCKERFILE = ROOT / "Dockerfile.media-runtime"
DAEMON = ROOT / "crates/plurxd/src/dv_runtime.rs"
PACKAGING = runpy.run_path(str(ROOT / "tools/dv_processing/bundle-manifest.py"))
loader = SourceFileLoader("media_runtime_for_dv_helpers", str(ROOT / "scripts/media-runtime"))
spec = importlib.util.spec_from_loader(loader.name, loader)
media_runtime = importlib.util.module_from_spec(spec)
loader.exec_module(media_runtime)


def stage(source: str, name: str) -> str:
    found = [item for item in _stages(source) if item.name == name]
    if len(found) != 1:
        raise AssertionError(f"expected exactly one {name} stage, found {len(found)}")
    return source[found[0].start:found[0].end]


def instructions(text: str) -> list[str]:
    joined = re.sub(r"\\\n", " ", text)
    return [line.strip() for line in joined.splitlines()
            if line.strip() and not line.lstrip().startswith("#")]


def daemon_identity_inputs() -> list[tuple[str, Path]]:
    source = DAEMON.read_text(encoding="utf-8")
    body = source[source.index("fn renderer_source_identity() -> String {"):]
    body = body[:body.index("hex::encode(identity.finalize())")]
    declared = re.search(r"let inputs: \[\(&str, &\[u8\]\); (\d+)\] = \[", body)
    pairs = re.findall(r'\(\s*"([^"]+)",\s*include_bytes!\("([^"]+)"\),?\s*\)', body)
    if declared is None or int(declared.group(1)) != len(pairs):
        raise AssertionError("renderer_source_identity no longer has the shape this contract reads")
    return [(name, (DAEMON.parent / path).resolve()) for name, path in pairs]


class DvHelperOwnershipTests(unittest.TestCase):
    def test_application_build_compiles_the_bundle_from_its_own_context(self):
        source = DOCKERFILE.read_text(encoding="utf-8")
        helpers = instructions(stage(source, "dv-processing-helpers"))
        self.assertEqual(helpers[0], "FROM dv-processing-dependencies AS dv-processing-helpers")
        # A context copy: no --from, so the helper sources are this checkout's.
        self.assertIn("COPY tools/dv_processing /build-inputs/dv_processing", helpers)
        build = [line for line in helpers if line.startswith("RUN ")]
        self.assertEqual(len(build), 1)
        self.assertIn("sh /build-inputs/dv_processing/build-bundle.sh /opt/dv-dependencies /opt/dv-bundle", build[0])
        self.assertIn("bundle-manifest.py /opt/dv-bundle/sources /opt/dv-bundle --verify", build[0])
        runtime = instructions(stage(source, "runtime"))
        install = "COPY --from=dv-processing-helpers /opt/dv-bundle /usr/lib/plurx/dv-processing"
        self.assertEqual(runtime.count(install), 1)
        # The runtime owns the directory: nothing a media base left there survives.
        self.assertEqual(runtime[runtime.index(install) - 1], "RUN rm -rf /usr/lib/plurx/dv-processing")
        check = runtime[runtime.index(install) + 1]
        for helper in ("segment_decode_render", "mux_rgb", "author_p81", "scan_title"):
            self.assertIn(helper, check)
        self.assertIn("grep -q 'usage:'", check)
        # The bundle is installed nowhere else, and the pinned bases stay untouched.
        self.assertEqual(source.count("/usr/lib/plurx/dv-processing ") + source.count("/usr/lib/plurx/dv-processing\n"), 2)
        for name in ("runtime-assets", "dv-processing-dependencies"):
            self.assertEqual(len(instructions(stage(source, name))), 1, name)

    def test_published_media_images_never_carry_a_helper_bundle(self):
        media = MEDIA_DOCKERFILE.read_text(encoding="utf-8")
        for owner_only in ("build-bundle.sh", "bundle-manifest.py", "/usr/lib/plurx/dv-processing",
                           "/opt/dv-bundle", "AS dv-processing-helpers", "COPY tools/dv_processing "):
            self.assertNotIn(owner_only, media)
        dependencies = stage(media, "dv-processing-dependencies")
        self.assertIn("sh /build-inputs/dependency-inputs.sh > /opt/dv-dependencies/build-inputs.sha256",
                      dependencies)

    def test_release_packaging_keeps_the_application_helper_build(self):
        source = DOCKERFILE.read_text(encoding="utf-8")
        for generated in (render(source), render(source, "registry.example/media@sha256:" + "a" * 64)):
            self.assertIn("COPY tools/dv_processing /build-inputs/dv_processing", generated)
            self.assertIn("COPY --from=dv-processing-helpers /opt/dv-bundle /usr/lib/plurx/dv-processing", generated)
            self.assertEqual(generated.count("ARG PLURX_MEDIA_RUNTIME_REPOSITORY=ghcr.io/pjunod/cinema-media-runtime\n"), 1)
            self.assertLess(generated.index("AS dv-processing-helpers"), generated.index("AS runtime-assets"))
            self.assertNotIn("apt-get", generated)
            self.assertNotIn("cargo build", generated)

    def test_bundle_identity_covers_exactly_the_sources_the_daemon_compiled_in(self):
        daemon = daemon_identity_inputs()
        names = [name for name, _ in daemon]
        # The daemon hashes in array order and the bundle in sorted order.
        self.assertEqual(names, sorted(PACKAGING["INPUTS"]))
        self.assertEqual(len(set(names)), len(names))
        expected = hashlib.sha256()
        for name, path in daemon:
            self.assertEqual(path, ROOT / "tools/dv_processing" / name)
            expected.update(name.encode() + b"\0" + hashlib.sha256(path.read_bytes()).digest())
        bundle = hashlib.sha256()
        for name in sorted(PACKAGING["INPUTS"]):
            bundle.update(name.encode() + b"\0"
                          + bytes.fromhex(PACKAGING["sha"](ROOT / "tools/dv_processing" / name)))
        self.assertEqual(bundle.hexdigest(), expected.hexdigest())

    def test_dependency_pin_records_this_trees_dependency_inputs(self):
        source = DOCKERFILE.read_text(encoding="utf-8")
        recorded = media_runtime.DV_INPUTS_ARG.findall(source)
        self.assertEqual(len(recorded), 1)
        self.assertEqual(
            recorded[0], media_runtime.dv_dependency_inputs(),
            "a DV dependency input changed: publish new dependencies and run "
            "scripts/media-runtime pin-dv-dependencies (docs/ci/MEDIA-RUNTIME.md)")
        helpers = stage(source, "dv-processing-helpers")
        self.assertIn("cmp /build-inputs/application-inputs.sha256 /opt/dv-dependencies/build-inputs.sha256", helpers)
        self.assertIn('= "$PLURX_DV_DEPENDENCY_INPUTS"', helpers)
        # Every input the dependency recipe copies is in the recorded identity.
        dependencies = stage(MEDIA_DOCKERFILE.read_text(encoding="utf-8"), "dv-processing-dependencies")
        copied = set()
        for line in instructions(dependencies):
            if line.startswith("COPY ") and "--from=" not in line:
                copied.update(part for part in line.split()[1:-1] if part.startswith("tools/"))
        listed = {f"{directory}/{pattern}" for directory, patterns in media_runtime.DV_DEPENDENCY_INPUTS
                  for pattern in patterns}
        self.assertEqual(copied - {"tools/dv_processing/dependency-inputs.sh"}, listed)
        if shutil.which("sha256sum"):
            printed = subprocess.run(["sh", str(ROOT / "tools/dv_processing/dependency-inputs.sh")],
                                     check=True, capture_output=True).stdout
            self.assertEqual("sha256:" + hashlib.sha256(printed).hexdigest(), recorded[0])

    def test_dependency_pin_names_a_published_image(self):
        pins = media_runtime.DV_PIN.findall(DOCKERFILE.read_text(encoding="utf-8"))
        self.assertEqual(len(pins), 1)
        self.assertNotEqual(
            pins[0][1], media_runtime.UNPUBLISHED,
            "the DV build dependencies are not published yet: run the publish media runtime workflow, "
            "copy the DV-Build-Dependencies index public and run scripts/media-runtime pin-dv-dependencies")


if __name__ == "__main__":
    unittest.main()
