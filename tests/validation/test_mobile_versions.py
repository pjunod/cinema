from __future__ import annotations

import dataclasses
from pathlib import Path
import subprocess
import tempfile
import unittest

from validation.mobile_versions import (
    APPLE_PLACEHOLDER_BUILD,
    MobileVersionError,
    MobileVersions,
    check_repository,
    read_versions,
    validate_versions,
)
from validation.runner import load_catalog, select_points, selected_checks


ROOT = Path(__file__).resolve().parents[2]
STAMP = '''    postBuildScripts:
      - name: Stamp derived build number
        script: |
          number="$("${SRCROOT}/../../scripts/build-number" ${mode})"
          /usr/libexec/PlistBuddy -c "Set :CFBundleVersion ${number}" "${TARGET_BUILD_DIR}/${INFOPLIST_PATH}"
'''


def versions(**changes) -> MobileVersions:
    """A tree that satisfies every rule, with ``changes`` applied."""
    good = MobileVersions(
        workspace="0.2.2",
        android_name="0.2.2",
        android_version_code="plurxVersionCode",
        android_runs_build_number=True,
        apple_name="0.2.2",
        apple_build=APPLE_PLACEHOLDER_BUILD,
        apple_marketing_references=2,
        apple_build_references=2,
        apple_stamp_phases=2,
        apple_runs_build_number=2,
    )
    return dataclasses.replace(good, **changes)


def apple_project(build: int = 1, name: str = "0.2.2", *, stamps: int = 2) -> str:
    targets = []
    for index, target in enumerate(("ios", "tvos")):
        targets.append(
            f'''  {target}:
{STAMP if index < stamps else ""}    info:
      properties:
        CFBundleShortVersionString: "$(MARKETING_VERSION)"
        CFBundleVersion: "$(CURRENT_PROJECT_VERSION)"
'''
        )
    return f'''settings:
  base:
    MARKETING_VERSION: "{name}"
    CURRENT_PROJECT_VERSION: "{build}"
targets:
''' + "".join(targets)


def android_gradle(code: str = "plurxVersionCode", name: str = "0.2.2") -> str:
    return (
        'val plurxVersionCode = run { exec("../../scripts/build-number") }\n'
        f'namespace = "org.plurx"\nversionCode = {code}\nversionName = "{name}"\n'
    )


def git(root: Path, *arguments: str) -> str:
    return subprocess.run(
        ["git", *arguments], cwd=root, check=True, text=True, stdout=subprocess.PIPE,
    ).stdout.strip()


def seed_repository(root: Path) -> None:
    """A minimal repository whose two apps both derive their build numbers."""
    (root / "clients/apple").mkdir(parents=True)
    (root / "clients/android/app").mkdir(parents=True)
    (root / "Cargo.toml").write_text(
        '[workspace]\n[workspace.package]\nversion = "0.2.2"\n', encoding="utf-8"
    )
    (root / "clients/apple/project.yml").write_text(apple_project(), encoding="utf-8")
    (root / "clients/android/app/build.gradle.kts").write_text(
        android_gradle(), encoding="utf-8"
    )
    git(root, "init", "-q")
    git(root, "config", "user.name", "Version Test")
    git(root, "config", "user.email", "version@example.invalid")
    git(root, "add", "-A")
    git(root, "commit", "-qm", "seed")


class MobileVersionCase(unittest.TestCase):
    def test_the_repository_derives_both_build_numbers(self):
        self.assertEqual(check_repository(ROOT), ())
        current = read_versions(lambda path: (ROOT / path).read_text(encoding="utf-8"))
        self.assertEqual(current.apple_build, APPLE_PLACEHOLDER_BUILD)
        self.assertEqual(current.android_version_code, "plurxVersionCode")

    def test_mobile_marketing_versions_still_match_the_workspace_release(self):
        errors = validate_versions(versions(android_name="0.2.1", apple_name="0.2.3"))

        self.assertEqual(len(errors), 2)
        self.assertIn("Android versionName", errors[0])
        self.assertIn("Apple MARKETING_VERSION", errors[1])

    def test_a_valid_tree_passes(self):
        self.assertEqual(validate_versions(versions()), ())

    def test_a_stored_android_version_code_is_rejected(self):
        errors = validate_versions(versions(android_version_code="211"))

        self.assertEqual(len(errors), 1)
        self.assertIn("never stored in source", errors[0])

    def test_an_android_build_that_does_not_run_build_number_is_rejected(self):
        errors = validate_versions(versions(android_runs_build_number=False))

        self.assertEqual(len(errors), 1)
        self.assertIn("scripts/build-number", errors[0])

    def test_a_bumped_apple_placeholder_is_rejected(self):
        errors = validate_versions(versions(apple_build=311))

        self.assertEqual(len(errors), 1)
        self.assertIn("fixed development placeholder", errors[0])
        self.assertIn("do not bump it", errors[0])

    def test_each_apple_target_must_stamp_the_derived_build_number(self):
        for changes in ({"apple_stamp_phases": 1}, {"apple_runs_build_number": 1}):
            with self.subTest(changes):
                errors = validate_versions(versions(**changes))

                self.assertEqual(len(errors), 1)
                self.assertIn("CFBundleVersion from scripts/build-number", errors[0])

    def test_both_targets_still_stamp_the_version_references(self):
        errors = validate_versions(
            versions(apple_marketing_references=1, apple_build_references=1)
        )

        self.assertEqual(len(errors), 2)

    def test_the_fixtures_read_back_as_a_valid_tree(self):
        files = {
            "Cargo.toml": '[workspace]\n[workspace.package]\nversion = "0.2.2"\n',
            "clients/apple/project.yml": apple_project(),
            "clients/android/app/build.gradle.kts": android_gradle(),
        }
        self.assertEqual(validate_versions(read_versions(files.__getitem__)), ())
        files["clients/apple/project.yml"] = apple_project(stamps=1)
        self.assertTrue(validate_versions(read_versions(files.__getitem__)))
        files["clients/apple/project.yml"] = apple_project()
        files["clients/android/app/build.gradle.kts"] = android_gradle("210")
        self.assertTrue(validate_versions(read_versions(files.__getitem__)))

    def test_each_mode_reads_its_own_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            seed_repository(root)
            for mode in ("all", "staged", "changed-from"):
                self.assertEqual(check_repository(root, mode=mode), (), mode)

            # A working-tree bump fails the working-tree check only.
            (root / "clients/apple/project.yml").write_text(
                apple_project(311), encoding="utf-8"
            )
            self.assertTrue(check_repository(root, mode="all"))
            self.assertEqual(check_repository(root, mode="staged"), ())

            # Staged, it fails the index check; committed, the HEAD check.
            git(root, "add", "clients/apple/project.yml")
            self.assertTrue(check_repository(root, mode="staged"))
            self.assertEqual(check_repository(root, mode="changed-from"), ())
            git(root, "commit", "-qm", "bump")
            self.assertTrue(check_repository(root, mode="changed-from"))

    def test_an_unknown_mode_is_rejected(self):
        with self.assertRaises(MobileVersionError):
            check_repository(ROOT, mode="nonsense")

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
        self.assertIn("mobile-version", check_ids("scripts/build-number"))
        self.assertNotIn(
            "mobile-version", check_ids("clients/apple/Tests/AppleClientTests.swift")
        )
        self.assertNotIn(
            "mobile-version",
            check_ids("clients/android/app/src/test/AppVersionTest.kt"),
        )


if __name__ == "__main__":
    unittest.main()
