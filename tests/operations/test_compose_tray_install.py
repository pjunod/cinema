"""The Linux Compose companion installs host GUI code without a host server."""
import argparse
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import types
import shutil
import signal
import subprocess
import sys
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("compose_tray_install", str(ROOT / "scripts/install-compose-tray"))
spec = importlib.util.spec_from_loader(loader.name, loader)
installer = importlib.util.module_from_spec(spec)
loader.exec_module(installer)
SERVER, DISCOVERY = "a" * 64, "b" * 64


class ComposeTraySelectionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        (self.root / "deploy").mkdir()
        (self.root / "deploy/docker-compose.yml").write_text('name: selected\nservices:\n  plurxd:\n    restart: "${PLURX_CONTAINER_RESTART_POLICY:-unless-stopped}"\n  plurx-discovery:\n    restart: "${PLURX_CONTAINER_RESTART_POLICY:-unless-stopped}"\n')
        (self.root / "deploy/.env").write_text('')
        self.calls = []
        self.server_ids = SERVER
        self.foreign = False
        self.bindings = [{"HostIp": "0.0.0.0", "HostPort": "43210"}, {"HostIp": "::", "HostPort": "43210"}]

    def output(self, argv, **kwargs):
        argv = list(map(str, argv))
        self.calls.append(argv)
        if "context" in argv:
            return "unix:///tmp/selected-docker.sock"
        if "ps" in argv:
            return self.server_ids if argv[-1] == "plurxd" else DISCOVERY
        if "inspect" in argv:
            service = "plurxd" if argv[-1] == SERVER else "plurx-discovery"
            return json.dumps({"project": "selected", "service": service,
                               "working_dir": str(self.root / ("elsewhere" if self.foreign else "deploy")), "ports": self.bindings})
        raise AssertionError(argv)

    def select(self):
        with mock.patch.object(installer, "output", side_effect=self.output), mock.patch.object(installer.shutil, "which", return_value="/usr/bin/docker"), mock.patch.dict(os.environ, {"DOCKER_HOST": ""}):
            return installer.select_deployment(self.root)

    def test_exact_selected_services_and_actual_published_port_are_pinned(self):
        record = self.select()
        self.assertEqual(record["server_id"], SERVER)
        self.assertEqual(record["discovery_id"], DISCOVERY)
        self.assertEqual(record["project"], "selected")
        self.assertEqual(record["browser_origin"], "http://127.0.0.1:43210")
        ps = [call for call in self.calls if "ps" in call]
        self.assertEqual({call[-1] for call in ps}, {"plurxd", "plurx-discovery"})
        for call in ps:
            self.assertIn("--no-trunc", call)
            self.assertIn(str(self.root / "deploy/docker-compose.yml"), call)
        self.assertFalse(any("start" in call or "update" in call or "stop" in call for call in self.calls))

    def test_ambiguous_container_selection_refuses_instead_of_picking_first(self):
        self.server_ids = SERVER + "\n" + "c" * 64
        with self.assertRaisesRegex(ValueError, "exactly one"):
            self.select()
        self.assertFalse(any("inspect" in call and "container" in call for call in self.calls))

    def test_container_from_another_checkout_is_rejected(self):
        self.foreign = True
        with self.assertRaisesRegex(ValueError, "exact Compose directory"):
            self.select()

    def test_remote_docker_endpoint_is_rejected_before_container_queries(self):
        with mock.patch.dict(os.environ, {"DOCKER_HOST": "tcp://remote:2375"}), mock.patch.object(installer.shutil, "which", return_value="/usr/bin/docker"), mock.patch.object(installer, "output") as output:
            with self.assertRaisesRegex(ValueError, "local Docker Unix socket"):
                installer.select_deployment(self.root)
            output.assert_not_called()

    def test_ambiguous_or_non_loopback_ports_are_rejected(self):
        for bindings in [[{"HostIp": "0.0.0.0", "HostPort": "1234"}, {"HostIp": "::", "HostPort": "4321"}],
                         [{"HostIp": "10.42.0.1", "HostPort": "1234"}], []]:
            with self.subTest(bindings=bindings), self.assertRaises(ValueError):
                installer.loopback_origin(bindings)

    def test_custom_compose_without_persistent_restart_contract_is_rejected(self):
        (self.root / "deploy/docker-compose.yml").write_text("services: {}\n")
        with mock.patch.object(installer, "output") as output, self.assertRaisesRegex(ValueError, "durable tray startup"):
            installer.select_deployment(self.root)
        output.assert_not_called()


