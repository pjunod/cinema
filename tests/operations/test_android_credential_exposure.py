"""Android credential exposure contracts.

Executes the audit half of
`docs/clients/ANDROID-CREDENTIAL-EXPOSURE-AND-RELEASE-BUILD.md` (review rows
D5 and D6-release). Everything here reads client source text, because no
Android SDK or Gradle toolchain runs in this suite: these cases cannot tell a
working build from a broken one, but each one fails the moment the exposure it
describes is reintroduced, which is the property the plan asks for.

Three exposures, three sections:

* the account bearer in Google's backup and in a device-to-device transfer
  (`§3.2`), pinned against both rule files *and* against the store that
  actually holds the token, so moving the token out from under `datastore/`
  fails here rather than silently leaving the exclusion pointing at nothing;
* a release APK signed with the debug key or falling back to it (`§3.5`);
* the shipping path publishing a debuggable build (`§3.5`).
"""

from __future__ import annotations

from pathlib import Path
import re
import unittest
import xml.etree.ElementTree as ET


ROOT = Path(__file__).resolve().parents[2]
ANDROID = ROOT / "clients/android"
RES_XML = ANDROID / "app/src/main/res/xml"
MANIFEST = ANDROID / "app/src/main/AndroidManifest.xml"
GRADLE = ANDROID / "app/build.gradle.kts"
SETTINGS_STORE = ANDROID / "app/src/main/java/tv/plurx/app/data/SettingsStore.kt"

# The directory the Preferences DataStore writes under the app's `file`
# backup domain. `preferencesDataStore(name = "plurx")` produces
# `files/datastore/plurx.preferences_pb`; the exclusion names the directory so
# a second store added later is covered without a second rule.
CREDENTIAL_DIR = "datastore/"

# Every section of `data-extraction-rules` that moves bytes off the device.
# `cloud-backup` alone leaves device-to-device transfer free to copy the same
# credential to a new handset (F-android-7).
TRANSFER_SECTIONS = ("cloud-backup", "device-transfer")


def _excluded_file_paths(element: ET.Element) -> set[str]:
    """The `domain="file"` paths this element excludes.

    A rule with any other domain is not a file exclusion: a package-name rule
    does not exclude a file, and neither does `domain="sharedpref"`. Only
    `file` rules count here, which is what makes a plausible-looking wrong
    rule fail instead of pass.
    """
    return {
        child.get("path", "")
        for child in element.findall("exclude")
        if child.get("domain") == "file"
    }


class AndroidBackupExclusionCase(unittest.TestCase):
    """The bearer is excluded from backup and transfer, in both rule files."""

    def test_legacy_backup_rules_exclude_the_credential_store(self) -> None:
        root = ET.parse(RES_XML / "backup_rules.xml").getroot()
        self.assertEqual(
            root.tag,
            "full-backup-content",
            "backup_rules.xml is the API <= 30 full-backup document",
        )
        self.assertIn(
            CREDENTIAL_DIR,
            _excluded_file_paths(root),
            "backup_rules.xml must exclude the DataStore directory holding the "
            "bearer token; without it Auto Backup copies an account credential "
            "to Google on every API <= 30 device",
        )

    def test_extraction_rules_exclude_the_store_from_every_transfer(self) -> None:
        root = ET.parse(RES_XML / "data_extraction_rules.xml").getroot()
        self.assertEqual(root.tag, "data-extraction-rules")
        sections = {child.tag for child in root}
        self.assertEqual(
            sections,
            set(TRANSFER_SECTIONS),
            "both transfer kinds must be declared: an absent section is "
            "governed by the platform default, not by this file",
        )
        for name in TRANSFER_SECTIONS:
            section = root.find(name)
            self.assertIsNotNone(section, f"{name} section is missing")
            self.assertIn(
                CREDENTIAL_DIR,
                _excluded_file_paths(section),
                f"<{name}> must exclude {CREDENTIAL_DIR}: excluding only one "
                "transfer kind leaves the other free to copy the bearer",
            )

    def test_both_rule_files_agree(self) -> None:
        """The two files are one policy expressed twice, for two API ranges.

        A device running API <= 30 reads only the first and a device on API >=
        31 only the second, so an exclusion added to one and forgotten in the
        other protects half the fleet. Compare the sets rather than listing the
        expected paths, so a future exclusion inherits the rule.
        """
        legacy = _excluded_file_paths(
            ET.parse(RES_XML / "backup_rules.xml").getroot()
        )
        modern = ET.parse(RES_XML / "data_extraction_rules.xml").getroot()
        for name in TRANSFER_SECTIONS:
            self.assertEqual(
                _excluded_file_paths(modern.find(name)),
                legacy,
                f"<{name}> and backup_rules.xml exclude different file paths",
            )

    def test_the_manifest_still_wires_both_rule_files(self) -> None:
        """Unreferenced rule files exclude nothing.

        Dropping either attribute from the manifest restores the default
        (back everything up) while leaving both XML files in the tree looking
        correct, so the wiring is part of the contract.
        """
        manifest = MANIFEST.read_text(encoding="utf-8")
        self.assertIn(
            'android:fullBackupContent="@xml/backup_rules"',
            manifest,
            "the API <= 30 rules must be referenced or they are dead XML",
        )
        self.assertIn(
            'android:dataExtractionRules="@xml/data_extraction_rules"',
            manifest,
            "the API >= 31 rules must be referenced or they are dead XML",
        )

    def test_the_bearer_still_lives_under_the_excluded_directory(self) -> None:
        """The exclusion is only worth what the storage choice makes it worth.

        `preferencesDataStore(name = …)` is what puts the token under
        `files/datastore/`. Moving the credential to `getSharedPreferences`,
        to a plain file, or to a DataStore built with an explicit producer
        elsewhere would leave both rule files pointing at a directory the
        bearer no longer occupies, and every case above would still pass. This
        one would not.
        """
        source = SETTINGS_STORE.read_text(encoding="utf-8")
        self.assertRegex(
            source,
            r"by\s+preferencesDataStore\(name\s*=\s*\"plurx\"\)",
            "SettingsStore must keep using the Preferences DataStore delegate; "
            f"anything else moves the token out from under {CREDENTIAL_DIR}",
        )
        self.assertRegex(
            source,
            r"val TOKEN = stringPreferencesKey\(\"token\"\)",
            "the bearer must still be a key of that DataStore; if it moved, "
            "the backup exclusions no longer cover it",
        )
        self.assertNotIn(
            "getSharedPreferences",
            source,
            "SharedPreferences lives under the `sharedpref` backup domain, "
            "which neither rule file excludes",
        )


