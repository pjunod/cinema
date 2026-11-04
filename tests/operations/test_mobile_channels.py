"""Exercise post-merge selection and the real controller command without stores."""

from __future__ import annotations

import fcntl
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from validation.mobile_channels import changed_platforms, main, platforms_for_paths, release


class MobileChannelsCase(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = self.root / "source"
        self.source.mkdir()
        self.run_git("init", "-b", "main")
        self.run_git("config", "user.name", "Test")
        self.run_git("config", "user.email", "test@example.invalid")
        self.run_git("remote", "add", "origin", str(self.source))
        self.base = self.commit("README.md")
        self.ansible = self.root / "ansible"
        self.ansible.mkdir()
        (self.ansible / "mobile-channels.yml").touch()
        (self.ansible / "inventory.yml").touch()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        fake = self.bin / "ansible-playbook"
        fake.write_text(
            "#!/usr/bin/env python3\nimport json, os, sys\n"
            "from pathlib import Path\n"
            "Path(os.environ['CAPTURE']).write_text(json.dumps(sys.argv[1:]))\n"
            "sys.exit(int(os.environ.get('PLAY_EXIT', '0')))\n"
        )
        fake.chmod(0o755)
        self.env = patch.dict(os.environ, {
            "PLURX_MOBILE_SOURCE_REPO": str(self.source),
            "PLURX_ANSIBLE_DIR": str(self.ansible),
            "PLURX_ANSIBLE_INVENTORY": str(self.ansible / "inventory.yml"),
            "PLURX_MOBILE_STATE_DIR": str(self.root / "state"),
            "PLURX_MOBILE_VARS_FILE": "",
            "CAPTURE": str(self.root / "command.json"),
            "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
        })
        self.env.start()
        self.addCleanup(self.env.stop)

    def run_git(self, *args: str) -> str:
        return subprocess.check_output(
            ["git", "-C", str(self.source), *args], text=True, stderr=subprocess.PIPE
        ).strip()

    def commit(self, path: str) -> str:
        target = self.source / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(path)
        self.run_git("add", ".")
        self.run_git("commit", "-m", "fixture")
        return self.run_git("rev-parse", "HEAD")

    def test_scope_uses_complete_push_range_and_ignores_tests_and_docs(self) -> None:
        self.commit("clients/apple/Sources/Shared/Player.swift")
        self.commit("clients/android/app/src/main/AndroidManifest.xml")
        head = self.commit("docs/PUBLISHING.md")
        self.assertEqual(changed_platforms(self.source, self.base, head), ["ios", "tvos", "android"])
        self.assertEqual(platforms_for_paths(("clients/apple/Tests/PlayerTests.swift", "docs/PUBLISHING.md")), [])

    def test_removed_or_renamed_mobile_source_still_selects_release(self) -> None:
        old = self.commit("clients/android/app/src/main/old.kt")
        self.run_git("mv", "clients/android/app/src/main/old.kt", "old.kt")
        self.run_git("commit", "-m", "move source out of app")
        self.assertEqual(changed_platforms(self.source, old, self.run_git("rev-parse", "HEAD")), ["android"])

    def test_build_metadata_selects_its_platform(self) -> None:
        self.assertEqual(platforms_for_paths(("clients/apple/project.yml",)), ["ios", "tvos"])
        self.assertEqual(platforms_for_paths(("clients/android/app/build.gradle.kts",)), ["android"])

    def test_first_push_selects_only_existing_mobile_sources_and_invalid_revision_fails(self) -> None:
        self.assertEqual(changed_platforms(self.source, "0" * 40, self.base), [])
        head = self.commit("clients/apple/Sources/Shared/Player.swift")
        self.assertEqual(changed_platforms(self.source, "0" * 40, head), ["ios", "tvos"])
        with self.assertRaises(ValueError):
            changed_platforms(self.source, "--option", self.base)

    def test_queued_release_includes_newer_main_and_enforces_internal_track(self) -> None:
        trigger = self.commit("clients/apple/project.yml")
        self.commit("clients/android/app/build.gradle.kts")
        with patch.dict(os.environ, {"PLURX_MOBILE_VARS_FILE": str(self.root / "local vars.yml")}):
            release(self.base, trigger)
        command = json.loads((self.root / "command.json").read_text())
        variables = json.loads(command[-1])
        self.assertEqual(variables["mobile_release_channels_apps"], ["cinema"])
        self.assertEqual(variables["mobile_release_channels_platforms"], ["ios", "tvos", "android"])
        self.assertEqual(variables["mobile_release_channels_android_track"], "internal")
        self.assertIs(variables["mobile_release_channels_upload"], True)
        self.assertEqual(command[-4:-2], ["-e", "@" + str(self.root / "local vars.yml")])
        self.assertEqual(variables["mobile_release_repositories"]["cinema"]["source_path"], str(self.source))

    def test_store_failure_is_not_reported_as_success(self) -> None:
        head = self.commit("clients/android/app/build.gradle.kts")
        with patch.dict(os.environ, {"PLAY_EXIT": "9"}):
            with self.assertRaises(subprocess.CalledProcessError):
                release(self.base, head)

    def test_apple_only_update_never_builds_or_submits_android(self) -> None:
        head = self.commit("clients/apple/Sources/Shared/Player.swift")
        release(self.base, head)
        command = json.loads((self.root / "command.json").read_text())
        self.assertEqual(json.loads(command[-1])["mobile_release_channels_platforms"], ["ios", "tvos"])

    def test_android_only_update_never_builds_or_submits_apple(self) -> None:
        head = self.commit("clients/android/app/src/main/AndroidManifest.xml")
        release(self.base, head)
        command = json.loads((self.root / "command.json").read_text())
        self.assertEqual(json.loads(command[-1])["mobile_release_channels_platforms"], ["android"])

    def test_manual_retry_requires_a_real_change_baseline(self) -> None:
        with self.assertRaisesRegex(ValueError, "full commit SHAs"):
            release("", self.base)
        self.assertFalse((self.root / "command.json").exists())

    def test_release_cli_refuses_non_main_and_pull_request_events(self) -> None:
        for ref, event in (("refs/heads/topic", "workflow_dispatch"), ("refs/heads/main", "pull_request")):
            with self.subTest(ref=ref, event=event), patch.dict(os.environ, {
                "GITHUB_REF": ref, "GITHUB_EVENT_NAME": event,
            }), patch("sys.argv", ["mobile_channels", "release", "--head", self.base]), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(main(), 1)
        self.assertFalse((self.root / "command.json").exists())

    def test_non_mobile_push_does_not_invoke_publisher(self) -> None:
        for path in ("docs/PUBLISHING.md", "clients/apple/Tests/PlayerTests.swift",
                     "clients/android/app/src/test/PlayerTest.kt", "crates/plurxd/src/main.rs"):
            with self.subTest(path=path):
                head = self.commit(path)
                release(self.base, head)
                self.assertFalse((self.root / "command.json").exists())

    def test_controller_lock_prevents_overlapping_publishers(self) -> None:
        state = self.root / "state"
        state.mkdir()
        with (state / "publish.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaisesRegex(ValueError, "another mobile"):
                release(self.base, self.base)
        self.assertFalse((self.root / "command.json").exists())

    def test_rewritten_history_does_not_publish_an_unrelated_main(self) -> None:
        trigger = self.commit("clients/apple/project.yml")
        self.run_git("reset", "--hard", self.base)
        with self.assertRaises(subprocess.CalledProcessError):
            release(self.base, trigger)
        self.assertFalse((self.root / "command.json").exists())


if __name__ == "__main__":
    unittest.main()