class ComposeTrayDesktopTests(unittest.TestCase):
    def test_launcher_pins_controller_and_record_and_escapes_field_codes(self):
        text = installer.desktop(Path('/home/operator/My Apps/plurx-tray'), Path('/home/operator/control%tool'), Path('/home/operator/selected.json'), autostart=True)
        self.assertIn('"--control" "/home/operator/control%%tool"', text)
        self.assertIn('"--installation" "/home/operator/selected.json"', text)
        self.assertIn('"--autostart"', text)

    def test_root_caller_gets_actionable_error_without_build_or_inspection(self):
        args = argparse.Namespace(checkout=ROOT, dry_run=False, no_autostart=False, update_mode="source", image_reference=None)
        with mock.patch.object(installer.sys, "platform", "linux"), mock.patch.object(installer.os, "getuid", return_value=0), mock.patch.object(installer, "native_helper") as helper:
            with self.assertRaisesRegex(ValueError, "ordinary Linux graphical login user"):
                installer.install(args)
            helper.assert_not_called()

    def test_dry_run_checks_eligibility_but_never_builds_or_writes(self):
        identity = {"docker": "/usr/bin/docker", "endpoint": "unix:///tmp/docker.sock", "project": "selected",
                    "server_id": SERVER, "discovery_id": DISCOVERY, "browser_origin": "http://127.0.0.1:32400", "checkout": str(ROOT)}
        helper = types.SimpleNamespace(eligible_session=mock.Mock())
        args = argparse.Namespace(checkout=ROOT, dry_run=True, no_autostart=False, update_mode="source", image_reference=None)
        with tempfile.TemporaryDirectory() as work, mock.patch.dict(os.environ, {"XDG_STATE_HOME": work + "/state", "XDG_DATA_HOME": work + "/data", "XDG_CONFIG_HOME": work + "/config"}), mock.patch.object(installer.sys, "platform", "linux"), mock.patch.object(installer.os, "getuid", return_value=os.getuid() or 1000), mock.patch.object(installer, "native_helper", return_value=helper), mock.patch.object(installer, "select_deployment", return_value=identity), mock.patch.object(installer, "safe_path", side_effect=lambda path, **_kwargs: path), mock.patch.object(installer, "private_dir") as private, mock.patch.object(installer, "run") as run, mock.patch.object(installer.subprocess, "Popen") as launch:
            installer.install(args)
            helper.eligible_session.assert_called_once()
            private.assert_not_called()
            run.assert_not_called()
            launch.assert_not_called()
            self.assertEqual(list(Path(work).iterdir()), [])

    def test_install_stages_only_host_tray_and_preserves_existing_login_off(self):
        with tempfile.TemporaryDirectory() as work:
            home = Path(work) / "home"
            home.mkdir()
            uid = os.getuid() or 1000
            identity = {"docker": "/usr/bin/docker", "endpoint": "unix:///tmp/docker.sock", "project": "selected",
                        "server_id": SERVER, "discovery_id": DISCOVERY, "browser_origin": "http://127.0.0.1:32400", "checkout": str(ROOT)}
            calls = []
            def stage(argv, **kwargs):
                calls.append(list(map(str, argv)))
                self.assertIn("--stage-tray", argv)
                destination = Path(argv[-1])
                destination.mkdir()
                (destination / "plurx-tray").write_text("#!/bin/sh\nexit 0\n")
                (destination / "plurx-tray").chmod(0o755)
                (destination / "icons").mkdir()
                (destination / "icons/tv.plurx.Server.svg").write_text("<svg/>\n")
                return {"tray": str(destination / "plurx-tray"), "source_commit": "c" * 40, "version": "0.7.0", "architecture": "amd64"}
            def register(argv, **kwargs):
                values = list(map(str, argv))
                calls.append(values)
                path = Path(values[values.index("--installation") + 1])
                if "register" in values:
                    path.write_text('{"schema_version":1,"installation_id":"fixture-id"}')
                    path.chmod(0o600)
                else:
                    self.assertEqual(values[-4:], ["startup", "set", "tray", "on"])
                    entry = home / "config/autostart" / installer.ENTRY
                    entry.parent.mkdir(parents=True, exist_ok=True)
                    entry.write_text("[Desktop Entry]\nX-Plurx-Installation=fixture-id\n")
            def private(path):
                path.mkdir(parents=True, mode=0o700, exist_ok=True)
            def write(path, text, mode=0o600):
                # Ownership checks are a fixture boundary when CI runs as root
                # while this case simulates an ordinary desktop account.
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
                path.chmod(mode)
            args = argparse.Namespace(checkout=ROOT, dry_run=False, no_autostart=False, update_mode="source", image_reference=None)
            environment = {"XDG_STATE_HOME": str(home / "state"), "XDG_DATA_HOME": str(home / "data"), "XDG_CONFIG_HOME": str(home / "config")}
            helper = types.SimpleNamespace(eligible_session=mock.Mock())
            with mock.patch.dict(os.environ, environment), mock.patch.object(installer.sys, "platform", "linux"), mock.patch.object(installer.os, "getuid", return_value=uid), mock.patch.object(installer.Path, "home", return_value=home), mock.patch.object(installer, "safe_path", side_effect=lambda path, **_kwargs: path), mock.patch.object(installer, "private_dir", side_effect=private), mock.patch.object(installer, "atomic_text", side_effect=write), mock.patch.object(installer, "native_helper", return_value=helper), mock.patch.object(installer, "select_deployment", return_value=identity), mock.patch.object(installer, "build_stage", side_effect=stage), mock.patch.object(installer, "deployment_mode", return_value=("source", None)), mock.patch.object(installer, "run", side_effect=register), mock.patch.object(installer.subprocess, "Popen") as launch:
                installer.install(args)
                autostart = home / "config/autostart" / installer.ENTRY
                self.assertTrue(autostart.is_file())
                launcher = next((home / "data/applications").glob("*.desktop"))
                self.assertIn('"--control"', launcher.read_text())
                self.assertIn('"--installation"', launcher.read_text())
                record = next((home / "state/plurx-compose").glob("*/installation.json"))
                self.assertEqual(record.stat().st_mode & 0o777, 0o600)
                self.assertIn("--window", launch.call_args.args[0])
                autostart.unlink()  # Existing user's explicit login Off.
                installer.install(args)
                self.assertFalse(autostart.exists())
                self.assertTrue(any("--refresh" in call for call in calls))
                self.assertTrue(all("--stage-tray" in call or "register" in call or "startup" in call for call in calls))
                self.assertFalse((home / "plurxd").exists())

    def test_compose_restart_policy_is_bound_to_persisted_environment(self):
        text = (ROOT / "deploy/docker-compose.yml").read_text()
        self.assertEqual(text.count('restart: "${PLURX_CONTAINER_RESTART_POLICY:-unless-stopped}"'), 2)
        self.assertNotIn("restart: unless-stopped", text)


