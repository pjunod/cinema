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

from validation.ci_scope import scope_for_paths
from validation.release_dockerfile import _stages, render
from validation.runner import load_catalog

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
        self.assertEqual(len(build), 2)
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
        # The daemon itself must accept the installed bundle, as the service account.
        verdict = 'RUN ["plurxd", "dv-helper-identity"]'
        self.assertEqual(runtime.count(verdict), 1)
        self.assertEqual(runtime[runtime.index(verdict) - 1], "USER plurx")
        self.assertLess(runtime.index("COPY --from=build /plurxd /usr/local/bin/plurxd"), runtime.index(verdict))
        self.assertNotIn("dv-helper-source-sha256", source)
        main = (ROOT / "crates/plurxd/src/main.rs").read_text(encoding="utf-8")
        self.assertIn("crate::vodencode::dv_runtime::installed_helper_identity().await", main)
        capture = DAEMON.read_text(encoding="utf-8")
        capture = capture[capture.index("pub(crate) async fn installed_helper_identity()"):]
        self.assertIn("ToolSet::capture()", capture[:capture.index("\n}\n")])
        # The bundle is installed nowhere else, and the pinned bases stay untouched.
        self.assertEqual(source.count("/usr/lib/plurx/dv-processing\n"), 2)
        # The CI image inherits the same base and installs no bundle: it must
        # not keep the base's, which matches no checkout it will test.
        ci = instructions(stage(source, "ci"))
        self.assertRegex(next(line for line in ci if line.startswith("RUN ")),
                         r"^RUN rm -rf /usr/lib/plurx/dv-processing\s+&& ")
        self.assertFalse(any("dv-processing-helpers" in line for line in ci))
        for name in ("runtime-assets", "dv-processing-dependencies"):
            self.assertEqual(len(instructions(stage(source, name))), 1, name)

    def test_application_build_runs_the_active_area_c_controls(self):
        helpers = instructions(stage(DOCKERFILE.read_text(encoding="utf-8"), "dv-processing-helpers"))
        control = [line for line in helpers if "test_dm_active_area" in line]
        self.assertEqual(len(control), 1)
        run = control[0]
        self.assertTrue(run.startswith("RUN "))
        self.assertGreater(helpers.index(run), helpers.index(
            next(line for line in helpers if "build-bundle.sh /opt/dv-dependencies" in line)))
        # Compiled from this context with warnings fatal, then executed: every
        # step is joined by &&, so a failing case fails the stage.
        self.assertIn("-Wall -Wextra -Werror", run)
        self.assertIn("/build-inputs/dv_processing/controls/test_dm_active_area.c", run)
        self.assertIn("-o /build-inputs/test_dm_active_area", run)
        self.assertTrue(run.rstrip().endswith("/build-inputs/test_dm_active_area"))
        self.assertNotIn("||", run)
        self.assertNotIn(";", run)
        source = (ROOT / "tools/dv_processing/controls/test_dm_active_area.c").read_text(encoding="utf-8")
        wrapper = (ROOT / "tools/dv_processing/controls/test_dm_active_area.py").read_text(encoding="utf-8")
        cases = re.findall(r"(?m)^static void (test_l5_\w+)\(void\)", source)
        self.assertGreaterEqual(len(cases), 6)
        for case in cases:
            # Defined, called from main, and named by the Regression-Test wrapper.
            self.assertIn(f"  RUN({case});", source)
            self.assertIn(case, wrapper)
        self.assertIn("test_l5_cm29_crop_domain_excludes_adversarial_bars", cases)

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
        for name, path in daemon:
            self.assertEqual(path, ROOT / "tools/dv_processing" / name)
        # Names only. That the two hashes agree is the daemon's to say: its
        # bundle-capture unit test runs this installer through capture_at, and
        # every image build runs `plurxd dv-helper-identity` on the real bundle.
        capture = DAEMON.read_text(encoding="utf-8")
        self.assertIn('include_str!("../../../tools/dv_processing/bundle-manifest.py")', capture)
        self.assertIn("async fn revised_bundle_capture_accepts_current_sources_and_rejects_stale_or_incomplete_identity(", capture)

    def test_every_source_compiled_into_the_bundle_is_in_its_identity(self):
        source = ROOT / "tools/dv_processing"
        recipe = (source / "build.sh").read_text(encoding="utf-8")
        pending = re.findall(r'"\$source_dir/([A-Za-z0-9_]+\.c)"', recipe)
        # One compile and one existence test name author_p81.c; the rest once.
        self.assertEqual(sorted(set(pending)),
                         ["author_p81.c", "mux_rgb.c", "scan_title.c", "segment_decode_render.c"])
        self.assertEqual(recipe.count('-o "$output_dir/'), 4)
        compiled = set()
        while pending:
            name = pending.pop()
            if name in compiled:
                continue
            compiled.add(name)
            for include in re.findall(r'(?m)^[ \t]*#[ \t]*include[ \t]+"([^"]+)"',
                                      (source / name).read_text(encoding="utf-8")):
                self.assertTrue((source / include).is_file(), f"{name} includes {include}")
                pending.append(include)
        # The decoder patch is compiled into the published FFmpeg, and is the
        # one identity input that is not a helper source.
        self.assertEqual(set(PACKAGING["INPUTS"]), compiled | {"hevc-rasl-frame-thread.patch"})
        self.assertEqual({name for name, _ in daemon_identity_inputs()}, set(PACKAGING["INPUTS"]))

    def test_a_helper_only_change_selects_the_daemon_suite_and_the_image_builds(self):
        catalog = load_catalog(ROOT / "validation/points.toml")
        for path in ("tools/dv_processing/dv_dm.h", "tools/dv_processing/mux_rgb.c",
                     "tools/dv_processing/hevc-rasl-frame-thread.patch",
                     "tools/dv_quality/backends/parsed-rpu/resolved-Cargo.lock"):
            scope = scope_for_paths(catalog, (path,))
            for lane in ("rust", "release_build", "container"):
                self.assertTrue(scope[lane], f"{path} must select {lane}")
            self.assertFalse(scope["docs_only"], path)

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

    def test_dependency_recipe_has_not_changed_since_the_pinned_publication(self):
        source = DOCKERFILE.read_text(encoding="utf-8")
        recorded = re.findall(r"(?m)^# dv-dependency-recipe-sha256: ([0-9a-f]{64})$", source)
        self.assertEqual(len(recorded), 1)
        # Directly above the pin it describes.
        self.assertRegex(source, r"(?m)^# dv-dependency-recipe-sha256: [0-9a-f]{64}\n"
                                 r"FROM \S+ AS dv-processing-dependencies$")
        self.assertEqual(
            recorded[0], media_runtime.dv_dependency_recipe(),
            "the dependency stage of Dockerfile.media-runtime, rust-toolchain.toml or "
            "scripts/configure-debian-snapshot changed: publish new DV build dependencies and run "
            "scripts/media-runtime pin-dv-dependencies (docs/ci/MEDIA-RUNTIME.md)")
        # The identity moves with each part of the recipe and with nothing else.
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("Dockerfile.media-runtime", *media_runtime.DV_DEPENDENCY_RECIPE_FILES):
                (root / name).parent.mkdir(parents=True, exist_ok=True)
                (root / name).write_bytes((ROOT / name).read_bytes())
            self.assertEqual(media_runtime.dv_dependency_recipe(root), recorded[0])
            media = (root / "Dockerfile.media-runtime").read_text(encoding="utf-8")
            for old, new in (("ARG DEBIAN_SNAPSHOT=20260928T000000Z\nARG TARGETARCH\nARG PLURX_DV_BUILD_JOBS",
                              "ARG DEBIAN_SNAPSHOT=20261001T000000Z\nARG TARGETARCH\nARG PLURX_DV_BUILD_JOBS"),
                             ("libvulkan-dev glslang-dev", "libvulkan-dev libfoo-dev glslang-dev"),
                             ("AS dv-processing-dependencies\n", "AS dv-processing-dependencies\nARG EXTRA=1\n")):
                self.assertEqual(media.count(old), 1, old)
                (root / "Dockerfile.media-runtime").write_text(media.replace(old, new), encoding="utf-8")
                self.assertNotEqual(media_runtime.dv_dependency_recipe(root), recorded[0], old)
            # A later stage, or the comment introducing it, is not this recipe.
            (root / "Dockerfile.media-runtime").write_text(
                media.replace("# Source-built portable recognizer.", "# Source-built recognizer, reworded."),
                encoding="utf-8")
            self.assertEqual(media_runtime.dv_dependency_recipe(root), recorded[0])
            (root / "Dockerfile.media-runtime").write_text(media, encoding="utf-8")
            for name in media_runtime.DV_DEPENDENCY_RECIPE_FILES:
                original = (root / name).read_bytes()
                (root / name).write_bytes(original + b"\n")
                self.assertNotEqual(media_runtime.dv_dependency_recipe(root), recorded[0], name)
                (root / name).write_bytes(original)

    def test_dependency_pin_names_a_published_image(self):
        pins = media_runtime.DV_PIN.findall(DOCKERFILE.read_text(encoding="utf-8"))
        self.assertEqual(len(pins), 1)
        self.assertNotEqual(
            pins[0][1], media_runtime.UNPUBLISHED,
            "the DV build dependencies are not published yet: run the publish media runtime workflow, "
            "copy the DV-Build-Dependencies index public and run scripts/media-runtime pin-dv-dependencies")


if __name__ == "__main__":
    unittest.main()
