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

from validation import mobile_build
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
        # Both manifests carry the development build, as the tree does.
        self.commit("clients/apple/project.yml", 'CURRENT_PROJECT_VERSION: "1"\n')
        self.commit("clients/android/app/build.gradle.kts", "versionCode = 1\n")
        self.base = self.commit("README.md")
        # Fixture histories are a handful of commits; the real floor is the
        # last number the stores hold.
        floor = patch.dict(mobile_build.LAST_CLAIMED, {"apple": 0, "android": 0})
        floor.start()
        self.addCleanup(floor.stop)
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

    def change(self, platform: str, revision: int) -> str:
        """A client source change. No commit in these tests claims a number."""
        if platform == "apple":
            return self.commit("clients/apple/Sources/Shared/Player.swift", f"let revision = {revision}\n")
        return self.commit("clients/android/app/src/main/Player.kt", f"val revision = {revision}\n")

    def pin_git(self, pin: Path, *args: str) -> str:
        return subprocess.check_output(["git", "-C", str(pin), *args], text=True).strip()

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
        self.assertEqual(
            changed_platforms(self.source, "0" * 40, self.base, initial_push=True),
            ["ios", "tvos", "android"],
        )
        self.run_git("rm", "-q", "-r", "clients/android")
        self.run_git("commit", "-m", "drop the Android client")
        head = self.run_git("rev-parse", "HEAD")
        self.assertEqual(changed_platforms(self.source, "0" * 40, head, initial_push=True), ["ios", "tvos"])
        with self.assertRaises(ValueError):
            changed_platforms(self.source, "--option", self.base)

    def test_queued_release_includes_newer_main_and_enforces_internal_track(self) -> None:
        trigger = self.change("apple", 2)
        self.change("android", 2)
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
        # The controller is given the newest main plus the build-number stamp.
        stamped = self.pin_git(pin, "rev-parse", "main")
        self.assertEqual(self.pin_git(pin, "rev-parse", "main^"), head)
        self.assertEqual(variables["mobile_release_expected_commits"], {"cinema": stamped})
        # Receipts get the commit on main as well as the stamp made from it.
        self.assertEqual(variables["mobile_release_source_commits"], {"cinema": head})
        build = int(self.run_git("rev-list", "--count", head))
        self.assertEqual(
            self.pin_git(pin, "show", "main:clients/android/app/build.gradle.kts"),
            f"versionCode = {build}",
        )
        self.assertEqual(
            self.pin_git(pin, "show", "main:clients/apple/project.yml"),
            f'CURRENT_PROJECT_VERSION: "{build}"',
        )
        self.assertEqual(variables["mobile_release_build_numbers"], {"cinema": build})
        ledger = [
            json.loads(line)
            for line in (self.root / "state" / mobile_build.LEDGER).read_text().splitlines()
        ]
        self.assertEqual([row["platform"] for row in ledger], ["ios", "tvos", "android"])
        for row in ledger:
            self.assertEqual(
                (row["build"], row["source_commit"], row["stamp_commit"]), (build, head, stamped)
            )

    def test_store_failure_is_not_reported_as_success(self) -> None:
        head = self.change("android", 2)
        with patch.dict(os.environ, {"PLAY_EXIT": "9"}):
            with self.assertRaises(subprocess.CalledProcessError):
                release(self.base, head)

    def test_apple_only_update_never_builds_or_submits_android(self) -> None:
        self.commit("clients/apple/Sources/Shared/Player.swift")
        head = self.change("apple", 2)
        release(self.base, head)
        command = json.loads((self.root / "command.json").read_text())
        self.assertEqual(json.loads(command[-1])["mobile_release_channels_platforms"], ["ios", "tvos"])

    def test_android_only_update_never_builds_or_submits_apple(self) -> None:
        self.commit("clients/android/app/src/main/AndroidManifest.xml")
        head = self.change("android", 2)
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

    def test_every_client_change_publishes_without_a_claimed_number(self) -> None:
        # The old guard skipped a platform whose sources changed after its
        # number was last claimed, until some later commit claimed one. No
        # commit claims a number now, and each push publishes what it changed.
        first = self.change("apple", 1)
        self.assertEqual(scope_platforms(self.source, self.base, first), ["ios", "tvos"])
        second = self.change("apple", 2)
        self.assertEqual(scope_platforms(self.source, first, second), ["ios", "tvos"])
        release(first, second)
        variables = json.loads(json.loads((self.root / "command.json").read_text())[-1])
        self.assertEqual(variables["mobile_release_channels_platforms"], ["ios", "tvos"])

    def test_each_release_is_stamped_with_a_higher_number_than_the_last(self) -> None:
        builds = []
        previous = self.base
        for revision in (1, 2, 3):
            head = self.change("android", revision)
            release(previous, head)
            variables = json.loads(json.loads((self.root / "command.json").read_text())[-1])
            pin = Path(variables["mobile_release_repositories"]["cinema"]["source_path"])
            builds.append(int(self.pin_git(
                pin, "show", "main:clients/android/app/build.gradle.kts").split("=")[1]))
            previous = head
        self.assertEqual(builds, sorted(set(builds)))

    def test_a_number_at_or_below_the_last_published_one_is_refused(self) -> None:
        head = self.change("android", 1)
        release(self.base, head)
        (self.root / "command.json").unlink()
        # The same commit again: the same number, already published.
        with self.assertRaisesRegex(ValueError, "not above android build"):
            release(self.base, head)
        self.assertFalse((self.root / "command.json").exists())
        # The floor is the forge's, not this host's: with the state directory
        # gone (a new controller), the same commit is still refused.
        import shutil
        shutil.rmtree(self.root / "state")
        with self.assertRaisesRegex(ValueError, "per the forge"):
            release(self.base, head)
        self.assertEqual(self.run_git("rev-parse", "refs/cinema-published/android"), head)
        # Apple has published nothing yet, so the same commit may still go.
        apple = self.change("apple", 1)
        release(head, apple)
        # A rewritten main with fewer commits than the published one.
        self.run_git("reset", "-q", "--hard", self.base)
        rewritten = self.change("android", 2)
        (self.root / "command.json").unlink()
        with self.assertRaisesRegex(ValueError, "not above android build"):
            release(self.base, rewritten)
        self.assertFalse((self.root / "command.json").exists())

    def test_a_failed_upload_records_nothing_and_may_be_retried(self) -> None:
        head = self.change("android", 1)
        with patch.dict(os.environ, {"PLAY_EXIT": "9"}):
            with self.assertRaises(subprocess.CalledProcessError):
                release(self.base, head)
        self.assertFalse((self.root / "state" / mobile_build.LEDGER).exists())
        release(self.base, head)
        self.assertTrue((self.root / "state" / mobile_build.LEDGER).is_file())

    def test_a_comment_only_client_change_publishes_nothing(self) -> None:
        self.commit("clients/android/app/src/main/Player.kt", "val revision = 1\n")
        before = self.run_git("rev-parse", "HEAD")
        head = self.commit("clients/android/app/src/main/Player.kt", "// A note.\nval revision = 1\n")
        self.assertEqual(scope_platforms(self.source, before, head), [])

    def test_a_claimed_number_in_the_tree_refuses_the_release(self) -> None:
        head = self.commit("clients/android/app/build.gradle.kts", "versionCode = 208\n")
        with self.assertRaisesRegex(ValueError, "already stamped or was edited by hand"):
            release(self.base, head)
        self.assertFalse((self.root / "command.json").exists())

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
        head = self.change("android", 2)
        release(self.base, head)
        before, after, built = json.loads((self.root / "command.json").read_text())
        self.assertEqual(before, after)
        self.assertEqual(built, before)
        pin = self.root / "state" / "pinned-source.git"
        self.assertEqual(self.pin_git(pin, "rev-parse", f"{built}^"), head)
        self.assertNotEqual(self.run_git("rev-parse", "HEAD"), head)
        # A later release re-pins to the newer main.
        newer = self.change("android", 3)
        release(head, newer)
        repinned = json.loads((self.root / "command.json").read_text())[0]
        self.assertEqual(self.pin_git(pin, "rev-parse", f"{repinned}^"), newer)

    def test_pin_drops_refs_left_by_an_earlier_run(self) -> None:
        state = self.root / "state"
        state.mkdir()
        first = self.change("android", 2)
        pin, _ = mobile_build.release_source(self.source, state, first, 7)
        subprocess.run(["git", "-C", str(pin), "update-ref", "refs/remotes/origin/stale", first], check=True)
        second = self.change("android", 3)
        _, stamped = mobile_build.release_source(self.source, state, second, 8)
        refs = subprocess.check_output(["git", "-C", str(pin), "for-each-ref", "--format=%(refname)"], text=True).split()
        self.assertEqual(refs, ["refs/heads/main"])
        self.assertEqual(self.pin_git(pin, "rev-parse", "main"), stamped)
        self.assertEqual(self.pin_git(pin, "rev-parse", "main^"), second)

    def test_rewritten_history_does_not_publish_an_unrelated_main(self) -> None:
        trigger = self.commit("clients/apple/project.yml")
        self.run_git("reset", "--hard", self.base)
        with self.assertRaises(subprocess.CalledProcessError):
            release(self.base, trigger)
        self.assertFalse((self.root / "command.json").exists())