class ComposeTrayBuildAndOwnershipTests(unittest.TestCase):
    def test_native_staging_waits_beyond_inspection_timeout_and_keeps_progress_visible(self):
        process = mock.Mock(pid=4242)
        process.wait.side_effect = [subprocess.TimeoutExpired(["fixture-build"], 1), 0]
        def launch(argv, **kwargs):
            kwargs["stdout"].write(b'{"tray":"/fixture/plurx-tray"}')
            kwargs["stdout"].flush()
            self.assertIsNone(kwargs["stderr"])
            self.assertTrue(kwargs["start_new_session"])
            return process
        with mock.patch.object(installer.subprocess, "Popen", side_effect=launch), mock.patch.object(installer.time, "monotonic", side_effect=[0, 31, 32]), mock.patch.object(installer, "stop_build") as stop:
            self.assertEqual(installer.build_stage(["fixture-build"]), {"tray": "/fixture/plurx-tray"})
            self.assertEqual(process.wait.call_count, 2)
            stop.assert_not_called()

    def test_native_staging_cancellation_cleans_build_group_before_returning(self):
        process = mock.Mock(pid=4242)
        process.wait.side_effect = installer.BuildCancelled(signal.SIGTERM)
        with mock.patch.object(installer.subprocess, "Popen", return_value=process), mock.patch.object(installer, "stop_build") as stop:
            with self.assertRaises(installer.BuildCancelled):
                installer.build_stage(["fixture-build"])
            stop.assert_called_once_with(process)

    def test_build_group_cleanup_only_targets_its_own_process_group(self):
        process = mock.Mock(pid=4242)
        with mock.patch.object(installer.os, "killpg") as kill:
            installer.stop_build(process, grace=0)
        self.assertEqual(kill.call_args_list, [mock.call(4242, signal.SIGTERM), mock.call(4242, signal.SIGKILL)])
        process.wait.assert_called_once_with()

    def test_docker_inspections_keep_short_timeout(self):
        with mock.patch.object(installer, "run", return_value=types.SimpleNamespace(stdout="inspected")) as run:
            self.assertEqual(installer.output(["fixture-docker", "inspect"]), "inspected")
            self.assertEqual(run.call_args.kwargs["timeout"], 30)

    def test_refresh_retains_explicit_published_owner_and_reference(self):
        with tempfile.TemporaryDirectory() as work:
            record = Path(work) / "installation.json"
            record.write_text('{"update_mode":"published","image_reference":"registry.example/plurx:v0.7.0"}')
            record.chmod(0o600)
            args = argparse.Namespace(update_mode=None, image_reference=None)
            self.assertEqual(installer.deployment_mode(args, record), ("published", "registry.example/plurx:v0.7.0"))
            args.update_mode = "source"
            self.assertEqual(installer.deployment_mode(args, record), ("source", None))

    def test_unknown_deployment_requires_choice_before_any_build(self):
        with tempfile.TemporaryDirectory() as work, mock.patch.object(installer, "build_stage") as build:
            args = argparse.Namespace(update_mode=None, image_reference=None)
            with self.assertRaisesRegex(ValueError, "--update-mode source.*--update-mode published"):
                installer.deployment_mode(args, Path(work) / "absent.json")
            build.assert_not_called()

    def test_published_choice_accepts_operator_reference_without_claiming_trust(self):
        with tempfile.TemporaryDirectory() as work:
            args = argparse.Namespace(update_mode="published", image_reference="registry.example/plurx:v0.7.0")
            self.assertEqual(installer.deployment_mode(args, Path(work) / "absent.json"), ("published", args.image_reference))

    def test_ipv6_only_publication_uses_bracketed_loopback(self):
        for binding in ["::1", "::"]:
            self.assertEqual(installer.loopback_origin([{"HostIp": binding, "HostPort": "43210"}]), "http://[::1]:43210")
        self.assertEqual(installer.loopback_origin([{"HostIp": "::1", "HostPort": "43210"}, {"HostIp": "127.0.0.1", "HostPort": "43210"}]), "http://127.0.0.1:43210")


class ComposeTrayInstallerRoutingTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        for name in ["deploy", "scripts", "bin", "data", "home"]:
            (self.root / name).mkdir()
        shutil.copyfile(ROOT / "deploy/install", self.root / "deploy/install")
        (self.root / "deploy/.env").write_text(f"PLURX_DATA={self.root / 'data'}\nPLURX_HTTP_PORT=32400\n")
        (self.root / "deploy/docker-compose.override.yml").write_text("services: {}\n")
        self.log = self.root / "calls"
        self.environment = {**os.environ, "PATH": str(self.root / "bin") + os.pathsep + os.environ["PATH"],
                            "HOME": str(self.root / "home"), "TRAY_ROUTE_LOG": str(self.log), "TRAY_ROUTE_ELIGIBLE": "1"}
        self.tool("id", '#!/bin/sh\ncase "$1" in -u|-g) echo 501;; esac\n')
        self.tool("uname", '#!/bin/sh\necho Linux\n')
        self.tool("docker", '#!/bin/sh\necho "docker $*" >> "$TRAY_ROUTE_LOG"\nexit 0\n')
        self.tool("make", '#!/bin/sh\necho "make $*" >> "$TRAY_ROUTE_LOG"\n')
        self.tool("curl", '#!/bin/sh\necho ready >> "$TRAY_ROUTE_LOG"\necho ready\n')
        (self.root / "bin/python3").symlink_to(sys.executable)
        (self.root / "scripts/install-linux-source-runtime").write_text('''import os, sys
with open(os.environ["TRAY_ROUTE_LOG"], "a") as stream: stream.write("eligible-probe\\n")
raise SystemExit(0 if os.environ["TRAY_ROUTE_ELIGIBLE"] == "1" else 1)
''')
        (self.root / "scripts/install-compose-tray").write_text('''import os, sys
with open(os.environ["TRAY_ROUTE_LOG"], "a") as stream: stream.write("host-companion " + " ".join(sys.argv[1:]) + "\\n")
''')

    def tool(self, name, content):
        path = self.root / "bin" / name
        path.write_text(content)
        path.chmod(0o755)

    def install(self, *args):
        return subprocess.run(["bash", self.root / "deploy/install", "docker", *args], env=self.environment, capture_output=True, text=True)

    def calls(self):
        return self.log.read_text() if self.log.exists() else ""

    def test_default_linux_docker_install_adds_host_companion_after_ready(self):
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        self.assertLess(calls.index("make docker-up"), calls.index("host-companion"))
        self.assertLess(calls.index("ready"), calls.index("host-companion"))
        self.assertIn("host-companion --checkout " + str(self.root), calls)
        self.assertNotIn("install-linux-source-runtime --binary", calls)

    def test_headless_docker_install_skips_host_companion(self):
        self.environment["TRAY_ROUTE_ELIGIBLE"] = "0"
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("make docker-up", self.calls())
        self.assertNotIn("host-companion", self.calls())

    def test_root_docker_install_prints_actionable_user_command(self):
        self.tool("id", '#!/bin/sh\necho 0\n')
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("scripts/install-compose-tray", result.stdout)
        self.assertNotIn("host-companion", self.calls())

    def test_server_only_and_dry_run_never_install_host_gui(self):
        for args in [("--server-only",), ("--dry-run",)]:
            with self.subTest(args=args):
                self.log.unlink(missing_ok=True)
                result = self.install(*args)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertNotIn("host-companion", self.calls())
                self.assertNotIn("eligible-probe", self.calls())


if __name__ == "__main__":
    unittest.main()


class ComposeTrayVersionTests(unittest.TestCase):
    def test_installer_never_labels_container_with_host_tray_build_version(self):
        source = (ROOT / "scripts/install-compose-tray").read_text()
        self.assertNotIn('"--version", facts["version"]', source)
