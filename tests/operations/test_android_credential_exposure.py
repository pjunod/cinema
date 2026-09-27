"""Android credential exposure contracts.

Executes the audit half of
`docs/clients/ANDROID-CREDENTIAL-EXPOSURE-AND-RELEASE-BUILD.md` (review rows
D5 and D6-release). Everything here reads client source text, because no
Android SDK or Gradle toolchain runs in this suite: these cases cannot tell a
working build from a broken one, but each one fails the moment the exposure it
describes is reintroduced, which is the property the plan asks for.

Four boundaries, four sections:

* the mobile book buttons must keep account URLs inside Plurx rather than
  putting them in an external reader's history;
* the account bearer in Google's backup and in a device-to-device transfer
  (`§3.2`), pinned against both rule files *and* against the store that
  actually holds the token, so moving the token out from under `datastore/`
  fails here rather than silently leaving the exclusion pointing at nothing;
* a release APK signed with the debug key or falling back to it (`§3.5`);
* a shipping path — `make android-publish` or the `scripts/ship-physical`
  fallback — putting a debuggable build on a device, or publishing a release
  build without keeping the R8 mapping that de-obfuscates it (`§3.5`).
"""

from __future__ import annotations

import os
from pathlib import Path
import re
import runpy
import stat
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET


ROOT = Path(__file__).resolve().parents[2]
ANDROID = ROOT / "clients/android"
RES_XML = ANDROID / "app/src/main/res/xml"
MANIFEST = ANDROID / "app/src/main/AndroidManifest.xml"
GRADLE = ANDROID / "app/build.gradle.kts"
SHIP_PHYSICAL = ROOT / "scripts/ship-physical"
SIGNING_VARIABLES = (
    "PLURX_ANDROID_KEYSTORE",
    "PLURX_ANDROID_KEYSTORE_PASSWORD",
    "PLURX_ANDROID_KEY_ALIAS",
    "PLURX_ANDROID_KEY_PASSWORD",
    "PLURX_ANDROID_OLD_KEYSTORE",
    "PLURX_ANDROID_OLD_KEYSTORE_PASSWORD",
    "PLURX_ANDROID_OLD_KEY_ALIAS",
    "PLURX_ANDROID_OLD_KEY_PASSWORD",
    "PLURX_ANDROID_LINEAGE",
    "PLURX_ANDROID_RELEASE_CERT_SHA256",
)
SETTINGS_STORE = ANDROID / "app/src/main/java/tv/plurx/app/data/SettingsStore.kt"
ANDROID_DETAIL = ANDROID / "app/src/main/java/tv/plurx/app/ui/DetailScreen.kt"
ANDROID_PDF_READER = ANDROID / "app/src/main/java/tv/plurx/app/ui/PdfReaderScreen.kt"
APPLE_DETAIL = ROOT / "clients/apple/Sources/DetailView.swift"

# The directory the Preferences DataStore writes under the app's `file`
# backup domain. `preferencesDataStore(name = "plurx")` produces
# `files/datastore/plurx.preferences_pb`; the exclusion names the directory so
# a second store added later is covered without a second rule.
CREDENTIAL_DIR = "datastore/"

# Every section of `data-extraction-rules` that moves bytes off the device.
# `cloud-backup` alone leaves device-to-device transfer free to copy the same
# credential to a new handset (F-android-7).
TRANSFER_SECTIONS = ("cloud-backup", "device-transfer")


