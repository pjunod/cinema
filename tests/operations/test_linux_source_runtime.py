"""Native GUI source installs preserve service policy and real package ownership."""
import argparse
import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/install-linux-source-runtime"
loader = importlib.machinery.SourceFileLoader("linux_source_runtime", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
runtime = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = runtime
loader.exec_module(runtime)


class LinuxSourceRuntimeTests(unittest.TestCase):
    def test_session_requires_local_active_owning_gui_display_and_bus(self):
        properties = {"User": "1000", "Active": "yes", "Remote": "no", "Class": "user", "Type": "wayland", "Seat": "seat0"}
        environment = {"WAYLAND_DISPLAY": "wayland-0"}
        self.assertTrue(runtime.session_policy(1000, properties, environment, container=False, display_socket=True, session_bus=True))
        for key, value in [("User", "1001"), ("Active", "no"), ("Remote", "yes"), ("Class", "greeter"), ("Type", "tty"), ("Seat", "")]:
            with self.subTest(key=key):
                changed = {**properties, key: value}
                self.assertFalse(runtime.session_policy(1000, changed, environment, container=False, display_socket=True, session_bus=True))
        for option in ("container", "display_socket", "session_bus"):
            options = {"container": False, "display_socket": True, "session_bus": True}
            options[option] = not options[option]
            self.assertFalse(runtime.session_policy(1000, properties, environment, **options))
        self.assertFalse(runtime.session_policy(0, {**properties, "User": "0"}, environment, container=False, display_socket=True, session_bus=True))
        self.assertFalse(runtime.session_policy(1000, properties, {**environment, "SSH_CONNECTION": "remote"}, container=False, display_socket=True, session_bus=True))
        x11 = {**properties, "Type": "x11"}
        self.assertTrue(runtime.session_policy(1000, x11, {"DISPLAY": ":0"}, container=False, display_socket=True, session_bus=True))
        self.assertFalse(runtime.session_policy(1000, x11, {"DISPLAY": "localhost:10.0"}, container=False, display_socket=True, session_bus=True))

    def test_headless_probe_never_installs_dependencies_or_builds_gtk(self):
        with mock.patch.object(runtime.platform, "system", return_value="Linux"), mock.patch.object(runtime, "eligible_session", side_effect=ValueError("headless")), mock.patch.object(runtime, "prepare") as prepare, mock.patch.object(runtime, "run") as run, contextlib.redirect_stdout(io.StringIO()) as receipt:
            self.assertEqual(runtime.main(["--eligible-session"]), 1)
        self.assertFalse(json.loads(receipt.getvalue())["eligible"])
        prepare.assert_not_called()
        run.assert_not_called()

    def test_sudo_caller_drops_build_privileges_to_verified_gui_owner(self):
        session = runtime.Session(1000, "desktop", {"HOME": "/fixture/home"})
        account = mock.Mock(pw_name="desktop", pw_uid=1000, pw_gid=1000)
        with mock.patch.object(runtime.os, "geteuid", return_value=0), mock.patch.object(runtime.pwd, "getpwuid", return_value=account), mock.patch.object(runtime.os, "initgroups") as groups, mock.patch.object(runtime.os, "setgid") as gid, mock.patch.object(runtime.os, "setuid") as uid, mock.patch.dict(runtime.os.environ, {}, clear=True):
            runtime.owning_user_context(session)
            groups.assert_called_once_with("desktop", 1000)
            gid.assert_called_once_with(1000)
            uid.assert_called_once_with(1000)
            self.assertEqual(dict(runtime.os.environ), session.environment)

    def test_newer_or_nonstandard_installed_versions_are_not_overwritten(self):
        runtime.permit_version("0.7.0-1", "0.7.0")
        runtime.permit_version("0.6.0-1", "0.7.0")
        with self.assertRaisesRegex(ValueError, "downgrade"):
            runtime.permit_version("0.8.0-1", "0.7.0")
        with mock.patch.object(runtime, "output", return_value="installed 2:0.7.0-1"):
            with self.assertRaisesRegex(ValueError, "nonstandard"):
                runtime.installed_version("deb")

    def install_fixture(self, *, existing=False, enabled=False, running=False, no_start=False, fail_prepare=False, native_record=False, legacy=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            events = []
            legacy_files = []
            if legacy:
                unit = root / "plurxd.service"
                unit.write_text("identified fixture unit")
                legacy_files.append(unit)
            if native_record:
                (root / "installation.json").write_text("managed fixture")
            session = runtime.Session(1000, "desktop", {"HOME": str(root)})
            def command(argv, **kwargs):
                events.append(list(map(str, argv)))
                return subprocess.CompletedProcess(argv, 0, stdout="")
            def prepare(stage, args, packager):
                events.append(["prepared"])
                if fail_prepare:
                    raise ValueError("missing source companion")
                return "0.7.0", [stage / "server.deb", stage / "desktop.deb"]
            with mock.patch.object(runtime, "ROOT", root), mock.patch.object(runtime, "RECORD", root / "installation.json"), mock.patch.object(runtime, "DROPINS", root / "no-dropins"), mock.patch.object(runtime, "eligible_session", return_value=session), mock.patch.object(runtime, "native_format", return_value="deb"), mock.patch.object(runtime, "source_identity", return_value=("a" * 40, "0.7.0")), mock.patch.object(runtime, "installed_version", return_value="0.7.0-1" if existing else None), mock.patch.object(runtime, "inspect_legacy", return_value=legacy_files), mock.patch.object(runtime, "stop_legacy_main", side_effect=lambda: events.append(["legacy-drained"])), mock.patch.object(runtime, "policy_snapshot", return_value=(existing, enabled, running)), mock.patch.object(runtime, "prepare", side_effect=prepare), mock.patch.object(runtime, "run", side_effect=command), mock.patch.object(runtime.subprocess, "Popen") as launch, mock.patch.object(runtime.os, "geteuid", return_value=1000), mock.patch.object(runtime.os, "getuid", return_value=1000), contextlib.redirect_stdout(io.StringIO()):
                args = argparse.Namespace(binary=None, no_start=no_start, dry_run=False)
                if fail_prepare:
                    with self.assertRaisesRegex(ValueError, "missing source"):
                        runtime.install(args)
                else:
                    runtime.install(args)
            return events, launch.call_args_list

    def test_fresh_binary_gui_install_stays_stopped_with_both_startup_preferences_off(self):
        events, launches = self.install_fixture(no_start=True)
        self.assertIn(["systemctl", "disable", "plurxd.service"], events)
        self.assertNotIn([str(runtime.CONTROL), "start"], events)
        self.assertIn([str(runtime.CONTROL), "startup", "set", "tray", "off"], events)
        self.assertEqual(len(launches), 1)
        self.assertEqual(launches[0].args[0], ["/usr/bin/plurx-tray", "--window"])
        install = next(event for event in events if event[:2] == ["apt-get", "install"])
        self.assertIn("--reinstall", install)
        self.assertLess(events.index(["prepared"]), events.index(install))

    def test_existing_disabled_stopped_policy_survives_normal_gui_install(self):
        events, launches = self.install_fixture(existing=True)
        self.assertIn(["systemctl", "disable", "plurxd.service"], events)
        self.assertNotIn([str(runtime.CONTROL), "start"], events)
        self.assertNotIn([str(runtime.CONTROL), "startup", "set", "tray", "off"], events)
        self.assertEqual(len(launches), 1)
        self.assertEqual(launches[0].args[0], ["/usr/bin/plurx-tray", "--window"])

    def test_existing_running_enabled_policy_survives_no_start_install(self):
        events, _ = self.install_fixture(existing=True, enabled=True, running=True, no_start=True, native_record=True)
        self.assertIn(["systemctl", "enable", "plurxd.service"], events)
        self.assertIn([str(runtime.CONTROL), "start"], events)
        self.assertLess(events.index(["prepared"]), events.index([str(runtime.CONTROL), "stop", "--confirm-interruption"]))
        self.assertNotIn([str(runtime.CONTROL), "startup", "set", "tray", "off"], events)

    def test_legacy_long_drain_never_escalates_or_treats_pid_or_state_alone_as_stopped(self):
        observations = [("active", 404), ("inactive", 404), ("active", 0), ("inactive", 0)]
        with mock.patch.object(runtime, "legacy_service_observation", side_effect=observations), mock.patch.object(runtime, "run") as run, mock.patch.object(runtime.time, "monotonic", return_value=0), mock.patch.object(runtime.time, "sleep") as sleep:
            runtime.stop_legacy_main()
            self.assertEqual(sleep.call_count, 2)
            self.assertEqual(run.call_args.args[0], ["systemctl", "kill", "--kill-whom=main", "--signal=SIGTERM", "plurxd.service"])
            self.assertEqual(run.call_count, 1)
        with mock.patch.object(runtime, "legacy_service_observation", return_value=("active", 404)), mock.patch.object(runtime, "run") as run, mock.patch.object(runtime.time, "monotonic", side_effect=[0, 91]), mock.patch.object(runtime.time, "sleep") as sleep:
            with self.assertRaisesRegex(ValueError, "still draining"):
                runtime.stop_legacy_main()
            self.assertEqual(run.call_args.args[0], ["systemctl", "kill", "--kill-whom=main", "--signal=SIGTERM", "plurxd.service"])
            self.assertEqual(run.call_count, 1)
            sleep.assert_not_called()

    def test_enabled_legacy_registration_is_disabled_before_unit_removal_then_restored(self):
        events, _ = self.install_fixture(existing=True, enabled=True, running=True, legacy=True)
        disable = events.index(["systemctl", "disable", "plurxd.service"])
        remove = next(index for index, event in enumerate(events) if event[:2] == ["rm", "--"])
        install = next(index for index, event in enumerate(events) if event[:2] == ["apt-get", "install"])
        enable = events.index(["systemctl", "enable", "plurxd.service"])
        self.assertLess(events.index(["prepared"]), events.index(["legacy-drained"]))
        self.assertLess(events.index(["legacy-drained"]), disable)
        self.assertLess(disable, remove)
        self.assertLess(remove, install)
        self.assertLess(install, enable)
        self.assertIn([str(runtime.CONTROL), "start"], events)
        self.assertNotIn(["systemctl", "stop", "plurxd.service"], events)

    def test_failed_staging_never_stops_or_replaces_existing_service(self):
        events, launches = self.install_fixture(existing=True, running=True, fail_prepare=True)
        self.assertEqual(events, [["prepared"]])
        self.assertEqual(launches, [])

    def test_unknown_custom_legacy_unit_and_symlink_are_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            unit = Path(directory) / "plurxd.service"
            dropins = Path(directory) / "plurxd.service.d"
            unit.write_text("[Service]\nExecStart=/operator/custom-server\n")
            with self.assertRaisesRegex(ValueError, "unrecognized"):
                runtime.inspect_legacy(unit, dropins)
            unit.unlink()
            unit.symlink_to(ROOT / "deploy/plurxd.service")
            with self.assertRaisesRegex(ValueError, "unrecognized"):
                runtime.inspect_legacy(unit, dropins)

    def test_compose_companion_staging_builds_only_native_tray_without_host_server(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            controls = root / "build/release"
            controls.mkdir(parents=True)
            elf = bytearray(32)
            elf[:6] = bytes.fromhex("7f454c460201")
            elf[18:20] = bytes.fromhex("3e00")
            (controls / "plurx-tray").write_bytes(elf)
            (controls / "plurx-tray").chmod(0o755)
            (root / "deploy/desktop/icons").mkdir(parents=True)
            (root / "deploy/desktop/tv.plurx.Server.svg").write_text("<svg/>")
            (root / "deploy/desktop/icons/tv.plurx.Server-running-symbolic.svg").write_text("<svg/>")
            session = runtime.Session(os.getuid(), "desktop", {"HOME": str(root)})
            destination = root / "companion"
            with mock.patch.object(runtime, "ROOT", root), mock.patch.object(runtime, "eligible_session", return_value=session), mock.patch.object(runtime, "owning_user_context"), mock.patch.object(runtime, "native_format", return_value="deb"), mock.patch.object(runtime, "build_context", return_value=("a" * 40, "0.7.0", "amd64", Path("/fixture/cargo"), controls, {})), mock.patch.object(runtime, "source_identity", return_value=("a" * 40, "0.7.0")), mock.patch.object(runtime, "output", return_value="plurx-tray 0.7.0 (" + "a" * 40 + ")"), mock.patch.object(runtime, "run") as run, mock.patch.object(runtime, "nfpm_tool") as nfpm, mock.patch.object(runtime, "policy_snapshot") as policy:
                receipt = runtime.stage_tray(destination)
            self.assertEqual(receipt["tray"], str(destination / "plurx-tray"))
            self.assertEqual((destination / "plurx-tray").read_bytes(), elf)
            self.assertTrue((destination / "icons/tv.plurx.Server.svg").is_file())
            self.assertEqual(len(run.call_args_list), 1)
            self.assertEqual(list(map(str, run.call_args.args[0])), ["/fixture/cargo", "build", "--locked", "--release", "-p", "plurx-tray"])
            nfpm.assert_not_called()
            policy.assert_not_called()

    def test_companion_staging_rejects_headless_or_existing_output_before_dependencies(self):
        with mock.patch.object(runtime, "eligible_session", side_effect=ValueError("headless")), mock.patch.object(runtime, "build_context") as build:
            with self.assertRaisesRegex(ValueError, "headless"):
                runtime.stage_tray(Path("/unused/companion"))
            build.assert_not_called()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            session = runtime.Session(os.getuid(), "desktop", {"HOME": str(root)})
            with mock.patch.object(runtime, "eligible_session", return_value=session), mock.patch.object(runtime, "owning_user_context"), mock.patch.object(runtime, "build_context") as build:
                with self.assertRaisesRegex(ValueError, "new absolute"):
                    runtime.stage_tray(root)
                build.assert_not_called()

    def test_package_linux_host_format_and_external_policy_keep_real_cohort(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            bins = directory / "bin"
            bins.mkdir()
            # Enough ELF identity for the production packager's architecture guard.
            elf = bytearray(32)
            elf[:6] = bytes.fromhex("7f454c460201")
            elf[18:20] = bytes.fromhex("3e00")
            for name in (*runtime.CONTROLS, "plurxd", "plurx-tray"):
                (bins / name).write_bytes(elf)
                (bins / name).chmod(0o755)
            (bins / "plurxd.dwp").write_bytes(b"paired fixture")
            capture = directory / "capture.jsonl"
            (bins / "nfpm").write_text("#!" + sys.executable + "\n" +
                "import json,os,pathlib,sys\n" +
                "args=sys.argv; fmt=args[args.index('--packager')+1]; out=pathlib.Path(args[args.index('--target')+1]); desktop='desktop-nfpm' in args[args.index('--config')+1]\n" +
                "name='plurx-desktop' if desktop else 'plurxd'; filename=(name+'_0.7.0-1_amd64.deb') if fmt=='deb' else (name+'-0.7.0-1.x86_64.rpm'); (out/filename).write_bytes(b'package')\n" +
                "with open(os.environ['CAPTURE'],'a') as f: f.write(json.dumps({'format':fmt,'desktop':desktop,'hook':pathlib.Path('postinstall').read_text()})+'\\n')\n")
            (bins / "nfpm").chmod(0o755)
            result = subprocess.run([
                str(ROOT / "scripts/package-linux"), "--arch", "amd64", "--version", "0.7.0",
                "--binary", str(bins / "plurxd"), "--control-dir", str(bins), "--desktop-binary", str(bins / "plurx-tray"),
                "--out", str(directory / "packages"), "--packager", "deb", "--service-policy", "external",
            ], env={**os.environ, "PATH": str(bins) + os.pathsep + os.environ.get("PATH", ""), "CAPTURE": str(capture), "PLURX_BUILD_SHA": "a" * 40}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = [json.loads(line) for line in capture.read_text().splitlines()]
            self.assertEqual([(call["format"], call["desktop"]) for call in calls], [("deb", False), ("deb", True)])
            hook = calls[0]["hook"]
            anchor = "# Source installer restores the observed service policy.\nexit 0\n"
            self.assertIn(anchor, hook)
            self.assertLess(hook.index(anchor), hook.index('if [ "$arriving" = 1 ]'))
            self.assertEqual(len(list((directory / "packages").glob("*.deb"))), 2)
            self.assertFalse(list((directory / "packages").glob("*.rpm")))