class AndroidReleaseSigningCase(unittest.TestCase):
    """A release build is signed with a real key or it does not build."""

    def _release_block(self) -> str:
        """The body of `buildTypes { release { … } }`, brace-matched."""
        gradle = GRADLE.read_text(encoding="utf-8")
        start = gradle.index("release {")
        depth = 0
        for index in range(start, len(gradle)):
            if gradle[index] == "{":
                depth += 1
            elif gradle[index] == "}":
                depth -= 1
                if depth == 0:
                    return gradle[start : index + 1]
        raise AssertionError("unterminated release build type in build.gradle.kts")

    def test_the_release_build_type_selects_a_signing_config(self) -> None:
        self.assertIn(
            "signingConfig = signingConfigs.getByName(\"release\")",
            self._release_block(),
            "without an explicit signingConfig, `assembleRelease` produces an "
            "unsigned APK (which no device installs) or, where a debug config "
            "is in scope, a debug-signed one (which Play refuses and which no "
            "properly signed build can later upgrade in place)",
        )

    def test_the_release_signing_config_reads_every_credential_from_the_environment(
        self,
    ) -> None:
        gradle = GRADLE.read_text(encoding="utf-8")
        for variable in (
            "PLURX_ANDROID_KEYSTORE",
            "PLURX_ANDROID_KEYSTORE_PASSWORD",
            "PLURX_ANDROID_KEY_ALIAS",
            "PLURX_ANDROID_KEY_PASSWORD",
        ):
            self.assertIn(
                variable,
                gradle,
                f"{variable} is how the signing material reaches Gradle; "
                "keystores and passwords are never tracked in the repository",
            )

    def test_the_release_build_never_falls_back_to_the_debug_key(self) -> None:
        """A silent fallback is worse than a failure.

        A debug-signed "release" installs fine on a test device, so the
        mistake is invisible until the first properly signed build refuses to
        upgrade it and every device in the fleet needs an uninstall. The build
        must fail while the mistake is still cheap.
        """
        release = self._release_block()
        self.assertNotIn(
            'signingConfigs.getByName("debug")',
            release,
            "the release build type must never select the debug signing config",
        )
        gradle = GRADLE.read_text(encoding="utf-8")
        self.assertRegex(
            gradle,
            r"error\(|throw GradleException|IllegalStateException",
            "an absent credential must fail the build loudly, naming the "
            "variables, rather than producing an unsigned artifact",
        )

    def test_the_signing_failure_names_the_variables_it_needs(self) -> None:
        """The error message is the documentation people actually read."""
        gradle = GRADLE.read_text(encoding="utf-8")
        message = re.search(
            r"fun\s+requiredSigningValue\b.*?^}", gradle, re.DOTALL | re.MULTILINE
        )
        self.assertIsNotNone(
            message,
            "build.gradle.kts must keep one helper that resolves a signing "
            "credential or fails naming it",
        )
        self.assertIn(
            "$name",
            message.group(0),
            "the failure must interpolate the missing variable's name",
        )

    def test_every_gradle_entry_point_names_the_tasks_it_runs(self) -> None:
        """The signing guard keys on the requested task names.

        `requiredSigningValue` is reached only when a requested task name
        contains "Release", because `signingConfigs { }` is evaluated on every
        invocation and debug builds must keep working without a key. That is
        exact only while every entry point names its tasks: an aggregate like
        `./gradlew build` reaches the release variant without the word
        "Release" anywhere in the start parameter, which would leave the
        signing config unpopulated. Pin the premise rather than trusting it.
        """
        makefile = (ROOT / "Makefile").read_text(encoding="utf-8")
        invocations = re.findall(r"\./gradlew\s+([^\n]*)", makefile)
        self.assertTrue(invocations, "no gradlew invocations found in the Makefile")
        for invocation in invocations:
            tasks = [
                word
                for word in invocation.split()
                if not word.startswith("-") and word != "\\"
            ]
            self.assertTrue(
                tasks,
                f"gradlew invocation names no task at all: {invocation!r}",
            )
            for task in tasks:
                self.assertNotIn(
                    task.rsplit(":", 1)[-1],
                    {"build", "assemble", "install", "publish", "check"},
                    f"aggregate task {task!r} reaches the release variant "
                    "without a 'Release' task name; the signing config would "
                    "be left unpopulated",
                )