class BookReaderCredentialBoundaryCase(unittest.TestCase):
    """A future book button must not reinstate the account-URL export."""

    def test_mobile_book_actions_keep_account_urls_inside_plurx(self) -> None:
        android = ANDROID_DETAIL.read_text(encoding="utf-8")
        apple = APPLE_DETAIL.read_text(encoding="utf-8")
        self.assertIn("onReadPdf(playable.id, playable.size)", android)
        self.assertNotIn("Session.mediaUrl(", android)
        self.assertNotIn("Intent.ACTION_VIEW", android)
        self.assertNotIn("Session.shared.mediaURL(", apple)
        self.assertNotIn("openBookExternally", apple)

    def test_pdf_download_checks_selected_size_and_profile(self) -> None:
        """The selected edition and signed-in profile survive the download."""
        detail = ANDROID_DETAIL.read_text(encoding="utf-8")
        reader = ANDROID_PDF_READER.read_text(encoding="utf-8")
        self.assertIn("onReadPdf(playable.id, playable.size)", detail)
        self.assertIn("require(expectedSize in 1L..MAX_PDF_READER_BYTES)", reader)
        self.assertIn("if (temporary.length() != expectedSize)", reader)
        self.assertIn("Session.canonicalPrimaryOrigin() != origin || Session.token != token", reader)

    def test_pdf_page_error_is_local_and_scroll_resets_per_page(self) -> None:
        """A bad page stays in the reader and navigation begins at its top."""
        reader = ANDROID_PDF_READER.read_text(encoding="utf-8")
        render = reader.split("val page by produceState", 1)[1].split("\n    Column(", 1)[0]
        self.assertIn("document.renderer.openPage(pageIndex).use", render)
        self.assertIn("source.render(rendered", render)
        self.assertRegex(render, r"catch \(failure: Exception\) \{\s*bitmap\?\.recycle\(\)\s*throw failure")
        self.assertIn("catch (cancelled: CancellationException)", render)
        self.assertIn("Result.failure(failure)", render)
        self.assertRegex(reader, r"key\(pageIndex\) \{\s*Box\(\s*Modifier\.fillMaxSize\(\)\.verticalScroll\(rememberScrollState\(\)\)")
        self.assertIn("page?.isFailure == true", reader)
        self.assertIn('Text("Previous page")', reader)


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

        The Makefile is not the only entry point: `scripts/ship-physical`
        drives Gradle directly on the Mac, so every file under `scripts/` is
        scanned too.
        """
        sources = [ROOT / "Makefile"] + sorted(
            path for path in (ROOT / "scripts").rglob("*") if path.is_file()
        )
        invocations = []
        for source in sources:
            try:
                text = source.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                continue
            invocations += [
                (source.relative_to(ROOT), match)
                for match in re.findall(r"\./gradlew\s+([^\n]*)", text)
            ]
        self.assertTrue(invocations, "no gradlew invocations found")
        self.assertIn(
            Path("scripts/ship-physical"),
            {source for source, _ in invocations},
            "scripts/ship-physical runs Gradle and must be among the scanned "
            "entry points",
        )
        for source, invocation in invocations:
            tasks = [
                word.strip("\"'")
                for word in invocation.split()
                if not word.startswith("-") and word != "\\"
            ]
            self.assertTrue(
                tasks,
                f"{source}: gradlew invocation names no task at all: {invocation!r}",
            )
            for task in tasks:
                self.assertNotIn(
                    task.rsplit(":", 1)[-1],
                    {"build", "assemble", "install", "publish", "check"},
                    f"{source}: aggregate task {task!r} reaches the release variant "
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
        for variable in SIGNING_VARIABLES:
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


def _write_executable(path: Path, body: str) -> None:
    path.write_text(body, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def _clean_environment(**extra: str) -> dict[str, str]:
    """The caller's environment without signing material or outer-make state."""
    env = {
        name: value
        for name, value in os.environ.items()
        if name not in SIGNING_VARIABLES
        and name not in {"MAKEFLAGS", "MAKELEVEL", "MFLAGS", "MAKEOVERRIDES"}
    }
    env.update(extra)
    return env


