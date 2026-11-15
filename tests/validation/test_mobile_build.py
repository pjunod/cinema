"""The store build number is assigned at release, from the release commit."""

from __future__ import annotations

from pathlib import Path
import re
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from validation import mobile_build
from validation.mobile_build import (
    ANDROID,
    APPLE,
    MobileBuildError,
    build_number,
    stamp_commit,
    stamp_manifests,
    stamp_tree,
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
        return git(self.root, "rev-parse", "HEAD")

    def branch(self, name: str, path: str) -> None:
        git(self.root, "checkout", "-q", "-b", name, self.base)
        self.write(path, name)
        self.commit(f"change {name}")
        git(self.root, "checkout", "-q", "main")

    def merge(self, name: str) -> str:
        git(self.root, "merge", "-q", "--no-ff", "-m", f"merge {name}", name)
        return git(self.root, "rev-parse", "HEAD")

    def test_two_client_branches_merge_in_either_order_and_every_release_rises(self):
        self.branch("apple-change", "clients/apple/Sources/A.swift")
        self.branch("android-change", "clients/android/app/src/main/B.kt")
        for order in (("apple-change", "android-change"), ("android-change", "apple-change")):
            with self.subTest(order=order):
                git(self.root, "reset", "-q", "--hard", self.base)
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
        subprocess.run(["git", "clone", "-q", str(self.root), str(clone)], check=True)
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

    def test_a_disposable_checkout_is_stamped_in_place(self):
        stamp_tree(self.root, 42)
        self.assertIn("versionCode = 42\n", (self.root / ANDROID).read_text())
        self.assertIn('CURRENT_PROJECT_VERSION: "42"\n', (self.root / APPLE).read_text())
        self.assertEqual(
            git(self.root, "status", "--porcelain").split(), ["M", ANDROID, "M", APPLE]
        )

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
        pin = self.pin()
        build = build_number(self.root, head)
        stamped = stamp_commit(pin, head, build)
        git(pin, "update-ref", "refs/heads/main", stamped)
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
