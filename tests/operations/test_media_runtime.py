from importlib.machinery import SourceFileLoader
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
loader = SourceFileLoader("media_runtime", str(ROOT / "scripts/media-runtime"))
spec = importlib.util.spec_from_loader(loader.name, loader)
media_runtime = importlib.util.module_from_spec(spec)
loader.exec_module(media_runtime)

OLD = "sha256:" + "1" * 64
NEW = "sha256:" + "2" * 64
SOURCE = (
    "# syntax=docker/dockerfile:1\n"
    "ARG PLURX_MEDIA_REGISTRY=forge.lan:3000\n"
    f"FROM ${{PLURX_MEDIA_REGISTRY}}/noirr/plurx-media-runtime@{OLD} AS runtime-assets\n"
    "FROM runtime-assets AS runtime\n"
    f'ARG PLURX_MEDIA_RUNTIME_DIGEST="{OLD}"\n'
    "COPY --from=build /plurxd /usr/local/bin/plurxd\n"
)


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
            with patch.object(media_runtime, "run", return_value=index("amd64", "arm64")) as run:
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
            with patch.object(media_runtime, "run", return_value=index("amd64", "arm64")) as run:
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

    def test_pin_refuses_mutable_image_or_other_repository(self):
        with patch.object(media_runtime, "run") as run:
            for image in ("registry.example/noirr/plurx-media-runtime:latest",
                          "registry.example/noirr/plurxd@" + NEW,
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
        self.assertIn("scripts/media-runtime pin --insecure", workflow)
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