class AndroidReleaseMakeTargetCase(unittest.TestCase):
    """`make android-release` / `android-publish`, run against a `docker` stub.

    These run the real recipes. `docker` is replaced by a stub on `PATH` that
    records its arguments, and `ANDROID_OUTPUTS` points `android-publish` at a
    fabricated build tree, so what is asserted is what the recipe actually
    hands Docker and actually leaves in the data directory.
    """

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        _write_executable(
            bin_dir / "docker",
            '#!/bin/sh\nprintf \'%s\\n\' "$@" >> "$DOCKER_LOG"\nexit 0\n',
        )
        self.docker_log = self.tmp / "docker.log"
        self.env = _clean_environment(
            PATH=f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}",
            DOCKER_LOG=str(self.docker_log),
            PLURX_ANDROID_IMAGE_READY="1",
            PLURX_ANDROID_KEYSTORE="plurx-upload.jks",
            PLURX_ANDROID_OLD_KEYSTORE="old-debug.jks",
            PLURX_ANDROID_LINEAGE="lineage.bin",
        )
        # PUBLISHING.md's `keytool` line leaves the keystore in the cwd under
        # this bare name; that is the case Docker mistakes for a volume name.
        (self.tmp / "plurx-upload.jks").write_bytes(b"not really a keystore")
        (self.tmp / "old-debug.jks").write_bytes(b"old signer fixture")
        (self.tmp / "lineage.bin").write_bytes(b"lineage fixture")
        self.outputs = self.tmp / "outputs"
        self.data = self.tmp / "data"
        self.data.mkdir()

    def _make(self, target: str, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["make", "-s", "-f", str(ROOT / "Makefile"), target, *args],
            cwd=self.tmp,
            env=self.env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=60,
        )

    def _build(self, version_code: int, apk: bytes, mapping: bytes | None) -> None:
        release = self.outputs / "apk/release"
        release.mkdir(parents=True, exist_ok=True)
        (release / "app-release.apk").write_bytes(apk)
        (release / "output-metadata.json").write_text(
            "{\n"
            '  "version": 3,\n'
            '  "variantName": "release",\n'
            '  "elements": [\n'
            "    {\n"
            '      "type": "SINGLE",\n'
            f'      "versionCode": {version_code},\n'
            '      "versionName": "0.3.0",\n'
            '      "outputFile": "app-release.apk"\n'
            "    }\n"
            "  ]\n"
            "}\n",
            encoding="utf-8",
        )
        mapping_file = self.outputs / "mapping/release/mapping.txt"
        mapping_file.parent.mkdir(parents=True, exist_ok=True)
        if mapping is None:
            mapping_file.unlink(missing_ok=True)
        else:
            mapping_file.write_bytes(mapping)

    def _publish(self) -> subprocess.CompletedProcess[str]:
        return self._make(
            "android-publish",
            f"ANDROID_OUTPUTS={self.outputs}",
            f"ANDROID_DATA_DIR={self.data}",
        )

    def test_a_relative_keystore_reaches_docker_as_an_absolute_bind_path(self) -> None:
        """Docker reads a non-absolute `-v` source as a named volume.

        Before the fix, `-v plurx-upload.jks:/signing/upload.jks:ro` created an
        empty named volume and mounted a *directory* at the keystore path, so
        the build failed on a keystore that existed on the host.
        """
        result = self._make("android-release")
        self.assertEqual(result.returncode, 0, result.stdout)
        words = self.docker_log.read_text(encoding="utf-8").splitlines()
        mounts = [
            words[index + 1]
            for index, word in enumerate(words[:-1])
            if word == "-v" and words[index + 1].endswith(":/signing/upload.jks:ro")
        ]
        self.assertEqual(len(mounts), 1, words)
        source = mounts[0].rsplit(":/signing/upload.jks:ro", 1)[0]
        self.assertTrue(
            os.path.isabs(source),
            f"docker -v source {source!r} is not absolute; Docker would treat it "
            "as a volume name and mount an empty directory",
        )
        self.assertEqual(
            os.path.realpath(source),
            os.path.realpath(self.tmp / "plurx-upload.jks"),
        )

    def test_publish_keeps_each_builds_mapping_keyed_by_version_code(self) -> None:
        """Plan §3.5 "Diagnostics retained" (F-android-12).

        The next Gradle run overwrites `mapping/release/mapping.txt`, so the
        mapping of a published build survives only if publishing keeps it.
        """
        self._build(120, b"apk-120", b"mapping-120")
        first = self._publish()
        self.assertEqual(first.returncode, 0, first.stdout)
        self._build(121, b"apk-121", b"mapping-121")
        second = self._publish()
        self.assertEqual(second.returncode, 0, second.stdout)

        self.assertEqual((self.data / "plurx-android.apk").read_bytes(), b"apk-121")
        self.assertEqual(
            (self.data / "plurx-android-120.mapping.txt").read_bytes(),
            b"mapping-120",
            "the earlier build's mapping must survive the next publish",
        )
        self.assertEqual(
            (self.data / "plurx-android-121.mapping.txt").read_bytes(),
            b"mapping-121",
        )

    def test_republishing_a_version_code_moves_the_earlier_mapping_aside(self) -> None:
        """Devices may still run the first build of a reused versionCode."""
        self._build(120, b"apk-a", b"mapping-a")
        self.assertEqual(self._publish().returncode, 0)
        self._build(120, b"apk-b", b"mapping-b")
        result = self._publish()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(
            (self.data / "plurx-android-120.mapping.txt").read_bytes(), b"mapping-b"
        )
        kept = sorted(self.data.glob("plurx-android-120.mapping.txt.*"))
        self.assertEqual(len(kept), 1, sorted(p.name for p in self.data.iterdir()))
        self.assertEqual(kept[0].read_bytes(), b"mapping-a")

    def test_publish_refuses_a_build_without_its_mapping(self) -> None:
        """A served APK always has its mapping: nothing is published without it."""
        self._build(120, b"apk-120", None)
        result = self._publish()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("no R8 mapping", result.stdout)
        self.assertFalse((self.data / "plurx-android.apk").exists())


