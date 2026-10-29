"""Desktop release cohort, exact-byte signature and installer policy regressions.

All assets/keys here are disposable fixtures, never production update trust.
"""
import argparse
import copy
import gzip
import hashlib
import importlib.machinery
import importlib.util
import json
import os
import plistlib
import re
from pathlib import Path
import shutil
import subprocess
import struct
import tempfile
import textwrap
import tarfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("desktop_manifest", str(ROOT / "scripts/create-desktop-update-manifest"))
spec = importlib.util.spec_from_loader(loader.name, loader)
MANIFEST = importlib.util.module_from_spec(spec)
loader.exec_module(MANIFEST)


class DesktopReleaseCase(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="desktop-release-fixture-", dir=ROOT / "target")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.args = argparse.Namespace(assets=self.root, output=self.root / MANIFEST.MANIFEST,
            version="1.2.3", sequence=123, source_commit="a" * 40, source_tree="b" * 40,
            packaging_commit="c" * 40, created_unix=1792400000, expires_unix=1792400000 + 86400,
            upgrade_from=[">=1.0.0, <1.2.3"], data_compatibility="test-data-v1",
            cluster_compatibility="test-cluster-v1", minimum_controller="1.0.0", rollback="manual_recovery",
            signing_key=None, inspect_packages=False)
        for identity in MANIFEST.expected(self.args.version):
            (self.root / identity["name"]).write_bytes(identity["name"].encode())

    def generate(self):
        MANIFEST.create(self.args)
        return json.loads(self.args.output.read_bytes())

    def test_desktop_autostart_basename_matches_user_override(self):
        specification = (ROOT / "deploy/packaging/desktop-nfpm.yaml").read_text()
        pair = re.search(r"src: \./([^\n]+autostart\.desktop)\n\s+dst: (/etc/xdg/autostart/[^\n]+)", specification)
        self.assertIsNotNone(pair)
        self.assertEqual(pair.group(1), "tv.plurx.Server-autostart.desktop")
        self.assertEqual(Path(pair.group(2)).name, pair.group(1),
                         "XDG Hidden=true overrides require the same system/user basename")

    def test_macos_native_archive_is_canonical_ustar_without_hidden_pax_headers(self):
        loader = importlib.machinery.SourceFileLoader("desktop_archive_writer", str(ROOT / "scripts/package-macos"))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        tool = importlib.util.module_from_spec(spec)
        loader.exec_module(tool)
        package = self.root / "package"
        (package / "runtime").mkdir(parents=True)
        (package / "runtime/plurxd").write_bytes(b"native archive format fixture")
        (package / "runtime/plurxd").chmod(0o755)
        # Fractional filesystem mtimes would make Python's default PAX format emit extension headers.
        os.utime(package / "runtime/plurxd", ns=(1792400000123456789, 1792400000123456789))
        release = {"schema_version": 1, "repository": "pjunod/cinema", "version": "1.2.3",
                   "source_commit": "a" * 40, "architecture": "universal"}
        (package / "release.json").write_text(json.dumps(release))
        closure = {"schema_version": 1, "version": "1.2.3", "files": []}
        for member in sorted(package.rglob("*")):
            if member.is_file():
                closure["files"].append({"path": member.relative_to(package).as_posix(),
                    "size": member.stat().st_size, "sha256": hashlib.sha256(member.read_bytes()).hexdigest(),
                    "executable": bool(member.stat().st_mode & 0o111)})
        (package / "runtime-closure-v1.json").write_text(json.dumps(closure))
        name = "plurxd-1.2.3-macos-universal.tar.gz"
        left, right = self.root / "left" / name, self.root / "right" / name
        tool.archive_native(package, left, 1792400000)
        tool.archive_native(package, right, 1792400000)
        self.assertEqual(left.read_bytes(), right.read_bytes())
        raw = gzip.decompress(left.read_bytes())
        offset, headers = 0, []
        while raw[offset:offset + 512].strip(b"\0"):
            header = raw[offset:offset + 512]
            self.assertEqual(header[257:263], b"ustar\0")
            self.assertIn(header[156:157], (b"0", b"5", b"\0"))
            size = int(header[124:136].strip(b"\0 ") or b"0", 8)
            headers.append(header[:100].split(b"\0", 1)[0].decode())
            offset += 512 + ((size + 511) // 512) * 512
        self.assertTrue(headers)
        self.assertTrue(all(name.startswith("plurxd-1.2.3-macos-universal") for name in headers))
        with tarfile.open(left) as archive:
            daemon = archive.getmember("plurxd-1.2.3-macos-universal/runtime/plurxd")
            self.assertTrue(daemon.mode & 0o111)
            self.assertEqual(archive.extractfile(daemon).read(), b"native archive format fixture")
        (package / "runtime/plurxd").write_bytes(b"tampered after closure")
        with self.assertRaisesRegex(ValueError, "closure bytes/mode mismatch"):
            tool.archive_native(package, self.root / "bad" / name, 1792400000)
        self.assertFalse((self.root / "bad" / name).exists())

    def test_desktop_package_ships_every_state_icon_used_by_the_tray(self):
        specification = (ROOT / "deploy/packaging/desktop-nfpm.yaml").read_text()
        package_script = (ROOT / "scripts/package-linux").read_text()
        for state in ("running", "starting", "stopped", "unhealthy", "unknown"):
            icon = f"tv.plurx.Server-{state}-symbolic.svg"
            self.assertIn("src: ./" + icon, specification)
            self.assertIn("dst: /usr/share/icons/hicolor/symbolic/apps/" + icon, specification)
        self.assertIn("for state in running starting stopped unhealthy unknown", package_script)
        self.assertIn('deploy/desktop/icons/tv.plurx.Server-$state-symbolic.svg', package_script)

    def test_registered_release_dispatch_reuses_only_a_named_dry_run_build(self):
        workflow = (ROOT / ".github/workflows/github-release.yml").read_text()
        job = workflow.split("\n  resolve:", 1)[1].split("\n  linux:", 1)[0]
        script = textwrap.dedent(job.split("        run: |\n", 1)[1].split("\n      - name:", 1)[0])
        output = self.root / "resolve-output"
        environment = {**os.environ, "EVENT_NAME": "workflow_dispatch", "INPUT_TAG": "",
            "INPUT_QUALIFICATION_RUN_ID": "123", "GITHUB_SHA": "a" * 40, "GITHUB_OUTPUT": str(output)}
        result = subprocess.run(["bash", "-c", script], cwd=self.root, env=environment, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output.read_text(), "mode=qualification\ncommit=" + "a" * 40 + "\n")
        self.assertFalse((self.root / "source").exists(), "qualification must not fetch or rebuild the source")
        for changed in ({"INPUT_TAG": "v1.2.3"}, {"INPUT_QUALIFICATION_RUN_ID": "123;touch unsafe"},
                        {"EVENT_NAME": "push", "GITHUB_REF": "refs/tags/v1.2.3", "GITHUB_REF_NAME": "v1.2.3"}):
            output.unlink(missing_ok=True)
            refused = subprocess.run(["bash", "-c", script], cwd=self.root, env={**environment, **changed},
                                     capture_output=True, text=True)
            self.assertNotEqual(refused.returncode, 0)
            self.assertFalse(output.exists())
            self.assertFalse((self.root / "source").exists())
        self.assertIn("uses: ./.github/workflows/desktop-qualification.yml", workflow)
        self.assertIn("      candidate_sha: ${{ github.sha }}", workflow)

    def test_manifest_is_complete_and_binds_distinct_source_and_packaging(self):
        value = self.generate()
        self.assertEqual(len(value["artifacts"]), 9)
        self.assertEqual(value["repository"], "pjunod/cinema")
        self.assertNotEqual(value["source_commit"], value["packaging_commit"])
        self.assertEqual(value["source_tree"], "b" * 40)
        for artifact in value["artifacts"]:
            self.assertEqual(MANIFEST.digest(self.root / artifact["name"]), {k: artifact[k] for k in ["size", "sha256"]})

    def test_missing_companion_and_symlinked_asset_refuse_before_feed(self):
        companion = self.root / "plurx-desktop_1.2.3-1_arm64.deb"
        companion.unlink()
        with self.assertRaises(ValueError):
            self.generate()
        self.assertFalse(self.args.output.exists())
        companion.symlink_to(self.root / "plurxd_1.2.3-1_arm64.deb")
        with self.assertRaisesRegex(ValueError, "regular release artifact"):
            self.generate()
        self.assertFalse(self.args.output.exists())

    def test_schema_rejects_mixed_cohort_duplicate_repository_expiry(self):
        value = self.generate()
        for mutate in [
            lambda m: m.update(repository="pjunod/plurx"),
            lambda m: m.update(expires_unix=m["created_unix"] + 91 * 86400),
            lambda m: m["artifacts"][0].update(package_version="1.2.2-1"),
            lambda m: m["artifacts"].append(copy.deepcopy(m["artifacts"][0])),
            lambda m: m.update(untrusted_extra=True),
        ]:
            with self.subTest(mutate=mutate):
                changed = copy.deepcopy(value)
                mutate(changed)
                with self.assertRaises(ValueError):
                    MANIFEST.validate(changed)

    def test_payload_metadata_must_match_signed_identity(self):
        identity = next(a for a in MANIFEST.expected("1.2.3") if a["format"] == "deb")
        with patch.object(MANIFEST.subprocess, "check_output", return_value="Package: plurx-desktop\nVersion: 9.0.0-1\nArchitecture: amd64\n"):
            with self.assertRaisesRegex(ValueError, "payload identity"):
                MANIFEST.inspect_artifact(self.root / identity["name"], identity)

    def test_exact_bytes_ed25519_signature_rejects_mutation(self):
        openssl = shutil.which("openssl")
        if not openssl:
            self.skipTest("OpenSSL Ed25519 fixture tool unavailable")
        key, public = self.root / "fixture-key.pem", self.root / "fixture-public.pem"
        generate = subprocess.run([openssl, "genpkey", "-algorithm", "ED25519", "-out", str(key)], capture_output=True)
        if generate.returncode:
            self.skipTest("host OpenSSL does not support Ed25519; Linux release job uses OpenSSL3")
        subprocess.run([openssl, "pkey", "-in", str(key), "-pubout", "-out", str(public)], check=True, capture_output=True)
        self.args.signing_key = key
        self.generate()
        signature = self.args.output.with_name(MANIFEST.MANIFEST + ".sig")
        self.assertEqual(signature.stat().st_size, 64)
        command = [openssl, "pkeyutl", "-verify", "-rawin", "-pubin", "-inkey", str(public), "-in", str(self.args.output), "-sigfile", str(signature)]
        self.assertEqual(subprocess.run(command, capture_output=True).returncode, 0)
        self.args.output.write_bytes(self.args.output.read_bytes() + b" ")
        self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)

    def test_production_signer_requires_pinned_identical_candidate_roots(self):
        openssl = shutil.which("openssl")
        if not openssl:
            self.skipTest("OpenSSL Ed25519 fixture tool unavailable")
        key = self.root / "fixture-production-test-key.pem"
        generated = subprocess.run([openssl, "genpkey", "-algorithm", "ED25519", "-out", str(key)], capture_output=True)
        if generated.returncode:
            self.skipTest("host OpenSSL does not support Ed25519")
        self.args.signing_key = key
        self.args.production = True
        packaging, candidate = self.root / "approved-fixture-roots.json", self.root / "candidate-fixture-roots.json"
        self.args.trusted_roots = [packaging, candidate]
        def roots(entries):
            return {"schema_version": 1, "repository": "pjunod/cinema", "keys": entries}
        def write(value):
            for path in [packaging, candidate]:
                path.write_text(json.dumps(value))
        write(roots([]))
        with self.assertRaisesRegex(ValueError, "absent, empty or malformed"):
            self.generate()
        self.assertFalse(self.args.output.exists())
        write(roots([{"id": "isolated-test-root", "public_key_hex": "00" * 32}]))
        with self.assertRaisesRegex(ValueError, "not pinned"):
            self.generate()
        self.assertFalse(self.args.output.exists())
        # Fixture files stay only in this disposable test directory; no client
        # production root or repository trust file is modified by this test.
        approved = roots([{"id": "isolated-test-root", "public_key_hex": MANIFEST.signing_public_key(key)}])
        write(approved)
        candidate.write_text(json.dumps(roots([approved["keys"][0] | {"id": "different-bridge"}])))
        with self.assertRaisesRegex(ValueError, "production trust roots differ"):
            self.generate()
        self.assertFalse(self.args.output.exists())
        write(approved)
        self.generate()
        self.assertEqual(self.args.output.with_name(MANIFEST.MANIFEST + ".sig").stat().st_size, 64)

    def test_macos_desktop_closure_covers_all_runtime_control_and_app_bytes(self):
        loader = importlib.machinery.SourceFileLoader("desktop_macos_package", str(ROOT / "scripts/package-macos"))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        tool = importlib.util.module_from_spec(spec)
        loader.exec_module(tool)
        stage = self.root / "native-stage"
        (stage / "bin").mkdir(parents=True)
        (stage / "provenance").mkdir()
        (stage / "bin/plurxd").write_bytes(b"daemon fixture")
        controls = self.root / "built-controls"
        controls.mkdir()
        names = ["plurx-control", "plurx-install-helper", "plurx-update-worker", "plurx-service-launch", "plurx-recovery"]
        for name in names:
            (controls / name).write_bytes(name.encode())
        app = self.root / "Plurx Server.app"
        (app / "Contents/MacOS").mkdir(parents=True)
        (app / "Contents/MacOS/PlurxServer").write_bytes(b"native app fixture")
        (app / "Contents/Info.plist").write_bytes(plistlib.dumps({"CFBundleShortVersionString": "1.2.3", "CFBundleIdentifier": "tv.plurx.Server", "CFBundleExecutable": "PlurxServer"}))
        with patch.object(tool.subprocess, "check_output", return_value=b"x86_64 arm64\n"), patch.object(tool.subprocess, "run"):
            tool.attach_desktop(stage, app, controls, "1.2.3", "a" * 40, "-")
        closure = json.loads((stage / "runtime-closure-v1.json").read_bytes())
        self.assertEqual(closure["version"], "1.2.3")
        files = {entry["path"]: entry for entry in closure["files"]}
        self.assertIn("runtime/plurxd", files)
        self.assertIn("install", files)
        self.assertIn("Plurx Server.app/Contents/Info.plist", files)
        self.assertIn("Plurx Server.app/Contents/MacOS/plurx-control", files)
        self.assertEqual((stage / "Plurx Server.app/Contents/MacOS/plurx-control").read_bytes(), (stage / "control/plurx-control").read_bytes())
        for name in names:
            self.assertIn("control/" + name, files)
            self.assertTrue(files["control/" + name]["executable"])
        actual = {p.relative_to(stage).as_posix() for p in stage.rglob("*") if p.is_file()} - {"runtime-closure-v1.json"}
        self.assertEqual(set(files), actual)
        for relative, entry in files.items():
            self.assertEqual(MANIFEST.digest(stage / relative), {"size": entry["size"], "sha256": entry["sha256"]})

    def test_macos_app_directory_link_cannot_redirect_embedded_controller_copy(self):
        loader = importlib.machinery.SourceFileLoader("desktop_app_paths", str(ROOT / "scripts/package-macos"))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        tool = importlib.util.module_from_spec(spec)
        loader.exec_module(tool)
        app = self.root / "Plurx Server.app"
        app.mkdir()
        outside = self.root / "outside"
        outside.mkdir()
        (app / "Contents").symlink_to(outside, target_is_directory=True)
        stage = self.root / "stage"
        (stage / "bin").mkdir(parents=True)
        with self.assertRaisesRegex(ValueError, "app contains a link"):
            tool.attach_desktop(stage, app, self.root, "1.2.3", "a" * 40, "-")
        self.assertEqual(list(outside.iterdir()), [])
        self.assertTrue((stage / "bin").is_dir(), "input validation precedes even the local runtime rename")

    def test_macos_distribution_requires_real_signature_team_notary_and_staple_checks(self):
        loader = importlib.machinery.SourceFileLoader("desktop_distribution", str(ROOT / "scripts/package-macos"))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        tool = importlib.util.module_from_spec(spec)
        loader.exec_module(tool)
        # Universal dylibs use the same all-member scan; parse their native
        # file types rather than relying on an executable bit or filename.
        fat = self.root / "fat-macho-fixture"
        image = struct.pack("<8I", 0xFEEDFACF, 0x0100000C, 0, 6, 0, 0, 0, 0)
        fat.write_bytes(b"\xca\xfe\xba\xbe" + struct.pack(">I", 1) + struct.pack(">5I", 0x0100000C, 0, 28, len(image), 2) + image)
        self.assertTrue(tool.loadable_macho(fat))
        fat.write_bytes(fat.read_bytes()[:-1])
        with self.assertRaisesRegex(ValueError, "slice escapes file"):
            tool.loadable_macho(fat)
        package = self.root / "distribution-fixture"
        package.mkdir()
        (package / "release.json").write_text(json.dumps({"version": "1.2.3"}))
        required = ["runtime/plurxd", "runtime/ffmpeg", "runtime/ffprobe", "runtime/subtitle-engine/whisper-cli", "Plurx Server.app/Contents/Info.plist"]
        required += ["control/" + name for name in ["plurx-control", "plurx-install-helper", "plurx-update-worker", "plurx-service-launch", "plurx-recovery"]]
        for name in required:
            target = package / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(b"distribution verification fixture")
            if not name.endswith("Info.plist"):
                target.chmod(0o755)
        dylib = package / "runtime/non-executable.dylib"
        dylib.write_bytes(struct.pack("<8I", 0xFEEDFACF, 0x0100000C, 0, 6, 0, 0, 0, 0))
        dwarf = package / "debug/plurxd.dSYM/Contents/Resources/DWARF/plurxd"
        dwarf.parent.mkdir(parents=True)
        dwarf.write_bytes(struct.pack("<8I", 0xFEEDFACF, 0x0100000C, 0, 10, 0, 0, 0, 0))
        closure = {"schema_version": 1, "version": "1.2.3", "files": []}
        for target in sorted(package.rglob("*")):
            if target.is_file():
                closure["files"].append({"path": target.relative_to(package).as_posix(), **MANIFEST.digest(target), "executable": bool(target.stat().st_mode & 0o111)})
        (package / "runtime-closure-v1.json").write_text(json.dumps(closure))
        calls = []
        def observed(command, **kwargs):
            calls.append(command)
            stderr = b""
            if command[0].endswith("codesign") and "-d" in command:
                stderr = b"Authority=Developer ID Application: Fixture\nTeamIdentifier=TEAM123456\nCodeDirectory v=20500 size=100 flags=0x10000(runtime) hashes=1+7 location=embedded\n"
            if command[0].endswith("spctl"):
                stderr = b"accepted\nsource=Notarized Developer ID\n"
            stdout = plistlib.dumps({"com.apple.security.cs.allow-unsigned-executable-memory": True}) if "--entitlements" in command else b""
            return subprocess.CompletedProcess(command, 0, stdout=stdout, stderr=stderr)
        with patch.object(tool.subprocess, "run", side_effect=observed):
            tool.verify_distribution(package, "TEAM123456")
        self.assertTrue(any("--check-notarization" in call and str(dylib) in call for call in calls), "a non-executable dylib still needs signing/notarization")
        self.assertFalse(any(str(dwarf) in call for call in calls), "dSYM DWARF is non-loadable debug data")
        self.assertTrue(any("stapler" in call and "validate" in call for call in calls))
        self.assertTrue(any(call[0].endswith("spctl") for call in calls))
        with patch.object(tool.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, stdout=b"", stderr=b"Signature=adhoc\nTeamIdentifier=not set\n")):
            with self.assertRaisesRegex(ValueError, "expected Developer ID/team"):
                tool.verify_distribution(package, "TEAM123456")
        def without_runtime(command, **kwargs):
            result = observed(command, **kwargs)
            return subprocess.CompletedProcess(command, 0, stdout=result.stdout, stderr=b"Executable=/fixture/runtime/server\n" + result.stderr.replace(b"0x10000(runtime)", b"0x0(none)"))
        with patch.object(tool.subprocess, "run", side_effect=without_runtime):
            with self.assertRaisesRegex(ValueError, "Hardened Runtime"):
                tool.verify_distribution(package, "TEAM123456")
        with patch.object(tool.subprocess, "run", side_effect=subprocess.CalledProcessError(65, "codesign --check-notarization")):
            with self.assertRaises(subprocess.CalledProcessError):
                tool.verify_distribution(package, "TEAM123456")

    def test_production_signing_secrets_are_isolated_and_fail_closed(self):
        workflow = (ROOT / ".github/workflows/github-release.yml").read_text()
        signing = workflow.split("  desktop_signing:", 1)[1].split("\n  publish:", 1)[0]
        self.assertIn("environment: desktop-release-signing", signing)
        self.assertIn("PLURX_TRUSTED_PACKAGING_COMMIT", signing)
        self.assertIn("secrets.PLURX_UPDATE_ED25519_PRIVATE_KEY", signing)
        self.assertNotIn("release-source", signing)
        self.assertNotIn("secrets.PLURX_UPDATE_ED25519_PRIVATE_KEY", workflow.split("  desktop_signing:", 1)[0])
        self.assertIn("--inspect-packages", signing)
        self.assertIn("--production", signing)
        self.assertIn("production-trust-roots.json", signing)
        self.assertIn("candidate-trust-roots.json", signing)
        candidate = workflow.split("  desktop_metadata:", 1)[1].split("\n  desktop_signing:", 1)[0]
        self.assertIn("fixtures are never published", candidate)
        self.assertNotIn("    environment:", candidate)
        self.assertNotIn("secrets.", candidate)
        self.assertIn("&& needs.resolve.outputs.mode == 'release'", signing)

    def test_linux_packager_rejects_mislabelled_architecture_before_output(self):
        binary = self.root / "fixture-arm64"
        binary.write_bytes(b"\x7fELF\x02\x01\x01" + b"\0" * 11 + struct.pack("<H", 183) + b"packaging fixture")
        binary.chmod(0o755)
        binary.with_name(binary.name + ".dwp").write_bytes(b"fixture symbols")
        tools = self.root / "tools"
        tools.mkdir()
        (tools / "nfpm").write_text("#!/bin/sh\nexit 99\n")
        (tools / "nfpm").chmod(0o755)
        out = self.root / "should-not-exist"
        result = subprocess.run([str(ROOT / "scripts/package-linux"), "--arch", "amd64", "--version", "1.2.3", "--binary", str(binary), "--control-dir", str(self.root), "--out", str(out)],
            env=os.environ | {"PATH": str(tools) + ":" + os.environ["PATH"]}, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not an ELF64 little-endian amd64 executable", result.stderr)
        self.assertFalse(out.exists())

    def run_scriptlet(self, phase, arguments, helper_exit):
        fixture = self.root / (phase + str(helper_exit))
        fixture.mkdir()
        fakebin = fixture / "bin"
        fakebin.mkdir()
        log = fixture / "commands"
        def stub(name, body):
            target = fakebin / name
            target.write_text('#!/bin/sh\nprintf "%s\\n" "' + name + ' $*" >> "' + str(log) + '"\n' + body)
            target.chmod(0o755)
        stub("getent", "exit 0\n")
        stub("hostname", "echo fixture-host\n")
        stub("systemctl", "exit 0\n")
        stub("install", 'args=; while [ "$#" -gt 0 ]; do case "$1" in -o|-g) shift 2 ;; *) args="$args \"$1\""; shift ;; esac; done; eval /usr/bin/install "$args"\n')
        helper = fixture / "usr/lib/plurx-control/plurx-install-helper"
        helper.parent.mkdir(parents=True)
        helper.write_text('#!/bin/sh\nprintf "%s\\n" "helper $*" >> "' + str(log) + '"\nexit ' + str(helper_exit) + '\n')
        helper.chmod(0o755)
        (fixture / "run/systemd/system").mkdir(parents=True)
        source = (ROOT / "deploy/packaging" / phase).read_text()
        source = re.sub(r"/(?:usr/lib/plurx-control|var/lib/plurx-control|var/lib/plurx|etc/plurx|run/systemd/system)", lambda m: str(fixture / m[0].lstrip("/")), source)
        script = fixture / phase
        script.write_text(source)
        result = subprocess.run(["sh", str(script), *arguments], env=os.environ | {"PATH": str(fakebin) + ":" + os.environ["PATH"], "PLURX_TRANSACTION_ID": "untrusted-environment"}, capture_output=True, text=True)
        return result, log.read_text() if log.exists() else ""

    def test_joined_package_scriptlet_never_starts_or_recursively_recovers(self):
        result, calls = self.run_scriptlet("postinstall", ["configure", "1.2.2-1"], 10)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("package-script postinstall server -- configure 1.2.2-1", calls)
        self.assertNotIn("systemctl", calls)
        self.assertNotIn("plurx-recovery", calls)
        self.assertNotIn("apt", calls)

    def test_ordinary_upgrade_retains_operator_service_policy(self):
        result, calls = self.run_scriptlet("postinstall", ["configure", "1.2.2-1"], 0)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("systemctl try-restart plurxd.service", calls)
        self.assertNotIn("systemctl enable", calls)

    def test_conflicting_preinstall_refuses_even_with_environment_transaction_id(self):
        result, calls = self.run_scriptlet("preinstall", ["upgrade", "1.2.2-1"], 73)
        self.assertEqual(result.returncode, 73, result.stdout + result.stderr)
        self.assertIn("package-script preinstall server", calls)
        self.assertNotIn("systemctl", calls)

    def test_gui_dependency_closure_stays_in_optional_exact_version_package(self):
        server = (ROOT / "deploy/packaging/nfpm.yaml").read_text()
        desktop = (ROOT / "deploy/packaging/desktop-nfpm.yaml").read_text()
        self.assertNotIn("gtk", server.lower())
        self.assertNotIn("plurx-tray", server)
        self.assertIn("plurxd (= ${PLURX_PKG_VERSION}-1)", desktop)
        self.assertIn("plurxd = ${PLURX_PKG_VERSION}-1", desktop)
        self.assertIn("libgtk-4-1 (>= 4.0)", desktop)
        for name in ["plurx-control", "plurx-service-launch", "plurx-recovery", "plurx-install-helper", "plurx-update-worker"]:
            self.assertIn(name, server)
        self.assertIn("mode: 0755", server)
        self.assertNotIn("4755", server)


if __name__ == "__main__":
    unittest.main()
