"""Production macOS signing order and immutable release closure regressions."""
import contextlib
import hashlib
import importlib.machinery
import importlib.util
import json
import os
import re
import io
from pathlib import Path
import plistlib
import struct
import subprocess
import shutil
import sys
import tempfile
import tarfile
import textwrap
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[2]


def load_script(name, path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


class MacosReleaseSigningCase(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="macos-release-signing-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.tool = load_script("macos_release_signing_fixture", ROOT / "scripts/sign-macos-release")
        self.package_tool = self.tool.package_tool()
        self.package = self.root / "plurxd-1.2.3-macos-universal"
        self.package.mkdir()
        self.app = self.package / "Plurx Server.app"
        self.images = ["runtime/plurxd", "runtime/ffmpeg", "runtime/ffprobe",
            "runtime/subtitle-engine/whisper-cli", "runtime/hidden-native-code.data",
            "Plurx Server.app/Contents/MacOS/PlurxServer",
            "Plurx Server.app/Contents/MacOS/plurx-control"]
        self.images += ["control/" + name for name in ["plurx-control", "plurx-install-helper",
            "plurx-update-worker", "plurx-service-launch", "plurx-recovery"]]
        for relative in self.images:
            target = self.package / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            # MH_DYLIB still needs signing even when its executable bit is absent.
            file_type = 6 if relative.endswith(".data") else 2
            target.write_bytes(struct.pack("<8I", 0xFEEDFACF, 0x0100000C, 0, file_type, 0, 0, 0, 0))
            target.chmod(0o644 if file_type == 6 else 0o755)
        dwarf = self.package / "debug/plurxd.dSYM/Contents/Resources/DWARF/plurxd"
        dwarf.parent.mkdir(parents=True)
        dwarf.write_bytes(struct.pack("<8I", 0xFEEDFACF, 0x0100000C, 0, 10, 0, 0, 0, 0))
        (self.app / "Contents/Info.plist").write_bytes(plistlib.dumps({
            "CFBundleIdentifier": "tv.plurx.Server", "CFBundleShortVersionString": "1.2.3",
            "CFBundleExecutable": "PlurxServer"}))
        self.write_json("release.json", {"schema_version": 1, "repository": "pjunod/cinema",
            "version": "1.2.3", "source_commit": "a" * 40, "architecture": "universal"})
        self.write_json("runtime/provenance/manifest.json", {"binaries": {
            name: {"sha256": self.digest(self.package / "runtime" / name), "bytes": 32}
            for name in ["ffmpeg", "ffprobe"]}})
        self.write_json("runtime/subtitle-engine/subtitle-engine.json", {
            "platform": "macos-universal", "binary_sha256": self.digest(self.package / "runtime/subtitle-engine/whisper-cli"),
            "binary_bytes": 32, "cpu_variants": {}})
        self.write_json("provenance/manifest.json", {"artifacts": {
            name: self.digest(self.package / "runtime" / name) for name in ["plurxd", "ffmpeg", "ffprobe"]},
            "subtitle_engine": json.loads((self.package / "runtime/subtitle-engine/subtitle-engine.json").read_text())})
        self.write_json("provenance/desktop-release.json", {"version": "1.2.3"})
        self.seal_closure()
        self.key = self.root / "AuthKey_FIXTURE123.p8"
        self.key.write_text("fixture key, never submitted")
        self.calls = []
        self.notary_status = "Accepted"

    @staticmethod
    def digest(path):
        return hashlib.sha256(path.read_bytes()).hexdigest()

    def write_json(self, relative, value):
        target = self.package / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(json.dumps(value))

    def seal_closure(self):
        files = []
        for target in sorted(self.package.rglob("*")):
            if target.is_file() and target.name != "runtime-closure-v1.json":
                files.append({"path": target.relative_to(self.package).as_posix(),
                    "size": target.stat().st_size, "sha256": self.digest(target),
                    "executable": bool(target.stat().st_mode & 0o111)})
        self.write_json("runtime-closure-v1.json", {"schema_version": 1, "version": "1.2.3", "files": files})

    def run_apple_tool(self, command, **kwargs):
        command = [str(part) for part in command]
        self.calls.append(command)
        stdout = b""
        if "codesign" in Path(command[0]).name and "--sign" in command:
            target = Path(command[-1])
            if target.is_file():
                target.write_bytes(target.read_bytes() + b"signed-fixture")
        if "notarytool" in command and "submit" in command:
            stdout = json.dumps({"id": "fixture-notary-id", "status": self.notary_status}).encode()
        if "stapler" in command and "staple" in command:
            (self.app / "Contents/CodeResources.ticket").write_bytes(b"fixture-stapled-ticket")
        if kwargs.get("text") or kwargs.get("encoding"):
            stdout = stdout.decode()
        return subprocess.CompletedProcess(command, 0, stdout=stdout,
            stderr="" if isinstance(stdout, str) else b"")

    @contextlib.contextmanager
    def apple_tools(self):
        engine = Mock()
        engine.verify.return_value = json.loads((self.package / "runtime/subtitle-engine/subtitle-engine.json").read_text())
        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.object(self.tool, "package_tool", return_value=self.package_tool))
            stack.enter_context(patch.object(self.package_tool, "validate_native_package", return_value={}))
            stack.enter_context(patch.object(self.package_tool, "engine_recipe", return_value=engine))
            stack.enter_context(patch.object(self.package_tool, "verify_distribution", side_effect=self.verify_final_distribution))
            stack.enter_context(patch.object(self.tool.subprocess, "run", side_effect=self.run_apple_tool))
            yield

    def verify_final_distribution(self, package, team_id):
        self.assertEqual(team_id, "TEAM123456")
        self.assertTrue((self.app / "Contents/CodeResources.ticket").is_file())
        self.package_tool.verify_package_closure(package)
        self.calls.append(["verify-final-distribution"])
        return {"team_id": team_id}

    def sign(self, **changes):
        arguments = dict(package=self.package, identity="Developer ID Application: Fixture (TEAM123456)",
            team_id="TEAM123456", key=self.key, key_id="FIXTURE123", issuer="11111111-2222-3333-4444-555555555555")
        arguments.update(changes)
        return self.tool.sign_package(**arguments)

    def test_signs_complete_native_closure_before_notary_and_staples_before_resealing(self):
        original = {name: self.digest(self.package / name) for name in self.images}
        with self.apple_tools():
            receipt = self.sign()
        signed = [call for call in self.calls if "--sign" in call]
        targets = [Path(call[-1]) for call in signed]
        for relative in self.images:
            if relative != "Plurx Server.app/Contents/MacOS/plurx-control":
                self.assertIn(self.package / relative, targets, relative)
            self.assertNotEqual(self.digest(self.package / relative), original[relative])
        self.assertEqual(targets[-1], self.app, "outer app must be signed after embedded code")
        self.assertFalse(any(".dSYM" in str(target) for target in targets))
        for call in signed:
            self.assertIn("--timestamp", call)
            options = call[call.index("--options") + 1]
            self.assertIn("runtime", options)
        daemon = next(call for call in signed if call[-1] == str(self.package / "runtime/plurxd"))
        self.assertIn("--entitlements", daemon)
        self.assertEqual(Path(daemon[daemon.index("--entitlements") + 1]).name,
                         "macos-source-parser.entitlements")
        notary = next(i for i, call in enumerate(self.calls) if "notarytool" in call and "submit" in call)
        staple = next(i for i, call in enumerate(self.calls) if "stapler" in call and "staple" in call)
        self.assertTrue(all(i < notary for i, call in enumerate(self.calls) if "--sign" in call))
        self.assertLess(notary, staple)
        self.assertEqual(self.calls[-1], ["verify-final-distribution"])
        self.package_tool.verify_package_closure(self.package)
        closure = json.loads((self.package / "runtime-closure-v1.json").read_text())
        self.assertIn("Plurx Server.app/Contents/CodeResources.ticket", {entry["path"] for entry in closure["files"]})
        self.assertIn("--wait", self.calls[notary])
        self.assertIn("--output-format", self.calls[notary])
        self.assertEqual((self.package / "runtime/hidden-native-code.data").stat().st_mode & 0o777, 0o644)
        for relative in self.images:
            self.assertEqual(receipt["signed_images"][relative]["input_sha256"], original[relative])
            self.assertEqual(receipt["signed_images"][relative]["signed_sha256"], self.digest(self.package / relative))
        native = json.loads((self.package / "runtime/provenance/manifest.json").read_text())
        for name in ["ffmpeg", "ffprobe"]:
            self.assertEqual(native["binaries"][name]["sha256"], self.digest(self.package / "runtime" / name))
            self.assertEqual(receipt["input_manifests"]["runtime/provenance/manifest.json"]["binaries"][name]["sha256"], original["runtime/" + name])
        engine = json.loads((self.package / "runtime/subtitle-engine/subtitle-engine.json").read_text())
        self.assertEqual(engine["binary_sha256"], self.digest(self.package / "runtime/subtitle-engine/whisper-cli"))
        outer = json.loads((self.package / "provenance/manifest.json").read_text())
        self.assertEqual(outer["subtitle_engine"], engine)
        for name, value in outer["artifacts"].items():
            self.assertEqual(value, self.digest(self.package / "runtime" / name))

    def test_rejected_notary_submission_never_staples_or_verifies_distribution(self):
        self.notary_status = "Invalid"
        with self.apple_tools(), self.assertRaises((ValueError, RuntimeError)):
            self.sign()
        self.assertFalse(any("stapler" in call for call in self.calls))
        self.assertNotIn(["verify-final-distribution"], self.calls)
        self.assertFalse((self.app / "Contents/CodeResources.ticket").exists())

    def test_codesign_failure_stops_before_notary_and_restores_file_mode(self):
        failed_image = self.package / "runtime/hidden-native-code.data"
        mode = failed_image.stat().st_mode & 0o777
        observed = self.run_apple_tool
        def fail_signing(command, **kwargs):
            if "--sign" in command and str(command[-1]) == str(failed_image):
                raise subprocess.CalledProcessError(1, command)
            return observed(command, **kwargs)
        with self.apple_tools(), patch.object(self.tool.subprocess, "run", side_effect=fail_signing):
            with self.assertRaises(subprocess.CalledProcessError):
                self.sign()
        self.assertEqual(failed_image.stat().st_mode & 0o777, mode)
        self.assertFalse(any("notarytool" in call or "stapler" in call for call in self.calls))

    def test_different_embedded_frontend_refuses_before_apple_tools(self):
        (self.app / "Contents/MacOS/plurx-control").write_bytes(b"mismatched embedded controller")
        self.seal_closure()
        with self.apple_tools(), self.assertRaises(ValueError):
            self.sign()
        self.assertEqual(self.calls, [])

    def test_metadata_paths_cannot_reference_credentials_outside_candidate(self):
        manifests = ["runtime/provenance/manifest.json", "provenance/manifest.json"]
        for relative in manifests:
            path = self.package / relative
            original = path.read_bytes()
            for reference in [str(self.key), "../../" + self.key.name]:
                with self.subTest(manifest=relative, reference=reference):
                    value = json.loads(original)
                    if relative.startswith("runtime/"):
                        value["binaries"][reference] = {"sha256": self.digest(self.key)}
                    else:
                        value["artifacts"][reference] = self.digest(self.key)
                    path.write_text(json.dumps(value))
                    self.seal_closure()
                    with self.apple_tools(), self.assertRaises(ValueError):
                        self.sign()
                    self.assertEqual(self.calls, [], "metadata paths must be preflighted before any signing")
                    path.write_bytes(original)
                    self.seal_closure()

    def test_preexisting_tamper_is_rejected_before_apple_tools(self):
        (self.package / "runtime/plurxd").write_bytes(b"tampered fixture")
        with self.apple_tools(), self.assertRaises(ValueError):
            self.sign()
        self.assertEqual(self.calls, [])

    def test_invalid_signing_configuration_is_rejected_before_apple_tools(self):
        cases = [{"team_id": ""}, {"team_id": "not-a-team"}, {"identity": "-"},
            {"key": self.root / "missing.p8"}, {"key_id": ""}, {"issuer": ""}]
        for changes in cases:
            with self.subTest(changes=changes), self.apple_tools(), self.assertRaises(ValueError):
                self.sign(**changes)
            self.assertEqual(self.calls, [])


    def test_candidate_extraction_preserves_exact_identity_and_refuses_mismatch(self):
        archive = self.root / "candidate.tar.gz"
        with tarfile.open(archive, "w:gz") as output:
            output.add(self.package, arcname=self.package.name)
        destination = self.root / "extracted"
        destination.mkdir()
        candidate = self.tool.extract_candidate(archive, destination, "1.2.3", "a" * 40)
        self.assertEqual(candidate.name, self.package.name)
        self.package_tool.verify_package_closure(candidate)
        rejected = self.root / "mismatched"
        rejected.mkdir()
        with self.assertRaisesRegex(ValueError, "identity"):
            self.tool.extract_candidate(archive, rejected, "1.2.3", "b" * 40)

    def test_candidate_extraction_refuses_traversal_links_and_duplicate_members(self):
        wrapper = self.package.name
        cases = [([wrapper + "/../outside"], None), (["/absolute"], None),
            ([wrapper + "/runtime/code", wrapper + "/runtime/code"], None),
            ([wrapper + "/runtime/link"], tarfile.SYMTYPE),
            ([wrapper + "/runtime/link"], tarfile.LNKTYPE),
            ([wrapper + "/runtime/device"], tarfile.CHRTYPE)]
        for index, (names, kind) in enumerate(cases):
            with self.subTest(names=names, kind=kind):
                archive = self.root / f"unsafe-{index}.tar.gz"
                with tarfile.open(archive, "w:gz") as output:
                    for name in names:
                        member = tarfile.TarInfo(name)
                        if kind is None:
                            member.size = 1
                            output.addfile(member, io.BytesIO(b"x"))
                        else:
                            member.type = kind
                            member.linkname = "../../outside"
                            output.addfile(member)
                destination = self.root / f"unsafe-output-{index}"
                destination.mkdir()
                with self.assertRaisesRegex(ValueError, "unsafe"):
                    self.tool.extract_candidate(archive, destination, "1.2.3", "a" * 40)
                self.assertEqual(list(destination.iterdir()), [], "all members must be checked before extraction")
        self.assertFalse((self.root / "outside").exists())

    def test_workflow_isolates_apple_credentials_and_routes_only_signed_release_artifact(self):
        workflow = (ROOT / ".github/workflows/github-release.yml").read_text()
        blocks = dict(re.findall(r"(?ms)^  ([a-z_]+):\n(.*?)(?=^  [a-z_]+:|\Z)", workflow.split("\njobs:\n", 1)[1]))
        macos = blocks["macos"]
        signing = blocks["macos_signing"]
        self.assertIn("name: ${{ needs.resolve.outputs.mode == 'release' && 'macos-candidate' || 'release-macos' }}", macos)
        self.assertNotIn("--verify-distribution", macos, "ad-hoc build candidates must reach the signing job")
        self.assertIn("if: github.server_url == 'https://github.com' && needs.resolve.outputs.mode == 'release'", signing)
        self.assertIn("environment: macos-release-signing", signing)
        self.assertIn('test "$PACKAGING_COMMIT" = "$TRUSTED_PACKAGING_COMMIT"', signing)
        self.assertIn('git -C trusted fetch -q --depth 1 "$REPO_URL" "$TRUSTED_PACKAGING_COMMIT"', signing)
        self.assertIn("name: macos-candidate", signing)
        self.assertNotIn("release-source", signing)
        self.assertLess(signing.index("python3 trusted/scripts/sign-macos-release"), signing.rindex("name: release-macos"))
        apple_secrets = {"PLURX_MACOS_CERTIFICATE_P12_BASE64", "PLURX_MACOS_CERTIFICATE_PASSWORD", "PLURX_MACOS_NOTARY_PRIVATE_KEY"}
        for name, block in blocks.items():
            found = set(re.findall(r"secrets\.(PLURX_MACOS_[A-Z0-9_]+)", block))
            self.assertEqual(found, apple_secrets if name == "macos_signing" else set(), name)
        self.assertIn("if: always()", signing)
        self.assertIn('trap cleanup EXIT', signing)
        self.assertIn("macos_signing", blocks["publish"].split("    steps:", 1)[0])
        metadata_gate = next(line for line in blocks["desktop_metadata"].splitlines() if line.strip().startswith("if:"))
        self.assertIn("!cancelled()", metadata_gate)
        self.assertIn("!failure()", metadata_gate)
        self.assertIn("needs.resolve.outputs.mode != 'qualification'", metadata_gate)

    def test_missing_workflow_signing_configuration_refuses_before_creating_credentials(self):
        workflow = (ROOT / ".github/workflows/github-release.yml").read_text()
        step = workflow.split("      - name: Sign every native image, notarize, staple and verify the complete archive\n", 1)[1]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1].split("      - name:", 1)[0])
        required = ["CERTIFICATE_BASE64", "CERTIFICATE_PASSWORD", "NOTARY_PRIVATE_KEY", "SIGNING_IDENTITY",
            "TEAM_ID", "NOTARY_KEY_ID", "NOTARY_ISSUER_ID"]
        for missing in required:
            with self.subTest(missing=missing):
                environment = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "RUNNER_TEMP": str(self.root),
                    **dict.fromkeys(required, "fixture")}
                environment[missing] = ""
                result = subprocess.run(["bash", "-c", script], cwd=self.root, env=environment,
                    capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(missing, result.stdout + result.stderr)
                self.assertFalse((self.root / "macos-release-credentials").exists())
                self.assertFalse((self.root / "dist").exists())

    def test_final_receipt_and_embedded_frontend_match_bytes_after_outer_app_resigning(self):
        observed = self.run_apple_tool
        def apple_signatures(command, **kwargs):
            result = observed(command, **kwargs)
            if "--sign" in command:
                target = Path(command[-1])
                if target.is_file():
                    target.write_bytes(target.read_bytes() + str(len(self.calls)).encode())
                elif target == self.app:
                    executable = self.app / "Contents/MacOS/PlurxServer"
                    executable.write_bytes(executable.read_bytes() + b"outer-app-signature")
            return result
        with self.apple_tools(), patch.object(self.tool.subprocess, "run", side_effect=apple_signatures):
            receipt = self.sign()
        for relative, facts in receipt["signed_images"].items():
            self.assertEqual(facts["signed_sha256"], self.digest(self.package / relative), relative)
        self.assertEqual((self.package / "control/plurx-control").read_bytes(),
            (self.app / "Contents/MacOS/plurx-control").read_bytes(),
            "native qualification requires the GUI frontend to be the identical signed controller")

    @staticmethod
    def workflow_step_script(title):
        workflow = (ROOT / ".github/workflows/github-release.yml").read_text()
        step = workflow.split("      - name: " + title + "\n", 1)[1]
        body = step.split("        run: |\n", 1)[1]
        return textwrap.dedent(re.split(r"(?m)^ {0,9}\S", body, maxsplit=1)[0])

    def test_update_publication_selector_requires_every_qualified_field(self):
        script = self.workflow_step_script("Select qualified in-app update publication")
        fields = ["UPGRADE_FROM", "DATA_COMPATIBILITY", "CLUSTER_COMPATIBILITY", "MINIMUM_CONTROLLER", "ROLLBACK"]
        cases = [dict.fromkeys(fields, ""), {**dict.fromkeys(fields, ""), "UPGRADE_FROM": "=1.0.0"},
            dict(zip(fields, ["=1.0.0", "fixture-data", "fixture-cluster", "1.0.0", "manual_recovery"]))]
        cases += [{**dict.fromkeys(fields, "fixture"), missing: ""} for missing in fields]
        for index, values in enumerate(cases):
            with self.subTest(values=values):
                output = self.root / f"qualification-output-{index}"
                summary = self.root / f"qualification-summary-{index}"
                result = subprocess.run(["bash", "-c", script], cwd=self.root,
                    env={"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "GITHUB_OUTPUT": str(output),
                         "GITHUB_STEP_SUMMARY": str(summary), **values}, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                enabled = all(values.values())
                self.assertEqual(output.read_text(), "enabled=" + str(enabled).lower() + "\n")
                if not enabled:
                    self.assertIn("Verified installers will publish", summary.read_text())
                    for field, value in values.items():
                        if not value:
                            self.assertIn(field, summary.read_text())
        workflow = (ROOT / ".github/workflows/github-release.yml").read_text()
        signer = workflow.split("\n  desktop_signing:\n", 1)[1].split("\n  publish:\n", 1)[0]
        gate = next(line for line in signer.splitlines() if line.strip().startswith("if:"))
        self.assertIn("needs.resolve.outputs.publish_updates == 'true'", gate)
        publisher = workflow.split("\n  publish:\n", 1)[1]
        gate = next(line for line in publisher.splitlines() if line.strip().startswith("if:"))
        self.assertIn("!cancelled()", gate)
        self.assertIn("!failure()", gate)

    def test_installer_publication_requires_exact_assets_and_never_advertises_fixture_feed(self):
        script = self.workflow_step_script("Check the asset set and compute its checksums")
        version = "1.2.3"
        native = [f"plurxd_{version}-1_amd64.deb", f"plurxd_{version}-1_arm64.deb",
            f"plurxd-{version}-1.x86_64.rpm", f"plurxd-{version}-1.aarch64.rpm",
            f"plurx-desktop_{version}-1_amd64.deb", f"plurx-desktop_{version}-1_arm64.deb",
            f"plurx-desktop-{version}-1.x86_64.rpm", f"plurx-desktop-{version}-1.aarch64.rpm",
            f"plurx-tray-{version}-linux-amd64.tar.gz", f"plurx-tray-{version}-linux-arm64.tar.gz",
            f"plurxd-{version}-macos-universal.tar.gz", f"plurxd-{version}-windows-x86_64.zip"]
        metadata = ["plurx-update-v1.json", "plurx-update-v1.json.sig"]
        # Exercise the actual shell asset validator; macOS hosts may lack GNU sha256sum.
        tools = self.root / "checksum-tools"
        tools.mkdir()
        if shutil.which("sha256sum") is None:
            checksum = tools / "sha256sum"
            checksum.write_text("#!" + sys.executable + "\nimport hashlib, pathlib, sys\n"
                "for name in sys.argv[1:]: print(hashlib.sha256(pathlib.Path(name).read_bytes()).hexdigest() + '  ' + name)\n")
            checksum.chmod(0o755)
        cases = [(False, native, True), (False, native + metadata[:1], False),
            (False, native + metadata, False), (False, native[:-2] + native[-1:], False),
            (True, native, False), (True, native + metadata[:1], False), (True, native + metadata, True)]
        for index, (enabled, files, success) in enumerate(cases):
            with self.subTest(enabled=enabled, files=files):
                run = self.root / f"publisher-{index}"
                (run / "assets").mkdir(parents=True)
                for name in files:
                    (run / "assets" / name).write_bytes(b"fixture asset; never a production feed")
                result = subprocess.run(["bash", "-c", script], cwd=run,
                    env={"PATH": str(tools) + os.pathsep + os.environ.get("PATH", "/usr/bin:/bin"),
                         "VERSION": version, "PUBLISH_UPDATES": str(enabled).lower()}, capture_output=True, text=True)
                self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
                if success:
                    checksums = (run / "SHA256SUMS").read_text().splitlines()
                    self.assertEqual(len(checksums), len(files))
                    self.assertEqual({line.split()[-1] for line in checksums}, set(files))
                else:
                    self.assertFalse((run / "SHA256SUMS").exists())

if __name__ == "__main__":
    unittest.main()
