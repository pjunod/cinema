"""scripts/build-number: the store build number, derived from Git history."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "build-number"
FLOOR = 310  # the last stored counter; scripts/build-number STORED_FLOOR


def clean_env(**extra: str) -> dict[str, str]:
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("GIT_") and key != "PLURX_BUILD_NUMBER"}
    env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull,
               GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
               GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid")
    env.update(extra)
    return env


class BuildNumberCase(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.tmp = Path(temporary.name)
        self.repo = self.tmp / "repo"
        self.repo.mkdir()
        self.git("init", "-q", "-b", "main")

    def git(self, *args: str, cwd: Path | None = None) -> str:
        return subprocess.run(["git", *args], cwd=cwd or self.repo, check=True, text=True,
                              capture_output=True, env=clean_env()).stdout.strip()

    def commit(self, count: int = 1) -> str:
        # Distinct messages: empty commits made in the same second with one
        # parent would otherwise be the same object on two branches.
        for _ in range(count):
            self.serial = getattr(self, "serial", 0) + 1
            self.git("commit", "-q", "--allow-empty", "-m", f"c{self.serial}")
        return self.git("rev-parse", "HEAD")

    def history(self, count: int) -> None:
        """`count` first-parent commits on main, quickly (fast-import)."""
        stream = "".join(
            f"commit refs/heads/main\nmark :{i + 1}\n"
            f"committer t <t@example.invalid> {1700000000 + i} +0000\n"
            f"data 2\nc\n" + ("" if i == 0 else f"from :{i}\n") + "\n"
            for i in range(count)
        )
        subprocess.run(["git", "fast-import", "--quiet"], cwd=self.repo, input=stream, text=True,
                       check=True, env=clean_env())
        self.git("reset", "-q", "--hard", "main")

    def run_script(self, *args: str, repo: Path | None = None, **env: str):
        return subprocess.run([sys.executable, str(SCRIPT), "--repo", str(repo or self.repo), *args],
                              capture_output=True, text=True, env=clean_env(**env), timeout=60)

    def number(self, *args: str, **env: str) -> int:
        result = self.run_script(*args, **env)
        self.assertEqual(result.returncode, 0, result.stderr)
        return int(result.stdout.strip())

    def test_a_main_commit_is_numbered_by_its_first_parent_position(self) -> None:
        self.commit(3)
        self.assertEqual(self.number(), 3)
        self.commit()
        self.assertEqual(self.number(), 4, "each later main commit gets a higher number")

    def test_a_merge_on_main_counts_once_whatever_it_brings_in(self) -> None:
        self.commit(2)
        self.git("checkout", "-q", "-b", "feature")
        self.commit(5)
        self.git("checkout", "-q", "main")
        self.git("merge", "-q", "--no-ff", "-m", "merge feature", "feature")
        self.assertEqual(self.number(), 3)

    def test_a_branch_build_takes_its_main_base_and_never_passes_later_main(self) -> None:
        self.commit(4)
        self.git("checkout", "-q", "-b", "feature")
        self.commit(6)
        self.assertEqual(self.number(), 4, "a branch build is numbered by its main base")
        self.git("checkout", "-q", "main")
        self.commit()
        self.git("checkout", "-q", "feature")
        self.assertEqual(self.number(), 4)
        self.git("checkout", "-q", "main")
        self.assertEqual(self.number(), 5, "the later main build is above every branch build")

    def test_a_release_build_off_main_is_refused(self) -> None:
        """A stale origin/main must not hand an up-to-date local main an old number."""

        self.history(FLOOR + 5)
        self.git("update-ref", "refs/remotes/origin/main", "HEAD~2")
        result = self.run_script("--release")
        self.assertEqual(result.returncode, 1)
        self.assertIn("is not on origin/main", result.stderr)
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.assertEqual(self.run_script("--release").returncode, 0)

    def test_origin_main_is_preferred_over_a_local_main(self) -> None:
        self.commit(3)
        self.git("update-ref", "refs/remotes/origin/main", "HEAD~1")
        self.git("checkout", "-q", "-b", "feature")
        self.commit(2)
        self.assertEqual(self.number(), 2)
        explained = self.run_script("--explain").stdout
        self.assertIn("origin/main", explained)
        self.assertIn("build number: 2", explained)

    def test_without_a_main_ref_the_head_is_counted(self) -> None:
        self.git("checkout", "-q", "-b", "other")
        self.commit(3)
        self.assertNotIn("main", self.git("branch", "--list", "main"))
        self.assertEqual(self.number(), 3)

    def test_a_release_build_needs_more_history_than_the_last_stored_counter(self) -> None:
        self.commit(5)
        result = self.run_script("--release")
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"not above {FLOOR}", result.stderr)
        self.assertEqual(result.stdout, "")
        # A development build still gets the derived number, with a warning.
        result = self.run_script()
        self.assertEqual((result.returncode, result.stdout.strip()), (0, "5"))
        self.assertIn("warning", result.stderr)

    def test_a_release_build_of_a_full_history_succeeds(self) -> None:
        self.history(FLOOR + 5)
        result = self.run_script("--release")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(FLOOR + 5))
        self.assertEqual(result.stderr, "")

    def test_a_shallow_clone_degrades_to_one_and_refuses_a_release(self) -> None:
        self.history(FLOOR + 5)
        shallow = self.tmp / "shallow"
        self.git("clone", "-q", "--depth", "3", f"file://{self.repo}", str(shallow), cwd=self.tmp)
        result = self.run_script(repo=shallow)
        self.assertEqual((result.returncode, result.stdout.strip()), (0, "1"))
        self.assertIn("shallow", result.stderr)
        result = self.run_script("--release", repo=shallow)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn("shallow", result.stderr)

    def test_a_checkout_without_git_history_degrades_to_one_and_refuses_a_release(self) -> None:
        plain = self.tmp / "plain"
        plain.mkdir()
        result = self.run_script(repo=plain, GIT_CEILING_DIRECTORIES=str(self.tmp))
        self.assertEqual((result.returncode, result.stdout.strip()), (0, "1"))
        self.assertIn("warning", result.stderr)
        result = self.run_script("--release", repo=plain, GIT_CEILING_DIRECTORIES=str(self.tmp))
        self.assertEqual(result.returncode, 1)

    def test_an_explicit_override_wins_and_is_validated(self) -> None:
        self.commit(2)
        self.assertEqual(self.number(PLURX_BUILD_NUMBER="4242"), 4242)
        self.assertEqual(self.number("--release", PLURX_BUILD_NUMBER="4242"), 4242)
        for bad in ("0", "-3", "12a"):
            with self.subTest(bad):
                self.assertEqual(self.run_script(PLURX_BUILD_NUMBER=bad).returncode, 1)
        self.assertEqual(self.run_script("--release", PLURX_BUILD_NUMBER="300").returncode, 1)

    def test_an_inherited_git_dir_cannot_redirect_the_count(self) -> None:
        self.commit(3)
        other = self.tmp / "other"
        other.mkdir()
        self.git("init", "-q", "-b", "main", cwd=other)
        self.git("commit", "-q", "--allow-empty", "-m", "c", cwd=other)
        self.assertEqual(self.number(GIT_DIR=str(other / ".git")), 3)

    def test_every_release_signed_android_variant_derives_strictly(self) -> None:
        """ProfileCapture is release-signed, so it gets --release like assembleRelease."""

        gradle = (ROOT / "clients/android/app/build.gradle.kts").read_text(encoding="utf-8")
        self.assertIn("val release = releaseTaskRequested ||", gradle)
        self.assertIn('it.contains("ProfileCapture")', gradle)

    def test_this_repository_is_numbered_above_the_last_stored_counter(self) -> None:
        if subprocess.run(["git", "-C", str(ROOT), "rev-parse", "--is-shallow-repository"],
                          capture_output=True, text=True).stdout.strip() != "false":
            self.skipTest("shallow checkout")
        # Not --release: this checkout is usually a pull request branch, which a
        # release build refuses; the derived number is the same either way.
        result = subprocess.run([sys.executable, str(SCRIPT)], capture_output=True,
                                text=True, env=clean_env(), timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertGreater(int(result.stdout), FLOOR)
        self.assertNotIn("not above", result.stderr)


if __name__ == "__main__":
    unittest.main()
