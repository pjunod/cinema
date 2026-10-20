"""Source installs assemble the macOS companion before touching server state."""
import hashlib
import importlib.machinery
import importlib.util
import io
import json
import os
import plistlib
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/build-macos-source-runtime"
loader = importlib.machinery.SourceFileLoader("macos_source_runtime", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
runtime = importlib.util.module_from_spec(spec)
loader.exec_module(runtime)
COMMIT = "0123456789abcdef0123456789abcdef01234567"


class MacosSourceRuntimeTests(unittest.TestCase):
    def test_source_closure_attests_native_identity_and_complete_files(self):
        with tempfile.TemporaryDirectory() as work:
            package = Path(work)
            for name in ["runtime/plurxd", "runtime/ffmpeg", "runtime/ffprobe", "runtime/plurx-source-parser.wasm",
                         "runtime/subtitle-engine/subtitle-engine.json", "Plurx Server.app/Contents/MacOS/plurx-control"]:
                path = package / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(name.encode())
            runtime.seal(package, "0.7.0", COMMIT, "arm64")
            self.assertEqual(json.loads((package / "source-install-v1.json").read_text()),
                             {"schema_version": 1, "source_commit": COMMIT, "architecture": "arm64"})
            release = json.loads((package / "release.json").read_text())
            self.assertEqual(release["architecture"], "arm64")
            closure = json.loads((package / "runtime-closure-v1.json").read_text())
            expected = {str(path.relative_to(package)) for path in package.rglob("*") if path.is_file()} - {"runtime-closure-v1.json"}
            self.assertEqual({entry["path"] for entry in closure["files"]}, expected)
            for entry in closure["files"]:
                self.assertEqual(entry["sha256"], hashlib.sha256((package / entry["path"]).read_bytes()).hexdigest())

    def test_source_closure_refuses_symlinks(self):
        with tempfile.TemporaryDirectory() as work:
            package = Path(work)
            (package / "outside").symlink_to("/tmp")
            with self.assertRaisesRegex(ValueError, "link"):
                runtime.seal(package, "0.7.0", COMMIT, "amd64")

    def test_prebuilt_source_must_match_exact_commit(self):
        with tempfile.TemporaryDirectory() as work:
            binary = Path(work) / "plurxd"
            binary.write_text("fixture")
            binary.chmod(0o755)
            for wrong in ["unknown", COMMIT[:7], "f" * 40]:
                with self.subTest(identity=wrong), mock.patch.object(runtime, "output", return_value=wrong):
                    with self.assertRaisesRegex(ValueError, "differs"):
                        runtime.verify_identity(binary, COMMIT)
            with mock.patch.object(runtime, "output", return_value=COMMIT) as invocation:
                runtime.verify_identity(binary, COMMIT)
                self.assertEqual(invocation.call_args.args[0], [binary, "--source-commit"])

    def test_prebuilt_daemon_symlink_is_rejected_before_execution(self):
        with tempfile.TemporaryDirectory() as work:
            binary = Path(work) / "plurxd"
            binary.symlink_to(sys.executable)
            with mock.patch.object(runtime, "output") as invocation:
                with self.assertRaisesRegex(ValueError, "regular"):
                    runtime.verify_identity(binary, COMMIT)
                invocation.assert_not_called()

    def test_pinned_media_extraction_refuses_traversal_and_links(self):
        with tempfile.TemporaryDirectory() as work:
            for name, kind in [("../escape", tarfile.REGTYPE), ("ffmpeg", tarfile.SYMTYPE)]:
                archive = Path(work) / "media.tar"
                with tarfile.open(archive, "w") as destination:
                    member = tarfile.TarInfo(name)
                    member.type = kind
                    destination.addfile(member, io.BytesIO())
                with self.assertRaisesRegex(ValueError, "unsafe"):
                    runtime.unpack_media(archive, Path(work) / "extract")
                self.assertFalse((Path(work).parent / "escape").exists())

    def test_dry_run_never_builds_or_creates_output(self):
        with tempfile.TemporaryDirectory() as work:
            destination = Path(work) / "runtime"
            result = subprocess.run([sys.executable, SCRIPT, "--output", destination, "--dry-run"], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("native Rust controllers and Swift tray", result.stdout)
            self.assertFalse(destination.exists())


class MacosSourceInstallerRoutingTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        (self.root / "deploy").mkdir()
        (self.root / "scripts").mkdir()
        (self.root / "bin").mkdir()
        (self.root / "home").mkdir()
        # Replace only the external GUI-launch boundary with a fixture tool.
        installer = (ROOT / "deploy/install").read_text().replace("/usr/bin/open", str(self.root / "bin/open"))
        (self.root / "deploy/install").write_text(installer)
        self.log = self.root / "calls"
        self.env = {**os.environ, "HOME": str(self.root / "home"),
                    "PATH": str(self.root / "bin") + os.pathsep + os.environ["PATH"], "SOURCE_TEST_LOG": str(self.log)}
        self.executable("id", '#!/bin/sh\ncase "$1" in -u) echo 501;; -g) echo 20;; esac\n')
        self.executable("launchctl", '#!/bin/sh\necho "launchctl $*" >> "$SOURCE_TEST_LOG"\nexit 113\n')
        self.executable("curl", '#!/bin/sh\necho ready\n')
        builder = self.root / "scripts/build-macos-source-runtime"
        builder.write_text('''import os, pathlib, sys
with open(os.environ["SOURCE_TEST_LOG"], "a") as stream: stream.write("build-complete\\n")
package = pathlib.Path(sys.argv[sys.argv.index("--output") + 1])
(package / "control").mkdir(parents=True)
control = package / "control/plurx-control"
control.write_text('#!/bin/sh\\ncase "$*" in *--preflight*) echo "preflight $*" >> "$SOURCE_TEST_LOG"; [ ! -e "$HOME/Applications/Plurx Server.app" ]; exit $?;; esac\\necho "bootstrap $*" >> "$SOURCE_TEST_LOG"\\nif [ "${SOURCE_TEST_BOOTSTRAP_FAIL:-0}" = 1 ]; then [ "${SOURCE_TEST_PARTIAL:-0}" != 1 ] || mkdir -p "$HOME/Library/Application Support/plurx/runtime"; exit 1; fi\\nmkdir -p "$HOME/Library/Application Support/plurx"\\necho \\'{"browser_origin":"http://127.0.0.1:32400"}\\' > "$HOME/Library/Application Support/plurx/installation.json"\\n')
control.chmod(0o755)
''')

    def executable(self, name, text):
        path = self.root / "bin" / name
        path.write_text(text)
        path.chmod(0o755)

    def run_mode(self, mode, *arguments):
        return subprocess.run(["bash", self.root / "deploy/install", mode, *arguments], env=self.env, capture_output=True, text=True)

    def install(self, *arguments):
        return self.run_mode("macos", *arguments)

    def calls(self):
        return self.log.read_text() if self.log.exists() else ""

    def test_default_stages_companion_before_native_bootstrap(self):
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        self.assertLess(calls.index("build-complete"), calls.index("bootstrap"))
        self.assertIn("bootstrap bootstrap macos-source --package", calls)
        self.assertNotIn("kickstart", calls)
        self.assertNotIn("bootout", calls)
        self.assertIn("open it from the GUI login", result.stdout)

    def test_gui_source_install_opens_stable_app_after_successful_bootstrap(self):
        self.executable("launchctl", '#!/bin/sh\necho "launchctl $*" >> "$SOURCE_TEST_LOG"\n[ "$2" = gui/501 ] && exit 0\nexit 113\n')
        self.executable("open", '#!/bin/sh\necho "open $*" >> "$SOURCE_TEST_LOG"\n')
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        self.assertLess(calls.index("bootstrap bootstrap"), calls.index("open "))
        self.assertIn("open " + str(self.root / "home/Applications/Plurx Server.app"), calls)

    def test_build_failure_keeps_running_service_untouched(self):
        (self.root / "scripts/build-macos-source-runtime").write_text("raise SystemExit(1)\n")
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls(), "")

    def legacy_running_fixture(self):
        agents = self.root / "home/Library/LaunchAgents"
        agents.mkdir(parents=True)
        binary = self.root / "legacy-bin/plurxd"
        binary.parent.mkdir()
        binary.write_text("#!/bin/sh\nexit 0\n")
        binary.chmod(0o755)
        with (agents / "com.plurx.plurxd.plist").open("wb") as stream:
            plistlib.dump({"Label": "com.plurx.plurxd", "ProgramArguments": [str(binary), "run"]}, stream)
        self.executable("launchctl", '''#!/bin/sh
echo "launchctl $*" >> "$SOURCE_TEST_LOG"
case "$1 $2" in
  "print-disabled gui/501") echo '{}'; exit 0 ;;
  "kill SIGTERM") touch "$HOME/stopped"; exit 0 ;;
  "kickstart gui/501/com.plurx.plurxd") rm -f "$HOME/stopped"; exit 0 ;;
  "print gui/501/com.plurx.plurxd")
    echo "program = ''' + str(binary) + '''"
    [ -f "$HOME/stopped" ] || { echo 'pid = 4242'; exit 0; }
    echo 'state = not running'; exit 0 ;;
esac
exit 113
''')
        return binary

    def test_running_legacy_exits_before_bootstrap_and_starts_only_after_success(self):
        self.legacy_running_fixture()
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        self.assertLess(calls.index("preflight"), calls.index("kill SIGTERM"))
        self.assertLess(calls.index("build-complete"), calls.index("kill SIGTERM"))
        self.assertLess(calls.index("kill SIGTERM"), calls.index("bootstrap bootstrap"))
        self.assertLess(calls.index("bootstrap bootstrap"), calls.index("bootstrap start"))
        self.assertNotIn("bootout", calls)
        self.assertNotIn("SIGKILL", calls)
        intent = next((self.root / "target").glob("*-migration/intent.json"))
        self.assertEqual(json.loads(intent.read_text())["phase"], "complete")

    def test_existing_stable_app_refuses_before_legacy_signal_or_bootout(self):
        self.legacy_running_fixture()
        app = self.root / "home/Applications/Plurx Server.app"
        app.mkdir(parents=True)
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("kill SIGTERM", self.calls())
        self.assertNotIn("bootout", self.calls())
        self.assertFalse((self.root / "home/stopped").exists())

    def test_pre_promotion_failure_recovers_unchanged_legacy_running_policy(self):
        self.legacy_running_fixture()
        self.env["SOURCE_TEST_BOOTSTRAP_FAIL"] = "1"
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        calls = self.calls()
        self.assertLess(calls.index("kill SIGTERM"), calls.index("kickstart"))
        self.assertFalse((self.root / "home/stopped").exists())
        self.assertIn("verified unchanged legacy installation recovered", result.stdout)
        intent = next((self.root / "target").glob("*-migration/intent.json"))
        self.assertEqual(json.loads(intent.read_text())["phase"], "recovered")

    def test_partial_native_promotion_holds_instead_of_restarting_legacy(self):
        self.legacy_running_fixture()
        self.env.update({"SOURCE_TEST_BOOTSTRAP_FAIL": "1", "SOURCE_TEST_PARTIAL": "1"})
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("kickstart", self.calls())
        self.assertIn("Recovery required", result.stdout)
        self.assertTrue((self.root / "home/stopped").exists())
        intent = next((self.root / "target").glob("*-migration/intent.json"))
        self.assertEqual(json.loads(intent.read_text())["phase"], "recovery_required")

    def test_default_custom_prefix_refuses_before_build_or_service_mutation(self):
        result = self.install("--prefix", str(self.root / "prefix"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--server-only for custom service prefixes", result.stderr)
        self.assertEqual(self.calls(), "")

    def test_source_installer_dry_run_does_not_build_or_bootstrap(self):
        result = self.install("--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("build-macos-source-runtime", result.stdout)
        self.assertIn("bootstrap macos-source", result.stdout)
        self.assertEqual(self.calls(), "")
        self.assertFalse((self.root / "target").exists())

    def mock_gui(self):
        self.executable("uname", '#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n')
        self.executable("launchctl", '#!/bin/sh\necho "launchctl $*" >> "$SOURCE_TEST_LOG"\n[ "$2" = gui/501 ] && exit 0\nexit 113\n')
        self.executable("open", '#!/bin/sh\necho "open $*" >> "$SOURCE_TEST_LOG"\n')

    def test_gui_binary_install_adds_companion_without_starting_fresh_server(self):
        self.mock_gui()
        result = self.run_mode("binary")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        self.assertIn("build-complete", calls)
        self.assertRegex(calls, r"bootstrap bootstrap macos-source --package .* --no-start")
        self.assertNotIn("bootstrap start", calls)
        self.assertIn("open " + str(self.root / "home/Applications/Plurx Server.app"), calls)

    def test_gui_binary_install_preserves_existing_managed_running_policy(self):
        self.mock_gui()
        record = self.root / "home/Library/Application Support/plurx/installation.json"
        record.parent.mkdir(parents=True)
        record.write_text('{"browser_origin":"http://127.0.0.1:32400"}')
        self.executable("launchctl", '#!/bin/sh\necho "launchctl $*" >> "$SOURCE_TEST_LOG"\ncase "$2" in gui/501) exit 0;; gui/501/com.plurx.plurxd) echo "pid = 4242"; exit 0;; esac\nexit 113\n')
        result = self.run_mode("binary")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--no-start", self.calls())
        self.assertNotIn("kill SIGTERM", self.calls())
        self.assertNotIn("startup set", self.calls())

    def test_native_macos_uninstall_refuses_before_touching_legacy_prefix_binary(self):
        record = self.root / "home/Library/Application Support/plurx/installation.json"
        record.parent.mkdir(parents=True)
        record.write_text("{}")
        prefix = self.root / "other-prefix"
        daemon = prefix / "bin/plurxd"
        daemon.parent.mkdir(parents=True)
        daemon.write_text("unrelated-prefix-daemon")
        result = self.install("--uninstall", "--prefix", str(prefix))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no files changed", result.stderr)
        self.assertEqual(daemon.read_text(), "unrelated-prefix-daemon")
        self.assertEqual(self.calls(), "")

    def test_native_linux_uninstall_refuses_before_service_or_prefix_mutation(self):
        destination = self.root / "linux-root"
        record = destination / "etc/plurx/installation.json"
        record.parent.mkdir(parents=True)
        record.write_text("{}")
        daemon = self.root / "unrelated-prefix/bin/plurxd"
        daemon.parent.mkdir(parents=True)
        daemon.write_text("unrelated-prefix-daemon")
        self.env["DESTDIR"] = str(destination)
        result = self.run_mode("linux", "--uninstall", "--prefix", str(daemon.parent.parent))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("apt or dnf", result.stderr)
        self.assertIn("no files or services changed", result.stderr)
        self.assertEqual(daemon.read_text(), "unrelated-prefix-daemon")
        self.assertTrue(record.exists())
        self.assertEqual(self.calls(), "")

    def test_verified_linux_gui_install_routes_to_complete_native_helper(self):
        helper = self.root / "scripts/install-linux-source-runtime"
        helper.write_text('''import os, sys
with open(os.environ["SOURCE_TEST_LOG"], "a") as stream:
    stream.write("linux-helper " + " ".join(sys.argv[1:]) + "\\n")
''')
        result = self.run_mode("linux", "--binary", "/fixture/plurxd")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        self.assertIn("linux-helper --eligible-session", calls)
        self.assertIn("linux-helper --binary /fixture/plurxd", calls)
        self.assertNotIn("launchctl", calls)

    def test_verified_linux_gui_binary_install_requests_fresh_server_stopped(self):
        self.executable("uname", '#!/bin/sh\necho Linux\n')
        helper = self.root / "scripts/install-linux-source-runtime"
        helper.write_text('''import os, sys
with open(os.environ["SOURCE_TEST_LOG"], "a") as stream:
    stream.write("linux-helper " + " ".join(sys.argv[1:]) + "\\n")
''')
        result = self.run_mode("binary")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("linux-helper --no-start", self.calls())

    def test_headless_linux_keeps_daemon_only_install_and_never_builds_gui(self):
        helper = self.root / "scripts/install-linux-source-runtime"
        helper.write_text('''import os, sys
with open(os.environ["SOURCE_TEST_LOG"], "a") as stream:
    stream.write("linux-helper " + " ".join(sys.argv[1:]) + "\\n")
raise SystemExit(1)
''')
        for tool in ["ffmpeg", "ffprobe"]:
            self.executable(tool, "#!/bin/sh\nexit 0\n")
        daemon = self.root / "plurxd"
        daemon.write_text("#!/bin/sh\nexit 0\n")
        daemon.chmod(0o755)
        result = self.run_mode("linux", "--binary", str(daemon), "--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("would wait for http://127.0.0.1:32400/readyz", result.stdout)
        self.assertEqual(self.calls().strip(), "linux-helper --eligible-session")
        self.assertNotIn("build-complete", self.calls())

    def test_explicit_server_only_route_keeps_custom_prefix_without_companion(self):
        daemon = self.root / "plurxd"
        daemon.write_text("#!/bin/sh\nexit 0\n")
        daemon.chmod(0o755)
        parser = self.root / "parser.wasm"
        parser.write_bytes(b"\0asm\1\0\0\0")
        result = self.install("--server-only", "--binary", str(daemon), "--prefix", str(self.root / "prefix"), "--source-parser", str(parser), "--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("build-macos-source-runtime", result.stdout)
        self.assertNotIn("macos-source", result.stdout)
        self.assertIn("would install Jellyfin FFmpeg", result.stdout)
        self.assertNotIn("build-complete", self.calls())


if __name__ == "__main__":
    unittest.main()