class ShipPhysicalReleaseVariantCase(unittest.TestCase):
    """`scripts/ship-physical`, the documented no-Ansible device path.

    docs/PUBLISHING.md sends operators here when the controller cannot run the
    play, so it must put the same release variant on a device that
    `android-publish` serves — otherwise every device installed through it is
    still `debuggable` and `run-as` still reads the bearer.
    """

    def setUp(self) -> None:
        self.script = SHIP_PHYSICAL.read_text(encoding="utf-8")

    def _run(self, cwd: Path, **env: str) -> subprocess.CompletedProcess[str]:
        """Run the preconditions on Linux with `uname` reporting Darwin.

        `PLURX_REPO` points at a directory that is not a repository, so a run
        that gets past the preconditions stops at its first `git fetch`
        without touching anything.
        """
        bin_dir = cwd / "bin"
        bin_dir.mkdir(exist_ok=True)
        _write_executable(bin_dir / "uname", "#!/bin/sh\necho Darwin\n")
        return subprocess.run(
            [str(SHIP_PHYSICAL), "--android", "--dry-run"],
            cwd=cwd,
            env=_clean_environment(
                PATH=f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}",
                PLURX_ADB="/bin/true",
                PLURX_REPO=str(cwd / "not-a-repo"),
                PLURX_RELEASE_ROOT=str(cwd / "release"),
                **env,
            ),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=60,
        )

    def test_it_builds_and_installs_the_release_apk(self) -> None:
        self.assertIn("./gradlew --no-daemon :app:assembleRelease", self.script)
        self.assertIn(
            'APK="$ANDROID_DIR/app/build/outputs/apk/release/app-release.apk"',
            self.script,
        )
        self.assertNotIn("assembleDebug", self.script)
        self.assertNotIn("app-debug.apk", self.script)

    def test_it_refuses_to_start_without_each_signing_input(self) -> None:
        """No debug fallback: a missing input stops the run before any work."""
        with tempfile.TemporaryDirectory() as raw:
            tmp = Path(raw)
            keystore = tmp / "upload.jks"
            keystore.write_bytes(b"x")
            old_keystore = tmp / "old.jks"
            old_keystore.write_bytes(b"x")
            lineage = tmp / "lineage.bin"
            lineage.write_bytes(b"x")
            full = {
                "PLURX_ANDROID_KEYSTORE": str(keystore),
                "PLURX_ANDROID_KEYSTORE_PASSWORD": "p",
                "PLURX_ANDROID_KEY_ALIAS": "a",
                "PLURX_ANDROID_KEY_PASSWORD": "k",
                "PLURX_ANDROID_OLD_KEYSTORE": str(old_keystore),
                "PLURX_ANDROID_OLD_KEYSTORE_PASSWORD": "p",
                "PLURX_ANDROID_OLD_KEY_ALIAS": "a",
                "PLURX_ANDROID_OLD_KEY_PASSWORD": "k",
                "PLURX_ANDROID_LINEAGE": str(lineage),
                "PLURX_ANDROID_RELEASE_CERT_SHA256": "0" * 64,
            }
            for missing in SIGNING_VARIABLES:
                env = {k: v for k, v in full.items() if k != missing}
                result = self._run(tmp, **env)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn(f"{missing} is required", result.stdout)
                self.assertNotIn("resolving origin/main", result.stdout)

    def test_a_relative_keystore_is_resolved_before_gradle_sees_it(self) -> None:
        """Gradle would resolve it against clients/android/app in the worktree."""
        with tempfile.TemporaryDirectory() as raw:
            tmp = Path(raw)
            (tmp / "plurx-upload.jks").write_bytes(b"x")
            (tmp / "old.jks").write_bytes(b"x")
            (tmp / "lineage.bin").write_bytes(b"x")
            result = self._run(
                tmp,
                PLURX_ANDROID_KEYSTORE="plurx-upload.jks",
                PLURX_ANDROID_KEYSTORE_PASSWORD="p",
                PLURX_ANDROID_KEY_ALIAS="a",
                PLURX_ANDROID_KEY_PASSWORD="k",
                PLURX_ANDROID_OLD_KEYSTORE="old.jks",
                PLURX_ANDROID_OLD_KEYSTORE_PASSWORD="p",
                PLURX_ANDROID_OLD_KEY_ALIAS="a",
                PLURX_ANDROID_OLD_KEY_PASSWORD="k",
                PLURX_ANDROID_LINEAGE="lineage.bin",
                PLURX_ANDROID_RELEASE_CERT_SHA256="0" * 64,
            )
            self.assertIn(
                "Android release signing keystore: "
                + os.path.realpath(tmp / "plurx-upload.jks"),
                result.stdout,
            )

    def test_the_lineage_and_exact_signers_are_checked_before_any_device(self) -> None:
        helper = (ROOT / "scripts/sign-android-release").read_text(encoding="utf-8")
        self.assertIn('python3 "$WORKTREE/scripts/sign-android-release"', self.script)
        self.assertIn('if hashlib.sha256(old_der).hexdigest() != OLD_CERT_SHA256:', helper)
        self.assertIn('if cert_digest(apksigner, APK, 28) != expected_new:', helper)
        self.assertIn('if cert_digest(apksigner, signed, api) != expected_new:', helper)
        self.assertNotIn('CN=Android Debug', self.script)

    def test_an_equal_version_code_still_installs_the_exact_main_artifact(self) -> None:
        """A version number does not identify the source tree or signer."""
        self.assertNotIn('already on versionCode $ANDROID_CODE', self.script)
        self.assertIn('install --no-streaming -r "$APK"', self.script)
        self.assertIn('if [[ $debuggable -eq 1 ]]', self.script)

    def test_a_signer_mismatch_never_uninstalls_the_existing_app(self) -> None:
        """A rejected rotation must preserve sign-in and offline downloads."""
        branch = self.script.split("*INSTALL_FAILED_UPDATE_INCOMPATIBLE*", 1)
        self.assertEqual(len(branch), 2, "the signer-mismatch branch is missing")
        message = branch[1].split("else", 1)[0]
        self.assertNotIn("debug keystore changed", message)
        self.assertIn("signing lineage was not accepted", message)
        self.assertIn("keeping the existing install and app data", message)
        self.assertNotIn(" uninstall ", message)


class AndroidLineageCapabilityCase(unittest.TestCase):
    """A rotated APK must keep the installed app's data capability."""

    def test_old_signer_without_installed_data_is_rejected(self) -> None:
        helper = runpy.run_path(str(ROOT / "scripts/sign-android-release"))
        validate = helper["validate_lineage"]
        old = helper["OLD_CERT_SHA256"]
        new = "a" * 64
        report = (
            "Signer #1 in lineage certificate DN: Old\n"
            f"Signer #1 in lineage certificate SHA-256 digest: {old}\n"
            "Has installed data capability: false\n"
            "Has rollback capability: false\n"
            "Signer #2 in lineage certificate DN: New\n"
            f"Signer #2 in lineage certificate SHA-256 digest: {new}\n"
            "Has installed data capability: true\n"
        )
        validate.__globals__["run"] = lambda *args, **kwargs: report.encode()
        with self.assertRaisesRegex(SystemExit, "cannot preserve installed data"):
            validate("apksigner", Path("lineage.bin"), new)


if __name__ == "__main__":
    unittest.main()
