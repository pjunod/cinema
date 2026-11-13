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

from validation.mobile_channels import (
    changed_platforms,
    main,
    platforms_for_paths,
    release,
    scope_platforms,
)


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

    def commit(self, path: str, text: str | None = None) -> str:
        target = self.source / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(path if text is None else text)
        self.run_git("add", ".")
        self.run_git("commit", "-m", "fixture")
        return self.run_git("rev-parse", "HEAD")

    def bump(self, platform: str, build: int) -> str:
        if platform == "apple":
            return self.commit("clients/apple/project.yml", f'CURRENT_PROJECT_VERSION: "{build}"\n')
        return self.commit("clients/android/app/build.gradle.kts", f"versionCode = {build}\n")

    def scope(self, base: str, head: str) -> tuple[list[str], str]:
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            platforms = scope_platforms(self.source, base, head)
        return platforms, err.getvalue()

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
        self.assertEqual(changed_platforms(self.source, "0" * 40, self.base, initial_push=True), [])
        head = self.commit("clients/apple/Sources/Shared/Player.swift")
        self.assertEqual(changed_platforms(self.source, "0" * 40, head, initial_push=True), ["ios", "tvos"])
        with self.assertRaises(ValueError):
            changed_platforms(self.source, "--option", self.base)

    def test_queued_release_includes_newer_main_and_enforces_internal_track(self) -> None:
        trigger = self.bump("apple", 2)
        self.bump("android", 2)
        with patch.dict(os.environ, {"PLURX_MOBILE_VARS_FILE": str(self.root / "local vars.yml")}):
            release(self.base, trigger)
        command = json.loads((self.root / "command.json").read_text())
        variables = json.loads(command[-1])
        self.assertEqual(variables["mobile_release_channels_apps"], ["cinema"])
        self.assertEqual(variables["mobile_release_channels_platforms"], ["ios", "tvos", "android"])
        self.assertEqual(variables["mobile_release_channels_android_track"], "internal")
        self.assertIs(variables["mobile_release_channels_upload"], True)
        self.assertEqual(command[-4:-2], ["-e", "@" + str(self.root / "local vars.yml")])
        head = self.run_git("rev-parse", "HEAD")
        pin = Path(variables["mobile_release_repositories"]["cinema"]["source_path"])
        self.assertEqual(pin, self.root / "state" / "pinned-source.git")
        self.assertEqual(subprocess.check_output(["git", "-C", str(pin), "rev-parse", "main"], text=True).strip(), head)
        self.assertEqual(variables["mobile_release_expected_commits"], {"cinema": head})

    def test_store_failure_is_not_reported_as_success(self) -> None:
        head = self.bump("android", 2)
        with patch.dict(os.environ, {"PLAY_EXIT": "9"}):
            with self.assertRaises(subprocess.CalledProcessError):
                release(self.base, head)

    def test_apple_only_update_never_builds_or_submits_android(self) -> None:
        self.commit("clients/apple/Sources/Shared/Player.swift")
        head = self.bump("apple", 2)
        release(self.base, head)
        command = json.loads((self.root / "command.json").read_text())
        self.assertEqual(json.loads(command[-1])["mobile_release_channels_platforms"], ["ios", "tvos"])

    def test_android_only_update_never_builds_or_submits_apple(self) -> None:
        self.commit("clients/android/app/src/main/AndroidManifest.xml")
        head = self.bump("android", 2)
        release(self.base, head)
        command = json.loads((self.root / "command.json").read_text())
        self.assertEqual(json.loads(command[-1])["mobile_release_channels_platforms"], ["android"])

    def test_manual_retry_requires_a_real_change_baseline(self) -> None:
        with self.assertRaisesRegex(ValueError, "full commit SHAs"):
            release("", self.base)
        with patch.dict(os.environ, {"GITHUB_EVENT_NAME": "workflow_dispatch"}):
            with self.assertRaisesRegex(ValueError, "real ancestor commit"):
                release("0" * 40, self.base)
        self.assertFalse((self.root / "command.json").exists())

    def test_manual_scope_rejects_the_initial_push_zero_sha(self) -> None:
        with patch.dict(os.environ, {"GITHUB_EVENT_NAME": "workflow_dispatch"}), patch(
            "sys.argv", ["mobile_channels", "scope", "--base", "0" * 40, "--head", self.base]
        ), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(main(), 1)

    def test_non_ancestor_baseline_never_invokes_the_publisher(self) -> None:
        other = self.commit("clients/apple/project.yml")
        self.run_git("reset", "--hard", self.base)
        head = self.commit("docs/PUBLISHING.md")
        with self.assertRaises(subprocess.CalledProcessError):
            release(other, head)
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

    def test_client_change_without_a_bump_is_skipped_not_failed(self) -> None:
        bumped = self.bump("apple", 2)
        self.bump("android", 2)
        head = self.commit("clients/apple/Sources/Shared/Player.swift")
        platforms, warning = self.scope(bumped, head)
        self.assertEqual(platforms, ["android"])
        self.assertIn("skipping Cinema apple", warning)
        self.assertIn("clients/apple/Sources/Shared/Player.swift", warning)
        platforms, _ = self.scope(self.base, head)
        self.assertEqual(platforms, ["android"])
        only_apple = self.commit("clients/apple/Sources/Shared/Other.swift")
        with contextlib.redirect_stderr(io.StringIO()):
            release(head, only_apple)
        self.assertFalse((self.root / "command.json").exists())

    def test_merged_bump_counts_even_when_older_than_the_branch_sources(self) -> None:
        # The controller guard took the last non-merge bump commit, so a
        # branch that bumped early and edited sources afterwards was refused
        # after it merged. Main's first-parent history sees one merge.
        self.bump("apple", 2)
        main_tip = self.run_git("rev-parse", "HEAD")
        self.run_git("switch", "-q", "-c", "feature")
        self.bump("apple", 3)
        self.commit("clients/apple/Sources/Shared/Player.swift")
        self.run_git("switch", "-q", "main")
        self.commit("docs/PUBLISHING.md")
        self.run_git("merge", "--no-ff", "-q", "-m", "merge feature", "feature")
        merged = self.run_git("rev-parse", "HEAD")
        platforms, warning = self.scope(main_tip, merged)
        self.assertEqual((platforms, warning), (["ios", "tvos"], ""))
        later = self.commit("clients/apple/Resources/Strings.txt")
        self.assertEqual(self.scope(merged, later)[0], [])

    def test_merges_during_the_build_do_not_move_the_published_commit(self) -> None:
        # The controller fetches origin/main before the build and again before
        # publishing and refuses if they differ; the pin keeps them equal.
        (self.bin / "ansible-playbook").write_text(
            "#!/usr/bin/env python3\nimport json, os, subprocess, sys\n"
            "from pathlib import Path\n"
            "repo = json.loads(sys.argv[-1])['mobile_release_repositories']['cinema']['source_path']\n"
            "def main():\n"
            "    subprocess.run(['git', '-C', repo, 'fetch', '-q', 'origin', '+refs/heads/main:refs/remotes/origin/main'], check=True)\n"
            "    return subprocess.check_output(['git', '-C', repo, 'rev-parse', 'origin/main'], text=True).strip()\n"
            "before = main()\n"
            "tree = os.environ['CAPTURE'] + '.' + before[:12]\n"
            "subprocess.run(['git', '-C', repo, 'worktree', 'add', '-q', '--detach', tree, before], check=True)\n"
            "src = os.environ['PLURX_MOBILE_SOURCE_REPO']\n"
            "subprocess.run(['git', '-C', src, 'commit', '-q', '--allow-empty', '-m', 'merged meanwhile'], check=True)\n"
            "after = main()\n"
            "head = subprocess.check_output(['git', '-C', tree, 'rev-parse', 'HEAD'], text=True).strip()\n"
            "Path(os.environ['CAPTURE']).write_text(json.dumps([before, after, head]))\n"
        )
        head = self.bump("android", 2)
        release(self.base, head)
        before, after, built = json.loads((self.root / "command.json").read_text())
        self.assertEqual((before, after, built), (head, head, head))
        self.assertNotEqual(self.run_git("rev-parse", "HEAD"), head)
        # A later release re-pins to the newer main.
        newer = self.bump("android", 3)
        release(head, newer)
        self.assertEqual(json.loads((self.root / "command.json").read_text())[0], newer)

    def test_rewritten_history_does_not_publish_an_unrelated_main(self) -> None:
        trigger = self.commit("clients/apple/project.yml")
        self.run_git("reset", "--hard", self.base)
        with self.assertRaises(subprocess.CalledProcessError):
            release(self.base, trigger)
        self.assertFalse((self.root / "command.json").exists())


if __name__ == "__main__":
    unittest.main()
