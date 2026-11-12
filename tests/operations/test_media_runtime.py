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
OLD_DV = "a" * 64
NEW_DV = "b" * 64
SOURCE = (
    "# syntax=docker/dockerfile:1\n"
    "ARG PLURX_MEDIA_RUNTIME_REPOSITORY=ghcr.io/pjunod/cinema-media-runtime\n"
    f"FROM ${{PLURX_MEDIA_RUNTIME_REPOSITORY}}@{OLD} AS runtime-assets\n"
    "FROM runtime-assets AS runtime\n"
    f'ARG PLURX_MEDIA_RUNTIME_DIGEST="{OLD}"\n'
    f'ARG PLURX_MEDIA_RUNTIME_DV_SOURCE_SHA256="{OLD_DV}"\n'
    "COPY --from=build /plurxd /usr/local/bin/plurxd\n"
)


def bundle_manifest():
    loader = SourceFileLoader("bundle_manifest", str(ROOT / "tools/dv_processing/bundle-manifest.py"))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


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
            with patch.object(media_runtime, "run", side_effect=[index("amd64", "arm64"), NEW_DV + "\n"]) as run, patch.object(media_runtime, "verify_public_image"):
                media_runtime.pin_image(image, dockerfile, NEW_DV)
            self.assertEqual(dockerfile.read_text(), SOURCE.replace(OLD, NEW).replace(OLD_DV, NEW_DV))
            self.assertEqual(dockerfile.stat().st_mode & 0o777, 0o640)
            self.assertEqual(run.call_args_list[0].args, (["docker", "manifest", "inspect", image],))
            self.assertEqual(run.call_args_list[1].args[0][:6],
                             ["docker", "run", "--rm", "--pull", "always", "--entrypoint"])
            self.assertIn("ghcr.io/pjunod/cinema-media-runtime@" + NEW, run.call_args_list[1].args[0])
            self.assertEqual(list(Path(directory).glob(".media-runtime-pin-*")), [])

    def test_pin_explicitly_supports_the_private_http_registry(self):
        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            dockerfile.write_text(SOURCE)
            image = "registry.example:5000/noirr/plurx-media-runtime@" + NEW
            with patch.object(media_runtime, "run", side_effect=[index("amd64", "arm64"), NEW_DV]) as run, patch.object(media_runtime, "verify_public_image"):
                media_runtime.pin_image(image, dockerfile, NEW_DV, insecure=True)
            self.assertEqual(run.call_args_list[0].args, (["docker", "manifest", "inspect", "--insecure", image],))
            self.assertEqual(dockerfile.read_text(), SOURCE.replace(OLD, NEW).replace(OLD_DV, NEW_DV))

    def test_pin_refuses_missing_platform_without_changing_source(self):
        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            dockerfile.write_text(SOURCE)
            with patch.object(media_runtime, "run", return_value=index("amd64")):
                with self.assertRaisesRegex(ValueError, "both linux/amd64 and linux/arm64"):
                    media_runtime.pin_image("registry.example/noirr/plurx-media-runtime@" + NEW, dockerfile, NEW_DV)
            self.assertEqual(dockerfile.read_text(), SOURCE)

    def test_pin_refuses_unpublished_digest_without_changing_source(self):
        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            dockerfile.write_text(SOURCE)
            with patch.object(media_runtime, "run", side_effect=subprocess.CalledProcessError(1, ["docker"])):
                with self.assertRaises(subprocess.CalledProcessError):
                    media_runtime.pin_image("registry.example/noirr/plurx-media-runtime@" + NEW, dockerfile, NEW_DV)
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
                    media_runtime.pin_image("registry.example/noirr/plurx-media-runtime@" + NEW, dockerfile, NEW_DV)
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
                    media_runtime.pin_image(image, Path("missing"), NEW_DV)
            run.assert_not_called()

    def test_pin_refuses_ambiguous_or_inconsistent_application_contract(self):
        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            for source in (SOURCE + SOURCE, SOURCE.replace(f'DIGEST="{OLD}"', f'DIGEST="{NEW}"')):
                dockerfile.write_text(source)
                with patch.object(media_runtime, "run") as run:
                    with self.assertRaises(ValueError):
                        media_runtime.pin_image("registry.example/noirr/plurx-media-runtime@" + NEW, dockerfile, NEW_DV)
                    run.assert_not_called()
                self.assertEqual(dockerfile.read_text(), source)

    def test_pin_requires_the_bundle_source_identity_and_one_record_of_it(self):
        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            image = "registry.example/noirr/plurx-media-runtime@" + NEW
            for source, dv_source in ((SOURCE, "B" * 64), (SOURCE, "b" * 63), (SOURCE, ""),
                                      (SOURCE.replace(f'ARG PLURX_MEDIA_RUNTIME_DV_SOURCE_SHA256="{OLD_DV}"\n', ""), NEW_DV)):
                dockerfile.write_text(source)
                with self.subTest(dv_source=dv_source), patch.object(media_runtime, "run") as run:
                    with self.assertRaises(ValueError):
                        media_runtime.pin_image(image, dockerfile, dv_source)
                    run.assert_not_called()
                self.assertEqual(dockerfile.read_text(), source)

    def test_pin_refuses_a_value_the_published_bundle_does_not_carry(self):
        with tempfile.TemporaryDirectory() as directory:
            dockerfile = Path(directory) / "Dockerfile"
            dockerfile.write_text(SOURCE)
            image = "registry.example/noirr/plurx-media-runtime@" + NEW
            for published in (OLD_DV, ""):
                with self.subTest(published=published), \
                        patch.object(media_runtime, "run", side_effect=[index("amd64", "arm64"), published]), \
                        patch.object(media_runtime, "verify_public_image"):
                    with self.assertRaisesRegex(ValueError, "published DV helper bundle reports"):
                        media_runtime.pin_image(image, dockerfile, NEW_DV)
                self.assertEqual(dockerfile.read_text(), SOURCE)

    def test_bundle_builder_and_plurxd_digest_the_same_renderer_inputs(self):
        inputs = bundle_manifest().INPUTS
        runtime = (ROOT / "crates/plurxd/src/dv_runtime.rs").read_text()
        body = runtime.split("fn renderer_source_identity()", 1)[1].split("\n}\n", 1)[0]
        names = re.findall(r'\(\s*"([^"]+)",\s*include_bytes!\("\.\./\.\./\.\./tools/dv_processing/([^"]+)"\)', body)
        self.assertEqual([name for name, _ in names], sorted(inputs))
        self.assertTrue(all(name == path for name, path in names))

    def test_application_build_checks_the_pinned_bundle_against_its_record(self):
        source = (ROOT / "Dockerfile").read_text()
        runtime = source.split("FROM runtime-assets AS runtime\n", 1)[1].split("\nFROM ", 1)[0]
        self.assertIn('ARG PLURX_MEDIA_RUNTIME_DV_SOURCE_SHA256="', runtime)
        self.assertIn("/usr/lib/plurx/dv-processing/build-identity.json", runtime)
        self.assertIn('"$PLURX_MEDIA_RUNTIME_DV_SOURCE_SHA256"', runtime)

    def test_release_render_keeps_the_pinned_bundle_check(self):
        from validation.release_dockerfile import render
        source = (ROOT / "Dockerfile").read_text()
        check = next(line for line in source.splitlines()
                     if line.startswith("RUN python3 -c") and "build-identity.json" in line)
        generated = render(source)
        self.assertIn('ARG PLURX_MEDIA_RUNTIME_DV_SOURCE_SHA256="', generated)
        self.assertIn(check, generated)

    def test_publication_workflow_ties_the_bundle_to_its_commit(self):
        workflow = (ROOT / ".github/workflows/media-runtime.yml").read_text()
        self.assertIn('test "$(cat media-digests/arm64.dv-source)" = "$dv_source"', workflow)
        self.assertIn('test "$(python3 tools/dv_processing/bundle-manifest.py --identity tools/dv_processing)" = "$dv_source"', workflow)
        self.assertIn("--dv-source-sha256 %s", workflow)

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
        # Both the standalone exporter and final builder use the maintenance recipe.
        self.assertEqual(workflow.count("file: Dockerfile.media-runtime"), 2)


if __name__ == "__main__":
    unittest.main()
