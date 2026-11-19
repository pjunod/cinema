from importlib.machinery import SourceFileLoader
import importlib.util
import hashlib
import io
import json
import re
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[2]
loader = SourceFileLoader("media_runtime", str(ROOT / "scripts/media-runtime"))
spec = importlib.util.spec_from_loader(loader.name, loader)
media_runtime = importlib.util.module_from_spec(spec)
loader.exec_module(media_runtime)

OLD = "sha256:" + "1" * 64
NEW = "sha256:" + "2" * 64
SOURCE = (
    "# syntax=docker/dockerfile:1\n"
    "ARG PLURX_MEDIA_RUNTIME_REPOSITORY=ghcr.io/pjunod/cinema-media-runtime\n"
    f"FROM ${{PLURX_MEDIA_RUNTIME_REPOSITORY}}@{OLD} AS runtime-assets\n"
    "FROM runtime-assets AS runtime\n"
    f'ARG PLURX_MEDIA_RUNTIME_DIGEST="{OLD}"\n'
    "COPY --from=build /plurxd /usr/local/bin/plurxd\n"
)


def bundle_manifest():
    loader = SourceFileLoader("bundle_manifest", str(ROOT / "tools/dv_processing/bundle-manifest.py"))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


AMD64 = "sha256:" + "a" * 64
ARM64 = "sha256:" + "b" * 64
RECIPE = "c" * 64


def dv_source(inputs, recipe="9" * 64):
    return SOURCE + (
        f"# dv-dependency-recipe-sha256: {recipe}\n"
        f"FROM ${{PLURX_MEDIA_RUNTIME_REPOSITORY}}@{OLD} AS dv-processing-dependencies\n"
        f'ARG PLURX_DV_DEPENDENCY_INPUTS="{inputs}"\n'
    )


def platform_index(amd64=AMD64, arm64=ARM64):
    return json.dumps({"manifests": [
        {"platform": {"os": "linux", "architecture": "amd64"}, "digest": amd64},
        {"platform": {"os": "linux", "architecture": "arm64"}, "digest": arm64},
    ]})


def index(*architectures):
    return json.dumps({"manifests": [
        {"platform": {"os": "linux", "architecture": arch}}
        for arch in architectures
    ]})