class AndroidShippingVariantCase(unittest.TestCase):
    """What reaches a device is the release variant, not the debug one."""

    def _makefile_recipe(self, target: str) -> str:
        makefile = (ROOT / "Makefile").read_text(encoding="utf-8")
        match = re.search(
            rf"^{re.escape(target)}:[^\n]*\n((?:\t[^\n]*\n|\n)*)",
            makefile,
            re.MULTILINE,
        )
        self.assertIsNotNone(match, f"Makefile has no {target} target")
        return match.group(1)

    def test_android_publish_no_longer_serves_a_debug_apk(self) -> None:
        """`/download/plurx-android.apk` is the fleet's install source.

        While it served `app-debug.apk`, every installed device was
        `debuggable`, which is what lets any host the device trusts run
        `adb shell run-as tv.plurx.app cat files/datastore/plurx.preferences_pb`
        and read the bearer straight out of the store the backup rules above
        protect from Google.
        """
        recipe = self._makefile_recipe("android-publish")
        self.assertNotIn(
            "app-debug.apk",
            recipe,
            "android-publish must copy the release APK, not the debug one",
        )
        self.assertIn(
            "apk/release/app-release.apk",
            recipe,
            "android-publish must publish the signed release artifact",
        )

    def test_android_publish_depends_on_the_release_build(self) -> None:
        makefile = (ROOT / "Makefile").read_text(encoding="utf-8")
        self.assertRegex(
            makefile,
            r"(?m)^android-publish:\s*android-release\b",
            "android-publish must build the release variant it copies; "
            "depending on the debug target would publish a stale or absent file",
        )

    def test_a_release_target_exists_and_assembles_the_release_variant(self) -> None:
        recipe = self._makefile_recipe("android-release")
        self.assertIn(
            ":app:assembleRelease",
            recipe,
            "android-release must run the release assemble task",
        )
        for variable in (
            "PLURX_ANDROID_KEYSTORE",
            "PLURX_ANDROID_KEYSTORE_PASSWORD",
            "PLURX_ANDROID_KEY_ALIAS",
            "PLURX_ANDROID_KEY_PASSWORD",
        ):
            self.assertIn(
                variable,
                recipe,
                f"{variable} must be forwarded into the build container; "
                "the Gradle build reads it from the environment",
            )

    def test_the_debug_build_target_still_exists_and_says_it_is_debug(self) -> None:
        """Local debug builds stay; they just stop being what ships."""
        makefile = (ROOT / "Makefile").read_text(encoding="utf-8")
        match = re.search(r"^android:[^\n]*##([^\n]*)", makefile, re.MULTILINE)
        self.assertIsNotNone(match, "the local debug build target must remain")
        self.assertIn(
            "debug",
            match.group(1).lower(),
            "the `android` target's help text must say it builds a debug APK, "
            "so nobody reaches for it expecting a shippable artifact",
        )


if __name__ == "__main__":
    unittest.main()
