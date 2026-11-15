from __future__ import annotations

from pathlib import Path
import subprocess
import tempfile
import unittest

from validation.mobile_build import DEVELOPMENT_BUILD
from validation.mobile_versions import (
    MobileVersionError,
    MobileVersions,
    _touches_shipped_bytes,
    check_repository,
    validate_versions,
)
from validation.runner import load_catalog, select_points, selected_checks


ROOT = Path(__file__).resolve().parents[2]


def versions(
    *,
    workspace: str = "0.2.2",
    apple_name: str | None = None,
    apple_build: int = DEVELOPMENT_BUILD,
    android_name: str | None = None,
    android_build: int = DEVELOPMENT_BUILD,
) -> MobileVersions:
    return MobileVersions(
        workspace=workspace,
        android_name=android_name or workspace,
        android_build=android_build,
        apple_name=apple_name or workspace,
        apple_build=apple_build,
        apple_marketing_references=2,
        apple_build_references=2,
    )


def apple_project(build: int, name: str = "0.2.2") -> str:
    return f'''settings:
  base:
    MARKETING_VERSION: "{name}"
    CURRENT_PROJECT_VERSION: "{build}"
targets:
  ios:
    info:
      properties:
        CFBundleShortVersionString: "$(MARKETING_VERSION)"
        CFBundleVersion: "$(CURRENT_PROJECT_VERSION)"
  tvos:
    info:
      properties:
        CFBundleShortVersionString: "$(MARKETING_VERSION)"
        CFBundleVersion: "$(CURRENT_PROJECT_VERSION)"
'''


def git(root: Path, *arguments: str) -> str:
    return subprocess.run(
        ["git", *arguments],
        cwd=root,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    ).stdout.strip()


def android_gradle(build: int, name: str = "0.2.2") -> str:
    return f'namespace = "org.plurx"\nversionCode = {build}\nversionName = "{name}"\n'


def cargo_workspace(version: str = "0.2.2", *, dependency: bool = False) -> str:
    manifest = f'[workspace]\n[workspace.package]\nversion = "{version}"\n'
    if dependency:
        manifest += '\n[workspace.dependencies]\nserde = "1"\n'
    return manifest


def seed_repository(
    root: Path,
    *,
    apple_build: int = DEVELOPMENT_BUILD,
    android_build: int = DEVELOPMENT_BUILD,
) -> None:
    """Lay down a minimal repo on a ``main`` branch with both apps present."""
    (root / "clients/apple/Sources").mkdir(parents=True)
    (root / "clients/android/app/src/main").mkdir(parents=True)
    (root / "Cargo.toml").write_text(cargo_workspace(), encoding="utf-8")
    (root / "clients/apple/project.yml").write_text(
        apple_project(apple_build), encoding="utf-8"
    )
    (root / "clients/apple/Sources/App.swift").write_text(
        "let release = 1\n", encoding="utf-8"
    )
    (root / "clients/android/app/build.gradle.kts").write_text(
        android_gradle(android_build), encoding="utf-8"
    )
    (root / "clients/android/app/src/main/AndroidManifest.xml").write_text(
        "<manifest />\n", encoding="utf-8"
    )
    git(root, "init", "-q")
    git(root, "checkout", "-qb", "main")
    git(root, "config", "user.name", "Version Test")
    git(root, "config", "user.email", "version@example.invalid")
    git(root, "add", "-A")
    git(root, "commit", "-qm", "seed")


