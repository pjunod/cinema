"""The store build number is assigned at release, from the release commit."""

from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from validation import mobile_build
from validation.mobile_build import (
    ANDROID,
    APPLE,
    MobileBuildError,
    build_number,
    build_scope,
    declared_builds,
    forge_published,
    on_main,
    published,
    record_published,
    release_checkout,
    release_source,
    require_above_published,
    require_main,
    require_stamped,
    stamp_commit,
    stamp_manifests,
)

ANDROID_TEXT = 'android {\n    defaultConfig {\n        versionCode = 1\n        versionName = "0.8.0"\n    }\n}\n'
APPLE_TEXT = 'settings:\n  base:\n    MARKETING_VERSION: "0.8.0"\n    CURRENT_PROJECT_VERSION: "1"\n'


def git(root: Path, *arguments: str) -> str:
    return subprocess.run(
        ["git", "-C", str(root), *arguments], check=True, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    ).stdout.strip()


class MobileBuildCase(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve() / "source"
        self.root.mkdir()
        git(self.root, "init", "-q", "-b", "main")
        git(self.root, "config", "user.name", "Build Test")
        git(self.root, "config", "user.email", "build@example.invalid")
        self.write(ANDROID, ANDROID_TEXT)
        self.write(APPLE, APPLE_TEXT)
        self.base = self.commit("seed")
        # Fixture histories are a handful of commits; the real floor is the
        # last number the stores hold.
        floor = patch.dict(mobile_build.LAST_CLAIMED, {"apple": 0, "android": 0})
        floor.start()
        self.addCleanup(floor.stop)

    def write(self, path: str, text: str) -> None:
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def commit(self, message: str) -> str:
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "--allow-empty", "-m", message)
        self.sync_main()
        return git(self.root, "rev-parse", "HEAD")

    def sync_main(self) -> None:
        """`origin/main` follows the local `main`, as after a fetch."""
        git(self.root, "update-ref", "refs/remotes/origin/main", git(self.root, "rev-parse", "main"))

    def branch(self, name: str, path: str) -> None:
        git(self.root, "checkout", "-q", "-b", name, self.base)
        self.write(path, name)
        self.commit(f"change {name}")
        git(self.root, "checkout", "-q", "main")

    def merge(self, name: str) -> str:
        git(self.root, "merge", "-q", "--no-ff", "-m", f"merge {name}", name)
        self.sync_main()
        return git(self.root, "rev-parse", "HEAD")

    def test_two_client_branches_merge_in_either_order_and_every_release_rises(self):
        self.branch("apple-change", "clients/apple/Sources/A.swift")
        self.branch("android-change", "clients/android/app/src/main/B.kt")
        for order in (("apple-change", "android-change"), ("android-change", "apple-change")):
            with self.subTest(order=order):
                git(self.root, "reset", "-q", "--hard", self.base)
                self.sync_main()
                numbers = [build_number(self.root, self.base)]
                for name in order:
                    numbers.append(build_number(self.root, self.merge(name)))
                self.assertEqual(numbers, sorted(set(numbers)), "each release has a higher number")
                # Neither branch wrote a number, so there was nothing to re-claim.
                self.assertEqual((self.root / ANDROID).read_text(), ANDROID_TEXT)
                self.assertEqual((self.root / APPLE).read_text(), APPLE_TEXT)

    def test_the_number_is_a_property_of_the_commit(self):
        head = self.commit("one more")
        self.assertEqual(build_number(self.root, head), 2)
        self.assertEqual(build_number(self.root, head), build_number(self.root, "HEAD"))
        clone = self.root.parent / "clone"
        subprocess.run(["git", "clone", "-q", "-b", "main", str(self.root), str(clone)], check=True)
        self.assertEqual(build_number(clone, head), 2)

    def test_a_fast_forward_of_main_to_a_side_history_still_rises(self):
        # main fast-forwarded to a branch that merged main into itself: the
        # first-parent chain changes, the ancestor count still only grows.
        git(self.root, "checkout", "-q", "-b", "effort")
        self.commit("effort work")
        git(self.root, "checkout", "-q", "main")
        before = build_number(self.root, self.commit("main work"))
        git(self.root, "checkout", "-q", "effort")
        git(self.root, "merge", "-q", "--no-ff", "-m", "merge main into effort", "main")
        git(self.root, "checkout", "-q", "main")
        git(self.root, "merge", "-q", "--ff-only", "effort")
        self.sync_main()
        self.assertGreater(build_number(self.root, "main"), before)

    def test_a_shallow_clone_or_a_foreign_history_is_refused(self):
        self.commit("second")
        shallow = self.root.parent / "shallow"
        subprocess.run(
            ["git", "clone", "-q", "--depth", "1", self.root.as_uri(), str(shallow)], check=True
        )
        with self.assertRaisesRegex(MobileBuildError, "shallow"):
            build_number(shallow)
        with patch.dict(mobile_build.LAST_CLAIMED, {"apple": 301, "android": 207}):
            with self.assertRaisesRegex(MobileBuildError, "not above the last store build"):
                build_number(self.root)

    def test_stamping_replaces_only_the_development_number(self):
        android, apple = stamp_manifests(ANDROID_TEXT, APPLE_TEXT, 15679)
        self.assertEqual(android, ANDROID_TEXT.replace("versionCode = 1", "versionCode = 15679"))
        self.assertEqual(
            apple, APPLE_TEXT.replace('VERSION: "1"', 'VERSION: "15679"')
        )
        with self.assertRaisesRegex(MobileBuildError, "already stamped"):
            stamp_manifests(android, APPLE_TEXT, 15680)
        with self.assertRaisesRegex(MobileBuildError, "exactly one"):
            stamp_manifests("android {}\n", APPLE_TEXT, 15680)
        with self.assertRaisesRegex(MobileBuildError, "exactly one"):
            stamp_manifests(ANDROID_TEXT, APPLE_TEXT * 2, 15680)

    def test_a_branch_build_takes_the_number_of_the_main_commit_it_left(self):
        base_number = build_number(self.root, self.base)
        git(self.root, "checkout", "-q", "-b", "feature")
        for index in range(5):
            self.write("clients/android/app/src/main/F.kt", f"val f = {index}\n")
            git(self.root, "add", "-A")
            git(self.root, "commit", "-q", "-m", f"feature {index}")
        feature = git(self.root, "rev-parse", "HEAD")
        self.assertFalse(on_main(self.root, feature))
        # Five commits ahead, and still numbered as its base: it does not
        # outrank the main build that follows, so a device upgrades from it.
        self.assertEqual(build_number(self.root, feature), base_number)
        git(self.root, "checkout", "-q", "main")
        later = self.commit("main moves on")
        self.assertGreater(build_number(self.root, later), build_number(self.root, feature))
        with self.assertRaisesRegex(MobileBuildError, "is not on origin/main"):
            require_main(self.root, feature)
        require_main(self.root, later)
        # A hotfix cut from an old main commit is a branch too, until it lands.
        git(self.root, "checkout", "-q", "-b", "hotfix", self.base)
        self.write("clients/android/app/src/main/H.kt", "val h = 1\n")
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "-m", "hotfix")
        self.assertEqual(build_number(self.root, "hotfix"), base_number)
        git(self.root, "checkout", "-q", "main")
        landed = self.merge("hotfix")
        self.assertGreater(build_number(self.root, landed), build_number(self.root, later))

    def test_a_repository_without_origin_main_cannot_number_a_build(self):
        git(self.root, "update-ref", "-d", "refs/remotes/origin/main")
        with self.assertRaisesRegex(MobileBuildError, "has no origin/main"):
            build_number(self.root)

    def test_an_unstamped_or_half_stamped_checkout_is_refused(self):
        with self.assertRaisesRegex(MobileBuildError, "is not a release source"):
            require_stamped(self.root)
        self.write(ANDROID, ANDROID_TEXT.replace("versionCode = 1", "versionCode = 42"))
        with self.assertRaisesRegex(MobileBuildError, "is not a release source"):
            require_stamped(self.root)
        self.write(APPLE, APPLE_TEXT.replace('VERSION: "1"', 'VERSION: "42"'))
        self.assertEqual(require_stamped(self.root), {"android": 42, "apple": 42})

    def test_a_release_checkout_is_stamped_reused_and_restored(self):
        self.write("clients/android/app/src/main/B.kt", "fun b() {}\n")
        head = self.commit("client change")
        state = self.root.parent / "state"
        checkout, build, stamped = release_checkout(self.root, state, "HEAD")
        self.assertEqual(build, build_number(self.root, head))
        self.assertEqual(declared_builds(checkout), {"android": build, "apple": build})
        self.assertEqual(git(checkout, "rev-parse", "HEAD^"), head)
        self.assertEqual(checkout.name, f"plurx-{stamped[:12]}")
        # Build output survives a second call; an edit to a tracked file does not.
        (checkout / "build-output").write_text("kept")
        (checkout / ANDROID).write_text("versionCode = 1\n")
        again, _, _ = release_checkout(self.root, state, head)
        self.assertEqual(again, checkout)
        self.assertEqual((checkout / "build-output").read_text(), "kept")
        self.assertEqual(declared_builds(checkout)["android"], build)
        # The working checkout was never touched.
        self.assertEqual(git(self.root, "status", "--porcelain"), "")
        self.assertEqual((self.root / ANDROID).read_text(), ANDROID_TEXT)
        # The next commit gets its own directory.
        self.commit("another")
        newer, newer_build, _ = release_checkout(self.root, state, "HEAD")
        self.assertNotEqual(newer, checkout)
        self.assertGreater(newer_build, build)

    def origin(self) -> None:
        """The repository is its own `origin`, as a forge both hosts can reach."""
        git(self.root, "remote", "add", "origin", str(self.root))

    def test_the_forge_records_every_publication_and_is_the_floor_on_any_host(self):
        self.origin()
        first = self.commit("first release")
        with patch.dict(mobile_build.LAST_CLAIMED, {"apple": 301, "android": 207}):
            # Nothing recorded yet: the floor is the last hand-claimed build.
            require_above_published(self.root, ["android"], 208)
            with self.assertRaisesRegex(MobileBuildError, "not above android build 207"):
                require_above_published(self.root, ["android"], 207)
            with self.assertRaisesRegex(MobileBuildError, "not above ios build 301"):
                require_above_published(self.root, ["ios", "android"], 250)
        number = build_number(self.root, first)
        record_published(self.root, ["ios", "tvos"], number, first, "b" * 40)
        self.assertEqual(
            git(self.root, "for-each-ref", "--format=%(refname) %(objectname)", "refs/cinema-published"),
            f"refs/cinema-published/ios {first}\nrefs/cinema-published/tvos {first}",
        )
        self.assertEqual(forge_published(self.root), {"ios": (number, first), "tvos": (number, first)})
        # Another host: a fresh clone with no state directory at all still
        # refuses the same number, and a rewritten main with fewer commits.
        clone = self.root.parent / "second-host"
        subprocess.run(["git", "clone", "-q", "-b", "main", str(self.root), str(clone)], check=True)
        for build in (number, number - 1):
            with self.subTest(build=build):
                with self.assertRaisesRegex(MobileBuildError, "per the forge"):
                    require_above_published(clone, ["ios"], build)
        require_above_published(clone, ["ios"], number + 1)
        require_above_published(clone, ["android"], number)  # nothing published there yet
        # The next release moves the refs forward.
        second = self.commit("second release")
        record_published(self.root, ["ios", "tvos", "android"], number + 1, second, "c" * 40)
        self.assertEqual(forge_published(clone)["android"], (number + 1, second))

    def test_a_publication_that_cannot_be_recorded_is_an_error_with_the_command(self):
        git(self.root, "remote", "add", "origin", str(self.root.parent / "no-such-forge"))
        with self.assertRaisesRegex(MobileBuildError, "could not be recorded in the forge"):
            record_published(self.root, ["android"], 5, self.base, "b" * 40)
        with self.assertRaises(MobileBuildError):
            require_above_published(self.root, ["android"], 999)

    def test_a_publication_ref_only_moves_forward_and_only_from_the_value_read(self):
        self.origin()
        first = self.commit("first release")
        second = self.commit("second release")
        third = self.commit("third release")
        ref = "refs/cinema-published/android"
        record_published(self.root, ["android"], build_number(self.root, second), second, "b" * 40)
        # An older main commit whose publish finishes late leaves the ref alone.
        record_published(self.root, ["android"], build_number(self.root, first), first, "c" * 40)
        self.assertEqual(git(self.root, "rev-parse", ref), second)
        # A stale read: this publisher saw no ref, or an older one, while
        # another moved it. The lease refuses the push and the ref is read again.
        real = mobile_build.forge_published
        for stale, publishing, expected in (
            ({}, third, third),
            ({"android": (build_number(self.root, first), first)}, second, third),
        ):
            reads = []

            def read(source, stale=stale, reads=reads):
                reads.append(None)
                return stale if len(reads) == 1 else real(source)

            with self.subTest(stale=stale), patch.object(mobile_build, "forge_published", read):
                record_published(
                    self.root, ["android"], build_number(self.root, publishing), publishing, "d" * 40
                )
                self.assertEqual(len(reads), 2)
            self.assertEqual(git(self.root, "rev-parse", ref), expected)
        source = Path(mobile_build.__file__).read_text(encoding="utf-8")
        self.assertIn("--force-with-lease=", source)
        self.assertNotIn('"--force"', source)

    def test_an_upload_the_forge_did_not_record_is_kept_locally_and_only_the_push_is_retried(self):
        self.origin()
        state = self.root.parent / "state"
        head = self.commit("a release")
        number = build_number(self.root, head)
        git(self.root, "remote", "set-url", "origin", str(self.root.parent / "no-such-forge"))
        with self.assertRaisesRegex(MobileBuildError, "retries the push"):
            record_published(self.root, ["ios", "android"], number, head, "b" * 40, state)
        # The ledger line was written before the push, so this host knows the build.
        self.assertEqual(published(state)["android"]["source_commit"], head)
        with self.assertRaises(MobileBuildError):
            mobile_build.retry_unrecorded(self.root, state)
        git(self.root, "remote", "set-url", "origin", str(self.root))
        self.assertEqual(forge_published(self.root), {})
        with self.assertRaisesRegex(MobileBuildError, "per this controller's ledger"):
            require_above_published(self.root, ["android"], number, state)
        self.assertEqual(mobile_build.retry_unrecorded(self.root, state), {"android": head, "ios": head})
        self.assertEqual(forge_published(self.root), {"android": (number, head), "ios": (number, head)})
        self.assertEqual(mobile_build.retry_unrecorded(self.root, state), {})
        self.assertEqual(len((state / mobile_build.LEDGER).read_text().splitlines()), 2)

    def test_one_publish_lock_serves_the_channel_job_and_a_shell_that_holds_a_descriptor(self):
        state = self.root.parent / "state"
        with mobile_build.publish_lock(state):
            with self.assertRaisesRegex(MobileBuildError, "another mobile release holds"):
                with mobile_build.publish_lock(state):
                    self.fail("the lock was taken twice")
        # scripts/ship opens the file on descriptor 9 and has this module lock
        # it; the lock outlives that helper and lasts as long as the shell.
        repo = Path(mobile_build.__file__).resolve().parents[1]
        probe = (
            "import sys\nfrom pathlib import Path\nfrom validation import mobile_build\n"
            "try:\n    with mobile_build.publish_lock(Path(sys.argv[1])):\n        print('free')\n"
            "except mobile_build.MobileBuildError:\n    print('held')\n"
        )
        script = (
            'exec 9>>"$STATE/publish.lock"\n'
            '"$PY" -m validation.mobile_build publish-lock --fd 9 || exit 7\n'
            '"$PY" -c "$PROBE" "$STATE"\n'
            '"$PY" -m validation.mobile_build publish-lock --fd 9 9>>"$STATE/publish.lock"; echo "second=$?"\n'
        )
        result = subprocess.run(
            ["bash", "-c", script], text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env={**os.environ, "STATE": str(state), "PLURX_MOBILE_STATE_DIR": str(state),
                 "PY": sys.executable, "PROBE": probe, "PYTHONPATH": str(repo)},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.split(), ["held", "second=1"])
        with mobile_build.publish_lock(state):
            pass  # released when the shell exited

    def test_the_controller_ledger_is_a_second_floor_and_names_both_commits(self):
        self.origin()
        state = self.root.parent / "state"
        record_published(self.root, ["android"], 480, self.base, "d" * 40, state)
        latest = published(state)
        self.assertEqual(
            (latest["android"]["build"], latest["android"]["source_commit"], latest["android"]["stamp_commit"]),
            (480, self.base, "d" * 40),
        )
        with self.assertRaisesRegex(MobileBuildError, "per this controller's ledger"):
            require_above_published(self.root, ["android"], 480, state)
        require_above_published(self.root, ["android"], 481, state)
        (state / mobile_build.LEDGER).write_text("not json\n")
        with self.assertRaisesRegex(MobileBuildError, "not a publication record"):
            published(state)

    def test_only_a_main_commit_gets_a_store_scope_stamp(self):
        state = self.root.parent / "state"
        self.commit("a second commit, so the number is not the development build")
        checkout, _, _ = release_checkout(self.root, state, "HEAD", "store")
        self.assertEqual(build_scope(checkout), "store")
        devices, _, _ = release_checkout(self.root, state, "HEAD")
        self.assertEqual(build_scope(devices), "devices")
        self.assertNotEqual(checkout, devices, "the scope is part of the stamp")
        self.assertEqual(build_scope(self.root), "", "an unstamped checkout has no scope")
        git(self.root, "checkout", "-q", "-b", "feature")
        self.write("clients/android/app/src/main/F.kt", "val f = 1\n")
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "-m", "feature")
        with self.assertRaisesRegex(MobileBuildError, "is not on origin/main"):
            release_checkout(self.root, state, "feature", "store")
        self.assertEqual(build_scope(release_checkout(self.root, state, "feature")[0]), "devices")

    def pin(self) -> Path:
        pin = self.root.parent / "pin.git"
        subprocess.run(["git", "init", "-q", "--bare", str(pin)], check=True)
        (pin / "objects" / "info" / "alternates").write_text(f"{self.root / '.git' / 'objects'}\n")
        return pin

    def test_the_stamp_commit_is_one_reproducible_child_that_changes_two_lines(self):
        self.write("clients/android/app/src/main/B.kt", "fun b() {}\n")
        head = self.commit("client change after the seed")
        pin = self.pin()
        stamped = stamp_commit(pin, head, 15679)
        self.assertEqual(git(pin, "rev-parse", f"{stamped}^"), head)
        self.assertEqual(
            git(pin, "diff", "--name-only", head, stamped).split(), [ANDROID, APPLE]
        )
        changed = git(pin, "diff", "-U0", head, stamped)
        self.assertEqual(
            sorted(line for line in changed.splitlines() if re.match(r"[+-][^+-]", line)),
            sorted([
                "-        versionCode = 1", "+        versionCode = 15679",
                '-    CURRENT_PROJECT_VERSION: "1"', '+    CURRENT_PROJECT_VERSION: "15679"',
            ]),
        )
        # A retried job stamps the same commit, from any repository holding it.
        self.assertEqual(stamp_commit(pin, head, 15679), stamped)
        self.assertEqual(stamp_commit(self.root, head, 15679), stamped)
        self.assertNotEqual(stamp_commit(pin, head, 15680), stamped)
        # Nothing was checked out or moved in the source repository.
        self.assertEqual(git(self.root, "status", "--porcelain"), "")
        self.assertEqual(git(self.root, "rev-parse", "HEAD"), head)

    def test_the_signing_controller_accepts_the_stamp_commit(self):
        """The controller's two checks, run against a stamped checkout.

        `noirr_mobile_metadata` reads each manifest with these patterns, and
        `noirr_mobile_release_guard` finds the newest commit that changed the
        number (`git log -1 -G <pattern> HEAD -- <manifest>`) and refuses the
        platform if any client path changed after it. Client sources changed
        long after the tree's number last did, so an unstamped main is
        refused; the stamp commit is the newest change to both numbers.
        """
        self.write("clients/android/app/src/main/B.kt", "fun b() {}\n")
        self.write("clients/apple/Sources/A.swift", "let a = 1\n")
        head = self.commit("client changes after the seed")
        build = build_number(self.root, head)
        pin, stamped = release_source(self.root, self.root.parent / "state", head, build)
        self.assertEqual(git(pin, "rev-parse", "main"), stamped)
        checkout = self.root.parent / "release"
        git(pin, "worktree", "add", "-q", "--detach", str(checkout), "main")

        def metadata(path: str, pattern: str) -> str:
            return re.search(pattern, (checkout / path).read_text(), re.MULTILINE).group(1)

        self.assertEqual(metadata(ANDROID, r"^\s*versionCode\s*=\s*(\d+)"), str(build))
        self.assertEqual(
            metadata(APPLE, r'^\s*CURRENT_PROJECT_VERSION:\s*["\']([^"\']+)["\']'), str(build)
        )

        def changed_after_the_number(repo: Path, pattern: str, manifest: str, client: str) -> list[str]:
            bump = git(repo, "log", "-1", "--format=%H", "-G", pattern, "HEAD", "--", manifest)
            return git(repo, "diff", "--name-only", f"{bump}..HEAD", "--", client).split()

        layouts = (
            ("versionCode", ANDROID, "clients/android"),
            ("CURRENT_PROJECT_VERSION", APPLE, "clients/apple"),
        )
        for pattern, manifest, client in layouts:
            with self.subTest(manifest=manifest):
                self.assertEqual(changed_after_the_number(checkout, pattern, manifest, client), [])
                self.assertNotEqual(
                    changed_after_the_number(self.root, pattern, manifest, client), [],
                    "without the stamp the controller would refuse this platform",
                )


if __name__ == "__main__":
    unittest.main()