class MediaRuntimeTests(unittest.TestCase):
    def test_pin_verifies_index_and_updates_only_digest_and_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            dockerfile.write_text(SOURCE)
            dockerfile.chmod(0o640)
            image = "registry.example:5000/noirr/plurx-media-runtime@" + NEW
            with patch.object(media_runtime, "run", return_value=index("amd64", "arm64")) as run, patch.object(media_runtime, "verify_public_image"):
                media_runtime.pin_image(image, dockerfile)
            self.assertEqual(dockerfile.read_text(), SOURCE.replace(OLD, NEW))
            self.assertEqual(dockerfile.stat().st_mode & 0o777, 0o640)
            run.assert_called_once_with(
                ["docker", "manifest", "inspect", image], capture=True)
            self.assertEqual(list(Path(directory).glob(".media-runtime-pin-*")), [])

    def test_pin_explicitly_supports_the_private_http_registry(self):
        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            dockerfile.write_text(SOURCE)
            image = "registry.example:5000/noirr/plurx-media-runtime@" + NEW
            with patch.object(media_runtime, "run", return_value=index("amd64", "arm64")) as run, patch.object(media_runtime, "verify_public_image"):
                media_runtime.pin_image(image, dockerfile, insecure=True)
            run.assert_called_once_with(
                ["docker", "manifest", "inspect", "--insecure", image], capture=True)
            self.assertEqual(dockerfile.read_text(), SOURCE.replace(OLD, NEW))

    def test_pin_refuses_missing_platform_without_changing_source(self):
        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            dockerfile.write_text(SOURCE)
            with patch.object(media_runtime, "run", return_value=index("amd64")):
                with self.assertRaisesRegex(ValueError, "both linux/amd64 and linux/arm64"):
                    media_runtime.pin_image("registry.example/noirr/plurx-media-runtime@" + NEW, dockerfile)
            self.assertEqual(dockerfile.read_text(), SOURCE)

    def test_pin_refuses_unpublished_digest_without_changing_source(self):
        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            dockerfile.write_text(SOURCE)
            with patch.object(media_runtime, "run", side_effect=subprocess.CalledProcessError(1, ["docker"])):
                with self.assertRaises(subprocess.CalledProcessError):
                    media_runtime.pin_image("registry.example/noirr/plurx-media-runtime@" + NEW, dockerfile)
            self.assertEqual(dockerfile.read_text(), SOURCE)

    def test_pin_refuses_private_only_digest_without_changing_source(self):
        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            dockerfile.write_text(SOURCE)
            with (
                patch.object(media_runtime, "run", return_value=index("amd64", "arm64")),
                patch.object(media_runtime, "verify_public_image",
                             side_effect=ValueError("canonical public media runtime is not anonymously available")) as public,
            ):
                with self.assertRaisesRegex(ValueError, "not anonymously available"):
                    media_runtime.pin_image("registry.example/noirr/plurx-media-runtime@" + NEW, dockerfile)
            public.assert_called_once_with(NEW)
            self.assertEqual(dockerfile.read_text(), SOURCE)
            self.assertEqual(list(Path(directory).glob(".media-runtime-pin-*")), [])

    def test_public_verification_checks_exact_index_and_both_platform_manifests(self):
        children = [json.dumps({"schemaVersion": 2, "config": {"digest": arch}}).encode()
                    for arch in ("amd64", "arm64")]
        digests = ["sha256:" + hashlib.sha256(payload).hexdigest() for payload in children]
        payload = json.dumps({"manifests": [
            {"platform": {"os": "linux", "architecture": arch}, "digest": digest}
            for arch, digest in zip(("amd64", "arm64"), digests)
        ]}).encode()
        digest = "sha256:" + hashlib.sha256(payload).hexdigest()
        opener = Mock()
        opener.open.side_effect = [io.BytesIO(b'{"token":"anonymous-pull-token"}'),
                                   io.BytesIO(payload), *[io.BytesIO(child) for child in children]]
        with patch.object(media_runtime.urllib.request, "build_opener", return_value=opener):
            media_runtime.verify_public_image(digest)
        requests = [call.args[0] for call in opener.open.call_args_list]
        self.assertEqual(len(requests), 4)
        self.assertTrue(requests[0].startswith("https://ghcr.io/token?"))
        self.assertEqual([request.full_url.rsplit("/", 1)[1] for request in requests[1:]],
                         [digest, *digests])
        self.assertTrue(all(request.get_header("Authorization") == "Bearer anonymous-pull-token"
                            for request in requests[1:]))

    def test_public_verification_refuses_wrong_bytes_or_missing_platform(self):
        for raw, digest, message in (
            (index("amd64", "arm64").encode(), NEW, "bytes do not match"),
            (index("amd64").encode(),
             "sha256:" + hashlib.sha256(index("amd64").encode()).hexdigest(), "both linux/amd64 and linux/arm64"),
        ):
            with self.subTest(message=message):
                opener = Mock()
                opener.open.side_effect = [io.BytesIO(b'{"token":"anonymous"}'), io.BytesIO(raw)]
                with (
                    patch.object(media_runtime.urllib.request, "build_opener", return_value=opener),
                    self.assertRaisesRegex(ValueError, message),
                ):
                    media_runtime.verify_public_image(digest)

    def test_pin_refuses_mutable_image_or_other_repository(self):
        # Keep the established regression ID. Explicit mirror repositories are
        # now supported, while mutable and malformed image references remain invalid.
        with patch.object(media_runtime, "run") as run:
            for image in ("registry.example/noirr/plurx-media-runtime:latest",
                          "registry.example/noirr/plurx-media-runtime@sha256:invalid",
                          "https://registry.example/noirr/plurx-media-runtime@" + NEW):
                with self.subTest(image=image), self.assertRaises(ValueError):
                    media_runtime.pin_image(image, Path("missing"))
            run.assert_not_called()

    def test_pin_refuses_ambiguous_or_inconsistent_application_contract(self):
        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            for source in (SOURCE + SOURCE, SOURCE.replace(f'DIGEST="{OLD}"', f'DIGEST="{NEW}"')):
                dockerfile.write_text(source)
                with patch.object(media_runtime, "run") as run:
                    with self.assertRaises(ValueError):
                        media_runtime.pin_image("registry.example/noirr/plurx-media-runtime@" + NEW, dockerfile)
                    run.assert_not_called()
                self.assertEqual(dockerfile.read_text(), source)

    def test_publication_workflow_is_manual_and_pins_published_native_platforms(self):
        workflow = (ROOT / ".github/workflows/media-runtime.yml").read_text()
        self.assertIn("workflow_dispatch:", workflow)
        self.assertNotIn("\n  push:", workflow)
        self.assertNotIn("\n  pull_request:", workflow)
        self.assertIn('"release-arm64"', workflow)
        self.assertIn("file: Dockerfile.media-runtime", workflow)
        self.assertIn("scripts/media-runtime pin", workflow)
        self.assertIn("needs: platform", workflow)
        self.assertIn("media-runtime-candidate", workflow)
        self.assertNotIn("git push", workflow)
        self.assertIn("docker manifest create --insecure --amend", workflow)
        self.assertIn('docker manifest push --insecure --purge "$tag"', workflow)
        self.assertNotIn("          scripts/media-runtime pin", workflow)
        self.assertNotIn("cp Dockerfile media-candidate/Dockerfile", workflow)
        self.assertIn("After anonymous public verification", workflow)
        self.assertIn("scripts/media-runtime pin ghcr.io/pjunod/cinema-media-runtime@%s", workflow)
        self.assertNotIn("imagetools", workflow)

    def test_maintenance_workflow_builds_audited_amd64_by_default(self):
        workflow = (ROOT / ".github/workflows/media-runtime.yml").read_text()
        self.assertIn("audited_amd64:", workflow)
        self.assertIn("default: true", workflow)
        self.assertEqual(workflow.count("if: matrix.arch == 'amd64' && inputs.audited_amd64"), 2)
        self.assertIn("target: linux-dolby-package-export", workflow)
        self.assertIn("outputs: type=tar,dest=linux-dolby-export.tar", workflow)
        self.assertIn("python3 -m validation.media_export linux-dolby-export.tar linux-dolby-export", workflow)
        self.assertIn("'runtime-assets-dolby-amd64' || 'media-runtime'", workflow)
        self.assertIn("'linux-dolby-package=./linux-dolby-export/package' || ''", workflow)
        self.assertIn("shipping-system-elf-sha256.txt", workflow)
        self.assertIn("sha256sum -c", workflow)
        self.assertIn("Audited-amd64: %s", workflow)
        # The standalone exporter, the final builder and the DV build
        # dependencies all use the maintenance recipe.
        self.assertEqual(workflow.count("file: Dockerfile.media-runtime"), 3)

    def test_maintenance_workflow_publishes_dependencies_and_qualifies_the_application_bundle(self):
        workflow = (ROOT / ".github/workflows/media-runtime.yml").read_text()
        self.assertIn("target: dv-processing-dependencies", workflow)
        # The helper bundle is never built by the maintenance recipe: the
        # application Dockerfile builds it against both candidates.
        qualify = workflow.split("- name: Qualify the application helper bundle against both candidates", 1)[1]
        qualify = qualify.split("\n      - name:", 1)[0]
        self.assertIn("file: Dockerfile\n", qualify)
        self.assertIn("target: runtime\n", qualify)
        self.assertIn("dv-processing-dependencies=docker-image://${{ env.RUNTIME_IMAGE }}@${{ steps.dv-dependencies.outputs.digest }}", qualify)
        self.assertIn("runtime-assets=docker-image://${{ env.RUNTIME_IMAGE }}@${{ steps.runtime.outputs.digest }}", qualify)
        self.assertIn("build=./media-qualification-build", qualify)
        self.assertIn("outputs: type=cacheonly", qualify)
        self.assertLess(workflow.index("Qualify the application helper bundle"),
                        workflow.index("media-digests/${{ matrix.arch }}.dv-dependencies.digest"))
        self.assertIn('docker manifest push --insecure --purge "$dv_tag"', workflow)
        self.assertIn("DV-Build-Dependencies: %s@%s", workflow)
        self.assertIn("scripts/media-runtime pin-dv-dependencies ghcr.io/pjunod/cinema-media-runtime@%s", workflow)

    def test_dv_dependency_pin_updates_only_its_digest_and_input_identity(self):
        source = dv_source("sha256:" + "3" * 64)
        repository = "registry.example:5000/noirr/plurx-media-runtime"
        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            dockerfile.write_text(source)
            image = repository + "@" + NEW
            with (
                patch.object(media_runtime, "run", return_value=platform_index()),
                patch.object(media_runtime, "verify_public_image") as public,
                patch.object(media_runtime, "dv_dependency_inputs", return_value="sha256:" + "4" * 64) as tree,
                patch.object(media_runtime, "dv_dependency_recipe", return_value=RECIPE) as recipe,
                patch.object(media_runtime, "image_dependency_inputs", return_value="sha256:" + "4" * 64) as recorded,
            ):
                media_runtime.pin_image(image, dockerfile, dv_dependencies=True)
            public.assert_called_once_with(NEW)
            tree.assert_called_once_with(dockerfile.resolve().parent)
            recipe.assert_called_once_with(dockerfile.resolve().parent)
            # Each platform is read from its own manifest, never from the index.
            self.assertEqual([call.args for call in recorded.call_args_list],
                             [(repository + "@" + AMD64, ("linux", "amd64")),
                              (repository + "@" + ARM64, ("linux", "arm64"))])
            self.assertEqual(dockerfile.read_text(), SOURCE + (
                f"# dv-dependency-recipe-sha256: {RECIPE}\n"
                f"FROM ${{PLURX_MEDIA_RUNTIME_REPOSITORY}}@{NEW} AS dv-processing-dependencies\n"
                f'ARG PLURX_DV_DEPENDENCY_INPUTS="sha256:{"4" * 64}"\n'
            ))
            # The runtime pin leaves the dependency pin alone, and the reverse.
            dockerfile.write_text(source)
            with patch.object(media_runtime, "run", return_value=index("amd64", "arm64")), patch.object(media_runtime, "verify_public_image"):
                media_runtime.pin_image(image, dockerfile)
            self.assertEqual(dockerfile.read_text(), source.replace(OLD, NEW, 1).replace(
                f'DIGEST="{OLD}"', f'DIGEST="{NEW}"'))
            # An index that cannot tell its platforms apart is refused unread.
            for broken in (platform_index(arm64=AMD64), platform_index(amd64=NEW), index("amd64", "arm64")):
                dockerfile.write_text(source)
                with (
                    patch.object(media_runtime, "run", return_value=broken),
                    patch.object(media_runtime, "verify_public_image"),
                    patch.object(media_runtime, "dv_dependency_inputs", return_value="sha256:" + "4" * 64),
                    patch.object(media_runtime, "image_dependency_inputs") as recorded,
                    self.assertRaisesRegex(ValueError, "media image index names"),
                ):
                    media_runtime.pin_image(image, dockerfile, dv_dependencies=True)
                recorded.assert_not_called()
                self.assertEqual(dockerfile.read_text(), source)

    def test_dv_dependency_pin_reads_each_platform_images_own_input_record(self):
        """A mocked docker that, like a real one holding one platform, answers
        an index reference with whichever image it has: only per-manifest
        references reach the right record."""
        repository = "registry.example:5000/noirr/plurx-media-runtime"
        source = dv_source("sha256:" + "3" * 64)
        image = repository + "@" + NEW
        local = {repository + "@" + AMD64: ("linux/amd64", "a1" * 32, b"tree inputs\n"),
                 repository + "@" + ARM64: ("linux/arm64", "b2" * 32, b"tree inputs\n")}
        # Asked for the index, this daemon serves its amd64 image both times.
        local[image] = local[repository + "@" + AMD64]
        tree = "sha256:" + hashlib.sha256(b"tree inputs\n").hexdigest()
        commands = []

        def docker(command, *, capture=False):
            commands.append(command)
            if command[:3] == ["docker", "manifest", "inspect"]:
                return platform_index()
            if command[:2] == ["docker", "create"]:
                self.assertEqual((command[2], command[5:]), ("--platform", ["true"]))
                return local[command[4]][1] + "\n"
            if command[:3] == ["docker", "image", "inspect"]:
                return local[command[-1]][0] + "\n"
            if command[:2] == ["docker", "cp"]:
                container, path = command[2].split(":", 1)
                self.assertEqual(path, "/opt/dv-dependencies/build-inputs.sha256")
                Path(command[3]).write_bytes(
                    next(record for _, held, record in local.values() if held == container))
                return ""
            raise AssertionError(command)

        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            dockerfile.write_text(source)
            with (
                patch.object(media_runtime, "run", side_effect=docker),
                patch.object(media_runtime.subprocess, "run") as removed,
                patch.object(media_runtime, "verify_public_image"),
                patch.object(media_runtime, "dv_dependency_inputs", return_value=tree),
                patch.object(media_runtime, "dv_dependency_recipe", return_value=RECIPE),
            ):
                media_runtime.pin_image(image, dockerfile, dv_dependencies=True)
                self.assertIn(NEW + " AS dv-processing-dependencies", dockerfile.read_text())
                self.assertIn(f'ARG PLURX_DV_DEPENDENCY_INPUTS="{tree}"', dockerfile.read_text())
                self.assertIn("# dv-dependency-recipe-sha256: " + RECIPE + "\n", dockerfile.read_text())
                created = [command[3:5] for command in commands if command[:2] == ["docker", "create"]]
                self.assertEqual(created, [["linux/amd64", repository + "@" + AMD64],
                                           ["linux/arm64", repository + "@" + ARM64]])
                # Both containers were created without being started, and removed.
                self.assertEqual([call.args[0] for call in removed.call_args_list],
                                 [["docker", "rm", "--force", "a1" * 32], ["docker", "rm", "--force", "b2" * 32]])
                self.assertFalse(any(command[1] in ("run", "start") for command in commands))
                # arm64 built from other inputs: read from its own manifest, so
                # the amd64 record this daemon would serve for the index cannot hide it.
                dockerfile.write_text(source)
                local[repository + "@" + ARM64] = ("linux/arm64", "b2" * 32, b"other inputs\n")
                removed.reset_mock()
                with self.assertRaisesRegex(ValueError, "linux/arm64 DV dependency image was built from other inputs"):
                    media_runtime.pin_image(image, dockerfile, dv_dependencies=True)
                self.assertEqual(dockerfile.read_text(), source)
                self.assertEqual(removed.call_count, 2)
                # A manifest that is not the platform it is listed as is refused.
                local[repository + "@" + ARM64] = ("linux/amd64", "b2" * 32, b"tree inputs\n")
                with self.assertRaisesRegex(ValueError, "is linux/amd64, not linux/arm64"):
                    media_runtime.pin_image(image, dockerfile, dv_dependencies=True)
                self.assertEqual(dockerfile.read_text(), source)

    def test_pin_refuses_a_value_the_published_bundle_does_not_carry(self):
        # The established regression ID. The runtime no longer carries a
        # bundle; what a pin can still misstate is which inputs the published
        # dependency image was built from, and the image itself is the witness.
        tree = "sha256:" + "4" * 64
        source = dv_source(tree)
        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            image = "registry.example/noirr/plurx-media-runtime@" + NEW
            for published in ("sha256:" + "5" * 64, "sha256:" + hashlib.sha256(b"").hexdigest()):
                dockerfile.write_text(source)
                with self.subTest(published=published), \
                        patch.object(media_runtime, "run", return_value=platform_index()), \
                        patch.object(media_runtime, "verify_public_image"), \
                        patch.object(media_runtime, "dv_dependency_inputs", return_value=tree), \
                        patch.object(media_runtime, "image_dependency_inputs", return_value=published):
                    with self.assertRaisesRegex(ValueError, "DV dependency image was built from other inputs"):
                        media_runtime.pin_image(image, dockerfile, dv_dependencies=True)
                self.assertEqual(dockerfile.read_text(), source)

    def test_bundle_builder_and_plurxd_digest_the_same_renderer_inputs(self):
        inputs = bundle_manifest().INPUTS
        runtime = (ROOT / "crates/plurxd/src/dv_runtime.rs").read_text()
        body = runtime.split("fn renderer_source_identity()", 1)[1].split("\n}\n", 1)[0]
        names = re.findall(r'\(\s*"([^"]+)",\s*include_bytes!\("\.\./\.\./\.\./tools/dv_processing/([^"]+)"\)', body)
        self.assertEqual([name for name, _ in names], sorted(inputs))
        self.assertTrue(all(name == path for name, path in names))
        # One implementation of the identity, shared by the installer's build,
        # its verification and its --identity query.
        installer = (ROOT / "tools/dv_processing/bundle-manifest.py").read_text()
        self.assertEqual(installer.count("def source_identity(source):"), 1)
        self.assertEqual(installer.count("source_identity(source)"), 3)
        self.assertIn("print(source_identity(args.source))", installer)

    def test_application_build_checks_the_installed_bundle_against_its_daemon(self):
        source = (ROOT / "Dockerfile").read_text()
        runtime = source.split("FROM runtime-assets AS runtime\n", 1)[1].split("\nFROM ", 1)[0]
        self.assertIn('RUN ["plurxd", "dv-helper-identity"]', runtime)
        # The recorded identity and its check of the pinned runtime's bundle
        # are gone with their subject: the runtime supplies no bundle.
        self.assertNotIn("PLURX_MEDIA_RUNTIME_DV_SOURCE_SHA256", source)
        self.assertNotIn("build-identity.json", source)

    def test_release_render_keeps_the_daemon_bundle_check(self):
        from validation.release_dockerfile import render
        generated = render((ROOT / "Dockerfile").read_text())
        self.assertIn('RUN ["plurxd", "dv-helper-identity"]', generated)
        self.assertIn("COPY --from=dv-processing-helpers /opt/dv-bundle /usr/lib/plurx/dv-processing", generated)

    def test_dv_dependency_pin_refuses_a_source_without_exactly_one_pin(self):
        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            dockerfile.write_text(SOURCE)
            with patch.object(media_runtime, "run") as run:
                with self.assertRaisesRegex(ValueError, "exactly one DV build dependency pin"):
                    media_runtime.pin_image("registry.example/noirr/plurx-media-runtime@" + NEW,
                                            dockerfile, dv_dependencies=True)
                run.assert_not_called()
            self.assertEqual(dockerfile.read_text(), SOURCE)

    def test_dv_dependency_input_identity_changes_with_any_input(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for folder, patterns in media_runtime.DV_DEPENDENCY_INPUTS:
                for pattern in patterns:
                    path = root / folder / pattern.replace("*", "decoder")
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(pattern)
            before = media_runtime.dv_dependency_inputs(root)
            (root / "tools/dv_processing/second.patch").write_text("another patch")
            added = media_runtime.dv_dependency_inputs(root)
            self.assertNotEqual(before, added)
            (root / "tools/dv_quality/backends/parsed-rpu/resolved-Cargo.lock").write_text("moved pin")
            self.assertNotEqual(added, media_runtime.dv_dependency_inputs(root))
            (root / "tools/dv_processing/build-requirements.txt").unlink()
            with self.assertRaisesRegex(ValueError, "is missing"):
                media_runtime.dv_dependency_inputs(root)


if __name__ == "__main__":
    unittest.main()