class EveryBuildPathStampsCase(unittest.TestCase):
    """No path builds, uploads or installs the tree's development build.

    The tree carries build 1. `validation/mobile_build.py` is the one place a
    build tree comes from, and each path below goes through it; the Ansible
    plays refuse a tree without a number (`noirr_mobile_metadata`, in the
    Ansible repository).
    """

    ROOT = Path(__file__).resolve().parents[2]

    def read(self, path: str) -> str:
        return (self.ROOT / path).read_text(encoding="utf-8")

    def test_ship_hands_ansible_a_stamped_checkout_never_the_working_tree(self) -> None:
        ship = self.read("scripts/ship")
        self.assertIn('python3 -m validation.mobile_build "${STAMP_ARGS[@]}"', ship)
        self.assertIn('STAMP_ARGS=(checkout --repo "$ROOT" --commit HEAD --fetch)', ship)
        # ...and records the upload in the forge, where every host reads the floor.
        self.assertIn('mobile_build record "$MOBILE_REPO" --repo "$ROOT" --platforms ios tvos', ship)
        self.assertLess(ship.index('run "${MOBILE_ARGS[@]}"'), ship.index("mobile_build record"))
        self.assertIn('-e "plurx_repo=$MOBILE_REPO"', ship)
        self.assertNotIn('plurx_repo=$ROOT"', ship)
        # A TestFlight upload is a store upload: main only.
        self.assertIn('[ "$DO_APPLE" -eq 1 ] && STAMP_ARGS+=(--store ios tvos)', ship)

    def test_ship_physical_builds_only_the_stamped_checkout(self) -> None:
        script = self.read("scripts/ship-physical")
        self.assertIn('checkout --repo "$SOURCE_REPO" --commit "$COMMIT" --state "$RELEASE_ROOT"', script)
        self.assertIn('require-stamped "$WORKTREE"', script)
        self.assertNotIn('worktree add', script)
        self.assertLess(script.index('require-stamped "$WORKTREE"'), script.index("xcodegen generate"))

    def test_the_release_job_takes_its_tree_number_and_floor_from_mobile_build(self) -> None:
        channels = self.read("validation/mobile_channels.py")
        for call in ("mobile_build.require_main(", "mobile_build.build_number(",
                     "mobile_build.require_above_published(", "mobile_build.release_source(",
                     "mobile_build.record_published("):
            self.assertIn(call, channels)
        self.assertLess(channels.index("mobile_build.require_above_published("),
                        channels.index('"ansible-playbook"'))
        self.assertLess(channels.index("subprocess.run(command, cwd=ansible.parent, check=True)"),
                        channels.index("mobile_build.record_published("))

    def test_no_other_script_or_recipe_runs_a_release_build(self) -> None:
        """A new path that assembles a release would bypass the number."""
        owners = {"Makefile", "scripts/ship-physical"}
        found = set()
        for path in [self.ROOT / "Makefile", *sorted((self.ROOT / "scripts").iterdir())]:
            if not path.is_file():
                continue
            try:
                text = path.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                continue
            if ":app:assembleRelease" in text or ":app:bundleRelease" in text:
                found.add(str(path.relative_to(self.ROOT)))
        self.assertEqual(found - owners, set(), "these build a release without a build number")
        self.assertIn("-PplurxBuildNumber=$$build :app:assembleRelease", self.read("Makefile"))


if __name__ == "__main__":
    unittest.main()
