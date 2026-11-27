"""scripts/fleet-image: build the plurxd image once per commit, pull it elsewhere."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "fleet-image"
SHA = "a" * 40
IMAGE_ID = "sha256:" + "3" * 64
REF = f"forge.lan:3000/noirr/plurxd:build-{SHA}-amd64"

GIT_STUB = """#!/bin/sh
case "$*" in
  "rev-parse HEAD") echo "$STUB_HEAD" ;;
  status*) [ "$STUB_DIRTY" = 1 ] && echo "?? stray.txt"; exit 0 ;;
  *) exit 99 ;;
esac
"""

DOCKER_STUB = """#!/bin/sh
printf '%s\\n' "$*" >> "$STUB_LOG"
case "$*" in
  pull*) [ "$STUB_PULL_EXIT" = 0 ] || { echo "$STUB_PULL_ERROR" >&2; exit 1; } ;;
  *"compose config --images plurxd"*) echo plurx/plurxd:latest ;;
  *"{{.Id}}"*) echo "$STUB_IMAGE_ID" ;;
  *org.opencontainers.image.revision*) echo "$STUB_REVISION" ;;
  *"{{.Os}}/{{.Architecture}}"*) echo "$STUB_PLATFORM" ;;
  tag*) ;;
  push*) [ "$STUB_PUSH_EXIT" = 0 ] || { echo "denied: requested access" >&2; exit 1; } ;;
  *) exit 98 ;;
esac
"""


class FleetImageCase(unittest.TestCase):
    def run_script(self, action: str, **overrides: str) -> tuple[subprocess.CompletedProcess[str], list[str]]:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            for name, body in (("git", GIT_STUB), ("docker", DOCKER_STUB)):
                stub = directory / name
                stub.write_text(body, encoding="utf-8")
                stub.chmod(0o755)
            log = directory / "docker.log"
            environment = dict(
                os.environ,
                PATH=f"{directory}:{os.environ['PATH']}",
                PLURX_REGISTRY="forge.lan:3000",
                DOCKER_DEFAULT_PLATFORM="linux/amd64",
                STUB_LOG=str(log),
                STUB_HEAD=SHA,
                STUB_DIRTY="0",
                STUB_PULL_EXIT="0",
                STUB_PULL_ERROR="",
                STUB_IMAGE_ID=IMAGE_ID,
                STUB_REVISION=SHA,
                STUB_PLATFORM="linux/amd64",
                STUB_PUSH_EXIT="0",
            )
            environment.update(overrides)
            result = subprocess.run(
                ["python3", str(SCRIPT), action],
                cwd=ROOT,
                env=environment,
                text=True,
                capture_output=True,
                check=False,
            )
            calls = log.read_text(encoding="utf-8").splitlines() if log.exists() else []
        return result, calls

    def test_pull_returns_the_verified_shared_image(self):
        result, calls = self.run_script("pull")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), IMAGE_ID)
        self.assertIn(f"pull --platform linux/amd64 {REF}", calls)

    def test_pull_declines_an_image_that_is_not_this_commit_or_architecture(self):
        for overrides in ({"STUB_REVISION": "b" * 40}, {"STUB_PLATFORM": "linux/arm64"}):
            with self.subTest(**overrides):
                result, _ = self.run_script("pull", **overrides)
                self.assertEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")

    def test_pull_builds_locally_when_nothing_is_shared_or_the_registry_is_down(self):
        for error in ("manifest unknown", "dial tcp: connection refused"):
            with self.subTest(error=error):
                result, _ = self.run_script("pull", STUB_PULL_EXIT="1", STUB_PULL_ERROR=error)
                self.assertEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")

    def test_unconfigured_or_dirty_checkouts_never_touch_the_registry(self):
        for overrides in ({"PLURX_REGISTRY": ""}, {"STUB_DIRTY": "1"}):
            with self.subTest(**overrides):
                for action in ("pull", "push"):
                    result, calls = self.run_script(action, **overrides)
                    self.assertEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, "")
                    self.assertEqual(calls, [])

    def test_push_shares_the_built_image_under_its_commit(self):
        result, calls = self.run_script("push")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"tag {IMAGE_ID} {REF}", calls)
        self.assertIn(f"push {REF}", calls)

    def test_push_never_fails_the_deploy(self):
        result, calls = self.run_script("push", STUB_PUSH_EXIT="1")
        self.assertEqual(result.returncode, 0)
        self.assertIn("other nodes will build it themselves", result.stderr)
        result, calls = self.run_script("push", STUB_REVISION="")
        self.assertEqual(result.returncode, 0)
        self.assertFalse(any(call.startswith("push") for call in calls))

    def test_registry_is_read_from_the_host_env_file(self):
        loader = importlib.machinery.SourceFileLoader("fleet_image", str(SCRIPT))
        spec = importlib.util.spec_from_loader("fleet_image", loader)
        module = importlib.util.module_from_spec(spec)
        loader.exec_module(module)
        with tempfile.TemporaryDirectory() as temporary:
            env_file = Path(temporary) / ".env"
            env_file.write_text(
                "# fleet\nPLURX_IMAGE=x\nexport PLURX_REGISTRY='forge.lan:3000'\n",
                encoding="utf-8",
            )
            self.assertEqual(module.env_file_value("PLURX_REGISTRY", env_file), "forge.lan:3000")
            self.assertEqual(module.env_file_value("MISSING", env_file), "")


if __name__ == "__main__":
    unittest.main()