class MobileVersionCase(unittest.TestCase):
    def test_this_repository_declares_coherent_versions_and_claims_no_build(self):
        self.assertEqual(check_repository(ROOT), ())

    def test_mobile_marketing_versions_still_match_the_workspace_release(self):
        errors = validate_versions(
            versions(apple_name="0.2.1", android_name="0.3.0")
        )

        self.assertTrue(any("Android versionName must match" in error for error in errors))
        self.assertTrue(
            any("Apple MARKETING_VERSION must match" in error for error in errors)
        )

    def test_the_development_build_is_the_only_number_the_tree_may_carry(self):
        self.assertEqual(validate_versions(versions()), ())
        apple = validate_versions(versions(apple_build=302))
        self.assertEqual(len(apple), 1)
        self.assertIn("Apple CURRENT_PROJECT_VERSION must stay 1", apple[0])
        android = validate_versions(versions(android_build=208))
        self.assertEqual(len(android), 1)
        self.assertIn("Android versionCode must stay 1", android[0])

    def test_a_client_change_obligates_no_edit_to_either_manifest(self):
        """Two branches each change a client; neither touches a counter.

        This is the property the old rule could not have: it demanded that
        each branch write the next number, so both wrote the same line.
        """
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            seed_repository(root)
            base = git(root, "rev-parse", "HEAD")
            for branch, path, text in (
                ("one", "clients/apple/Sources/App.swift", "let release = 2\n"),
                ("two", "clients/android/app/src/main/AndroidManifest.xml", "<manifest a='1' />\n"),
            ):
                git(root, "checkout", "-qb", branch, base)
                (root / path).write_text(text, encoding="utf-8")
                git(root, "add", "-A")
                git(root, "commit", "-qm", f"change {branch}")
                self.assertEqual(
                    check_repository(root, mode="changed-from", base=base, merge_target="main"),
                    (),
                )
            git(root, "checkout", "-q", "main")
            git(root, "merge", "-q", "--no-ff", "-m", "merge one", "one")
            git(root, "merge", "-q", "--no-ff", "-m", "merge two", "two")
            self.assertEqual(check_repository(root), ())

    def test_a_claimed_number_is_refused_in_the_tree_each_mode_reads(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            seed_repository(root)
            base = git(root, "rev-parse", "HEAD")
            manifest = root / "clients/apple/project.yml"
            manifest.write_text(apple_project(11), encoding="utf-8")

            # Unstaged: the working tree carries it, the index and HEAD do not.
            self.assertEqual(len(check_repository(root, mode="all")), 1)
            self.assertEqual(check_repository(root, mode="staged"), ())
            self.assertEqual(check_repository(root, mode="changed-from", base=base), ())

            git(root, "add", "clients/apple/project.yml")
            self.assertEqual(len(check_repository(root, mode="staged")), 1)
            self.assertEqual(check_repository(root, mode="changed-from", base=base), ())

            git(root, "commit", "-qm", "claim a build number")
            errors = check_repository(root, mode="changed-from", base=base)
            self.assertEqual(len(errors), 1)
            self.assertIn("assigned when a build is released", errors[0])

    def test_modes_keep_the_argument_shape_the_lanes_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            seed_repository(root)
            with self.assertRaises(MobileVersionError):
                check_repository(root, mode="changed-from")
            with self.assertRaises(MobileVersionError):
                check_repository(root, mode="all", merge_target="main")
            with self.assertRaises(MobileVersionError):
                check_repository(root, mode="nonsense")

    def test_catalog_routes_only_release_inputs_to_the_version_gate(self):
        catalog = load_catalog(ROOT / "validation/points.toml")

        def check_ids(path: str) -> set[str]:
            selection = select_points(catalog, (path,))
            return {
                check.id
                for check in selected_checks(catalog, selection, profile="commit")
            }

        self.assertIn(
            "mobile-version", check_ids("clients/apple/Sources/PlayerView.swift")
        )
        self.assertIn(
            "mobile-version",
            check_ids("clients/android/app/src/main/AndroidManifest.xml"),
        )
        self.assertIn("mobile-version", check_ids("Cargo.toml"))
        self.assertNotIn(
            "mobile-version", check_ids("clients/apple/Tests/AppleClientTests.swift")
        )
        self.assertNotIn(
            "mobile-version",
            check_ids("clients/android/app/src/test/AppVersionTest.kt"),
        )


class ShippedBytesCase(unittest.TestCase):
    """A comment carries no bytes to a device, so it is no reason to publish.

    `validation.mobile_channels` leaves a path out of the release scope only
    when every line its diff touched is a comment or blank. The exemption has
    to be narrow in exactly one direction — it may never excuse a change that
    ships — so every case that mixes code into the diff proves it does not.
    """

    def shipped(self, path: str, before: str | None, after: str | None) -> bool:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            seed_repository(root)
            target = root / path
            if before is not None:
                target.write_text(before, encoding="utf-8")
                git(root, "add", "-A")
                git(root, "commit", "-qm", "before")
            base = git(root, "rev-parse", "HEAD")
            if after is None:
                target.unlink()
            else:
                target.write_text(after, encoding="utf-8")
            git(root, "add", "-A")
            git(root, "commit", "-qm", "after")
            return _touches_shipped_bytes(root, path, base, "HEAD")

    SWIFT = "clients/apple/Sources/App.swift"

    def test_a_comment_only_change_ships_nothing(self):
        self.assertFalse(
            self.shipped(self.SWIFT, None, "/// See the parity notes.\nlet release = 1\n")
        )

    def test_a_block_comment_and_its_delimiters_ship_nothing(self):
        self.assertFalse(
            self.shipped(self.SWIFT, None, "/*\n * See the parity notes.\n */\nlet release = 1\n")
        )

    def test_a_code_change_beside_a_comment_ships(self):
        self.assertTrue(
            self.shipped(self.SWIFT, None, "/// A comment.\nlet release = 2\n")
        )

    def test_a_deleted_source_file_ships(self):
        self.assertTrue(self.shipped(self.SWIFT, None, None))

    def test_a_new_source_file_of_nothing_but_comments_ships_nothing(self):
        self.assertFalse(
            self.shipped("clients/apple/Sources/Notes.swift", None, "// Only a note.\n")
        )

    def test_a_non_source_release_input_always_ships(self):
        self.assertTrue(
            self.shipped(
                "clients/android/app/src/main/AndroidManifest.xml",
                None,
                "<!-- only a comment -->\n<manifest />\n",
            )
        )

    def test_an_unreadable_diff_ships(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            seed_repository(root)
            self.assertTrue(_touches_shipped_bytes(root, self.SWIFT, "no-such-ref", "HEAD"))


if __name__ == "__main__":
    unittest.main()
