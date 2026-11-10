"""Release qualification uses the candidate's media runtime and host-owned source."""
from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("release_readiness", str(ROOT / "scripts/release-readiness"))
spec = importlib.util.spec_from_loader(loader.name, loader)
readiness = importlib.util.module_from_spec(spec)
loader.exec_module(readiness)


class ReleaseReadinessCase(unittest.TestCase):
    def test_both_release_paths_use_candidate_runtime_after_checkout(self):
        workflow = (ROOT / ".github/workflows/release-readiness.yml").read_text()
        check = workflow.split("\n  release-check:\n", 1)[1].split("\n  tag:\n", 1)[0]
        self.assertNotIn("ffmpeg-6", check)
        self.assertIn("runs-on: [self-hosted, Linux, X64, lab, unit-memory-16g]", check)
        saved = check.index('cp scripts/release-readiness "$RUNNER_TEMP/release-readiness"')
        checkout = check.index('git checkout -q --detach "$RELEASE_SHA"')
        self.assertLess(saved, checkout)
        for command in ("make release-check", "scripts/validate run --profile ci --all --strict"):
            invocation = f'python3 "$RUNNER_TEMP/release-readiness" {command}'
            self.assertIn(invocation, check)
            self.assertLess(checkout, check.index(invocation))
        self.assertIn("lane: release-readiness", check)
        self.assertIn("uses: ./.github/actions/cargo-cache-finalize", check)
        dockerfile = (ROOT / "Dockerfile").read_text()
        self.assertIn("FROM runtime-assets AS ci", dockerfile)
        self.assertIn("FROM runtime-assets AS runtime", dockerfile)

    def test_ci_tools_use_the_media_stage_selected_by_publication(self):
        source = (ROOT / "Dockerfile").read_text()
        generated = readiness.ci_dockerfile(source)
        self.assertEqual(readiness.media_target(source), "runtime-assets")
        self.assertIn("FROM runtime-assets AS ci\n", generated)
        self.assertIn("COPY --from=ci-rust-toolchain /usr/local/cargo/bin /usr/local/cargo/bin", generated)
        self.assertNotIn("AS linux-dolby-package-export", generated)
        # Older candidate tags still select their audited installer stage.
        historical = source + "\nFROM scratch AS linux-dolby-package-export\nFROM runtime-assets AS runtime-assets-dolby-amd64\n"
        self.assertEqual(readiness.media_target(historical), "runtime-assets-dolby-amd64")
        historical_generated = readiness.ci_dockerfile(historical)
        self.assertGreater(historical_generated.index("FROM runtime-assets-dolby-amd64 AS ci"),
                           historical_generated.index("FROM runtime-assets AS runtime-assets-dolby-amd64"))
        self.assertIn("sha256sum -c /usr/lib/jellyfin-ffmpeg/provenance/shipping-system-elf-sha256.txt",
                      readiness.BOOTSTRAP)

    def test_media_registry_prefers_explicit_override_without_reading_origin(self):
        with patch.dict(os.environ, {"PLURX_MEDIA_REGISTRY": "mirror.example:5000"}, clear=True), \
             patch.object(readiness.subprocess, "check_output") as origin:
            self.assertEqual(readiness.media_registry(), "mirror.example:5000")
            origin.assert_not_called()

    def test_media_registry_strips_origin_credentials_and_repository_path(self):
        with patch.dict(os.environ, {}, clear=True), \
             patch.object(readiness.subprocess, "check_output",
                          return_value="https://operator:private-token@forge.example:3000/noirr/plurx.git\n"):
            self.assertEqual(readiness.media_registry(), "forge.example:3000")

    def test_media_registry_falls_back_for_ssh_or_unavailable_origins(self):
        with patch.dict(os.environ, {}, clear=True):
            for origin in ("operator@forge.example:noirr/plurx.git", "ssh://operator@forge.example/noirr/plurx.git",
                           "https://forge.example:invalid/noirr/plurx.git"):
                with self.subTest(origin=origin), \
                     patch.object(readiness.subprocess, "check_output", return_value=origin):
                    self.assertEqual(readiness.media_registry(), "forge.lan:3000")
            with patch.object(readiness.subprocess, "check_output",
                              side_effect=subprocess.CalledProcessError(2, ["git"])):
                self.assertEqual(readiness.media_registry(), "forge.lan:3000")

    def test_image_is_built_from_checkout_and_run_by_immutable_id(self):
        image = "sha256:" + "a" * 64
        calls = []
        def build(args):
            calls.append(args)
            if args[:3] == ["docker", "buildx", "build"]:
                self.assertIn("linux/amd64", args)
                self.assertIn("--load", args)
                self.assertEqual(args[-1], ".")
                if "--file" in args:
                    self.assertIn("PLURX_CI_SOURCE_SHA=candidate-sha", args)
                    self.assertIn("FROM runtime-assets AS ci\n",
                                  Path(args[args.index("--file") + 1]).read_text())
                    self.assertNotIn("--build-context", args)
                Path(args[args.index("--iidfile") + 1]).write_text(image)
            return 0
        with patch.object(readiness.subprocess, "check_output", return_value="candidate-sha\n"), \
             patch.object(readiness, "run_owned", side_effect=build), \
             patch.object(readiness.subprocess, "run") as cleanup:
            self.assertEqual(readiness.build_image(), image)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][calls[0].index("--target") + 1], "ci")
        cleanup.assert_not_called()

    def test_missing_immutable_image_id_refuses_execution(self):
        def bad_build(args):
            if "--iidfile" in args:
                value = "moving-image:latest" if "--file" in args else "sha256:" + "a" * 64
                Path(args[args.index("--iidfile") + 1]).write_text(value)
        with patch.object(readiness.subprocess, "check_output", return_value="candidate\n"), \
             patch.object(readiness, "run_owned", side_effect=bad_build), \
             patch.object(readiness.subprocess, "run"):
            with self.assertRaisesRegex(ValueError, "immutable CI image ID"):
                readiness.build_image()

    def test_android_mounts_host_paths_and_checks_cannot_inherit_wrong_ffmpeg_or_credentials(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            (root / "Cargo.toml").touch()
            (root / "Dockerfile").write_text((ROOT / "Dockerfile").read_text())
            (root / ".git").mkdir()
            docker, buildx = root / "docker", root / "docker-buildx"
            docker.touch(); buildx.touch()
            env = {
                "RUNNER_TEMP": str(root / "tmp"),
                "CARGO_HOME": str(root / "cargo"),
                "CARGO_TARGET_DIR": str(root / "build"),
                "PLURX_FFMPEG": "/wrong/ffmpeg6", "PATH": "/wrong/bin",
                "CI_REGISTRY_PULL_TOKEN": "never-forward-this", "GITHUB_TOKEN": "nor-this",
                "GITHUB_SHA": "candidate", "RUST_TEST_THREADS": "2",
                "PLURX_LIVE_TV_DEVICE": "http://tuner.test",
                "PLURX_LIVE_TV_TUNERS": "2", "PLURX_SKIP_UI_BASELINE": "1",
            }
            with patch.dict(os.environ, env, clear=True), \
                 patch.object(Path, "cwd", return_value=root), \
                 patch.object(Path, "is_socket", return_value=True), \
                 patch.object(readiness.shutil, "which", return_value=str(docker)), \
                 patch.object(readiness.subprocess, "check_output", return_value=json.dumps([
                     {"Name": "buildx", "Path": str(buildx)},
                 ])):
                args = readiness.container_command("sha256:" + "a" * 64, ["make", "release-check"], "test-release", host_network=True, memory_gb=8)
            mounts = [args[i + 1] for i, value in enumerate(args) if value == "--mount"]
            self.assertIn(f"type=bind,source={root},target={root}", mounts)
            for path in (root / "cargo", root / "build", root / "tmp" / "shipped-runtime-tmp"):
                self.assertIn(f"type=bind,source={path},target={path}", mounts)
            self.assertIn(f"type=bind,source={docker},target=/usr/local/bin/docker,readonly", mounts)
            self.assertIn(f"type=bind,source={buildx},target=/usr/local/lib/docker/cli-plugins/docker-buildx,readonly", mounts)
            self.assertNotIn("--privileged", args)
            self.assertEqual(args[args.index("--memory") + 1], "8g")
            self.assertEqual(args[args.index("--memory-swap") + 1], "8g")
            self.assertEqual(args[args.index("--network") + 1], "host")
            settings = dict(args[i + 1].split("=", 1) for i, value in enumerate(args) if value == "--env")
            self.assertEqual(settings["READINESS_HOST_NETWORK"], "1")
            self.assertEqual(settings["TMPDIR"], str(root / "tmp" / "shipped-runtime-tmp"))
            self.assertEqual(settings["PLURX_LIVE_TV_DEVICE"], "http://tuner.test")
            self.assertEqual(settings["PLURX_LIVE_TV_TUNERS"], "2")
            self.assertNotIn("PLURX_SKIP_UI_BASELINE", settings)
            self.assertNotIn("GITHUB_STEP_SUMMARY", settings)
            self.assertEqual(settings["PLURX_FFMPEG"], "/usr/lib/jellyfin-ffmpeg/ffmpeg")
            self.assertEqual(settings["PLURX_FFPROBE"], "/usr/lib/jellyfin-ffmpeg/ffprobe")
            self.assertEqual(settings["PLURX_BOUND_FFPROBE"], "/usr/local/lib/plurx/ffprobe")
            self.assertEqual(settings["PLURX_DOVI_TOOL"], "/usr/local/bin/dovi_tool")
            self.assertEqual(settings["PLURX_MKVMERGE"], "/usr/bin/mkvmerge")
            self.assertEqual(settings["READINESS_UID"], str(os.getuid()))
            self.assertEqual(settings["GITHUB_SHA"], "candidate")
            self.assertNotIn("PATH", settings)
            self.assertNotIn("CI_REGISTRY_PULL_TOKEN", settings)
            self.assertNotIn("GITHUB_TOKEN", settings)
            self.assertNotIn("never-forward-this", " ".join(args))
            self.assertEqual(args[-2:], ["make", "release-check"])
            self.assertIn('exec setpriv --reuid "$READINESS_UID"', readiness.BOOTSTRAP)
            self.assertIn('test "$(readlink -f "$(command -v ffmpeg)")" = "$PLURX_FFMPEG"', readiness.BOOTSTRAP)
            self.assertIn('test "$(readlink -f "$(command -v ffprobe)")" = "$PLURX_FFPROBE"', readiness.BOOTSTRAP)

    def test_nightly_options_reach_container_without_consuming_command_flags(self):
        with patch.object(readiness, "build_image", return_value="sha256:" + "a" * 64), \
             patch.object(readiness, "container_command", return_value=["docker", "run"]) as command, \
             patch.object(readiness.signal, "signal"), \
             patch.object(readiness, "run_owned", return_value=0), \
             patch.object(readiness.subprocess, "run"):
            self.assertEqual(readiness.main(["--memory-gb", "8", "--host-network", "make", "--quiet", "validate-nightly"]), 0)
            self.assertEqual(command.call_args.args[1], ["make", "--quiet", "validate-nightly"])
            self.assertEqual(command.call_args.kwargs, {"host_network": True, "memory_gb": 8})

    def test_check_failure_is_preserved_and_container_is_removed(self):
        with patch.object(readiness, "build_image", return_value="sha256:" + "a" * 64), \
             patch.object(readiness, "container_command", return_value=["docker", "run"]), \
             patch.object(readiness.signal, "signal"), \
             patch.object(readiness, "run_owned", return_value=37), \
             patch.object(readiness.subprocess, "run") as cleanup:
            self.assertEqual(readiness.main(["make", "release-check"]), 37)
            self.assertEqual(cleanup.call_args.args[0][:3], ["docker", "rm", "--force"])

    def test_cancellation_removes_the_running_container(self):
        with patch.object(readiness, "build_image", return_value="sha256:" + "a" * 64), \
             patch.object(readiness, "container_command", return_value=["docker", "run"]), \
             patch.object(readiness.signal, "signal"), \
             patch.object(readiness, "run_owned", side_effect=KeyboardInterrupt), \
             patch.object(readiness.subprocess, "run") as cleanup:
            with self.assertRaises(KeyboardInterrupt):
                readiness.main(["make", "release-check"])
            self.assertEqual(cleanup.call_args.args[0][:3], ["docker", "rm", "--force"])

    def test_cancellation_terminates_and_reaps_owned_build_client(self):
        with patch.object(readiness.subprocess, "Popen") as popen:
            child = popen.return_value.__enter__.return_value
            child.wait.side_effect = [SystemExit(143), 0]
            with self.assertRaises(SystemExit):
                readiness.run_owned(["docker", "buildx", "build"])
            child.terminate.assert_called_once()
            self.assertEqual(child.wait.call_count, 2)


if __name__ == "__main__":
    unittest.main()
