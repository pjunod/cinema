import importlib.machinery
import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "deploy/apple-runner/plurx-apple-standby"
loader = importlib.machinery.SourceFileLoader("apple_standby", str(SOURCE))
spec = importlib.util.spec_from_loader(loader.name, loader)
standby = importlib.util.module_from_spec(spec)
loader.exec_module(standby)


class Child:
    returncode = None

    def __init__(self):
        self.signals = 0

    def poll(self):
        return self.returncode

    def terminate(self):
        self.signals += 1


class AppleRunnerStandbyTests(unittest.TestCase):
    def setUp(self):
        self.started = []
        self.config = {
            "failure_checks": 3,
            "recovery_checks": 2,
            "runner_command": ["runner", "daemon"],
            "runner_directory": "/tmp",
        }

        def spawn(*args, **kwargs):
            child = Child()
            self.started.append(child)
            return child

        self.controller = standby.Standby(self.config, spawn, lambda *a, **k: None)

    def outage(self):
        for _ in range(3):
            self.controller.step(False)

    def test_available_primary_keeps_backup_out_of_the_pool_even_when_busy(self):
        for _ in range(10):
            self.controller.step(True)
        self.assertEqual(self.started, [])

    def test_an_operator_pause_drains_the_backup_and_offers_no_slot_until_resume(self):
        with tempfile.TemporaryDirectory() as folder:
            marker = Path(folder) / "paused"
            self.config["pause_marker"] = str(marker)
            self.outage()
            child = self.started[0]
            marker.write_text("{}")
            self.controller.step(False)
            self.assertEqual(child.signals, 1)
            child.returncode = 0
            for _ in range(5):
                self.controller.step(False)
            self.assertEqual(len(self.started), 1)
            marker.unlink()
            self.outage()
            self.assertEqual(len(self.started), 2)

    def test_one_failed_probe_cannot_enable_backup(self):
        self.controller.step(False)
        self.controller.step(False)
        self.controller.step(True)
        self.controller.step(False)
        self.assertEqual(self.started, [])
        self.controller.step(False)
        self.controller.step(False)
        self.assertEqual(len(self.started), 1)

    def test_recovery_drains_once_and_does_not_duplicate_an_active_job(self):
        self.outage()
        child = self.started[0]
        self.controller.step(True)
        self.assertEqual(child.signals, 0)
        self.controller.step(True)
        self.assertEqual(child.signals, 1)
        self.outage()  # Primary fails again while the active backup job drains.
        self.assertEqual(child.signals, 1)
        self.assertEqual(len(self.started), 1)
        child.returncode = 0
        self.controller.step(False)
        self.assertEqual(len(self.started), 2)

    def test_recovery_returns_to_standby_after_the_backup_finishes(self):
        self.outage()
        self.controller.step(True)
        self.controller.step(True)
        self.started[0].returncode = 0
        self.controller.step(True)
        self.assertIsNone(self.controller.child)
        self.assertEqual(len(self.started), 1)

    def test_failed_backup_restarts_only_during_an_outage(self):
        self.outage()
        self.started[0].returncode = 1
        self.controller.step(False)
        self.assertEqual(len(self.started), 2)
        self.started[1].returncode = 1
        self.controller.step(True)
        self.assertIsNone(self.controller.child)

    def test_timeout_is_an_unavailable_primary(self):
        config = {
            "probe_command": [sys.executable, "-c", "import time; time.sleep(5)"],
            "probe_timeout_seconds": 0.05,
        }
        self.assertFalse(standby.probe(config))

    def test_primary_health_outputs_only_a_fully_validated_toolchain(self):
        import os
        health = ROOT / "deploy/apple-runner/plurx-apple-primary-health"
        with tempfile.TemporaryDirectory() as folder:
            base = Path(folder)
            shims = {
                "launchctl": '#!/bin/sh\n[ "$PRIMARY_SERVICE_READY" = 1 ] || exit 1\nprintf "state = running\\n"\n',
                "xcodebuild": '#!/bin/sh\nprintf "%s\\n" "$PRIMARY_XCODE_VERSION"\n',
                "curl": '#!/bin/sh\n[ "$PRIMARY_FORGE_READY" = 1 ] || exit 22\n',
            }
            script = health.read_text()
            for original, name in (("/bin/launchctl", "launchctl"),
                                   ("/usr/bin/xcodebuild", "xcodebuild"),
                                   ("/usr/bin/curl", "curl")):
                path = base / name
                path.write_text(shims[name])
                path.chmod(0o755)
                self.assertIn(original, script, "readiness dependency moved; update the execution fixture")
                script = script.replace(original, str(path))
            candidate = base / "health"
            candidate.write_text(script)
            expected = "Xcode 27.2\nBuild version 27B52\n"
            cases = (
                ({}, True),
                ({"PRIMARY_SERVICE_READY": "0"}, False),
                ({"PRIMARY_XCODE_VERSION": "Xcode 27.1\nBuild version 27B52"}, False),
                ({"PRIMARY_XCODE_VERSION": "Xcode 27.2\nBuild version 27WRONG"}, False),
                ({"PRIMARY_FORGE_READY": "0"}, False),
            )
            for changes, healthy in cases:
                with self.subTest(changes=changes):
                    env = dict(os.environ, PRIMARY_SERVICE_READY="1", PRIMARY_FORGE_READY="1",
                               PRIMARY_XCODE_VERSION=expected.rstrip("\n"))
                    env.update(changes)
                    result = subprocess.run(["/bin/bash", str(candidate)], env=env,
                                            capture_output=True, text=True, check=False)
                    self.assertEqual(result.returncode == 0, healthy)
                    self.assertEqual(result.stdout, expected if healthy else "")
                    if healthy:
                        self.assertEqual(len(result.stdout.encode()), 31)

    def test_expected_probe_output_requires_exact_successful_response(self):
        for command, expected, healthy in (
            ('print("primary-ready")', "primary-ready\n", True),
            ('print("wrong-primary")', "primary-ready\n", False),
            ('print("primary-ready"); raise SystemExit(2)', "primary-ready\n", False),
            ('print("primary-ready ")', "primary-ready\n", False),
        ):
            with self.subTest(command=command):
                config = {"probe_command": [sys.executable, "-c", command],
                          "probe_timeout_seconds": 2,
                          "probe_expected_stdout": expected}
                self.assertEqual(standby.probe(config), healthy)

    def test_legacy_probe_without_expected_output_keeps_return_code_semantics(self):
        for code, healthy in ((0, True), (2, False)):
            config = {"probe_command": [sys.executable, "-c", "print('legacy'); raise SystemExit(%d)" % code],
                      "probe_timeout_seconds": 2}
            self.assertEqual(standby.probe(config), healthy)

    def test_expected_probe_output_configuration_requires_string(self):
        import json
        with tempfile.TemporaryDirectory() as folder:
            config = dict(self.config, probe_command=["true"], interval_seconds=20,
                          probe_timeout_seconds=12, runner_directory=folder,
                          probe_expected_stdout=7)
            path = Path(folder) / "standby.json"
            path.write_text(json.dumps(config))
            with self.assertRaisesRegex(ValueError, "probe_expected_stdout must be a string"):
                standby.load_config(path)

    @unittest.skipUnless(Path("/usr/bin/pgrep").is_file(), "native pgrep is required")
    def test_installer_detects_bare_relative_absolute_and_versioned_daemons(self):
        import time
        installer = ROOT / "deploy/apple-runner/install"
        with tempfile.TemporaryDirectory() as directory:
            for name in ("forgejo-runner", "./forgejo-runner", "/tmp/forgejo-runner", "/opt/homebrew/bin/forgejo-runner-13.2.0"):
                with self.subTest(invocation=name):
                    ready = Path(directory) / "ready"
                    ready.unlink(missing_ok=True)
                    child = subprocess.Popen(
                        [name, "-c", "sleep 60 & worker=$!; trap 'kill \"$worker\"; wait \"$worker\"; exit 0' TERM; touch \"$1\"; wait \"$worker\"", "daemon", str(ready)],
                        executable="/bin/bash",
                        cwd=directory, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                    )
                    try:
                        deadline = time.monotonic() + 5
                        while not ready.exists() and time.monotonic() < deadline:
                            time.sleep(0.01)
                        self.assertTrue(ready.exists(), "fixture daemon did not start")
                        result = subprocess.run(
                            ["/bin/bash", str(installer), "--check-unmanaged"],
                            capture_output=True, text=True, check=False,
                        )
                        self.assertEqual(result.returncode, 1)
                        self.assertIn(str(child.pid), result.stderr)
                    finally:
                        child.terminate()
                        child.wait(timeout=5)

    def test_graceful_drain_waits_for_an_in_flight_job(self):
        with tempfile.TemporaryDirectory() as directory:
            ready = Path(directory) / "ready"
            release = Path(directory) / "release"
            finished = Path(directory) / "finished"
            command = [sys.executable, "-c", """
import signal, sys, time
from pathlib import Path
ready, release, finished = map(Path, sys.argv[1:])
draining = False
def stop(*_):
    global draining
    draining = True
signal.signal(signal.SIGTERM, stop)
ready.touch()
while not draining or not release.exists():
    time.sleep(0.01)
finished.touch()
""", str(ready), str(release), str(finished)]
            config = dict(self.config, runner_command=command, runner_directory=directory)
            controller = standby.Standby(config, log=lambda *a, **k: None)
            try:
                for _ in range(3):
                    controller.step(False)
                import time
                deadline = time.monotonic() + 5
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(ready.exists(), "fixture runner did not start")
                controller.step(True)
                controller.step(True)
                self.assertIsNone(controller.child.poll(), "drain killed the active job")
                release.touch()
                controller.close()
                self.assertTrue(finished.exists(), "active job did not finish")
            finally:
                release.touch()
                if controller.child is not None and controller.child.poll() is None:
                    controller.close()


class UserJanitorTests(unittest.TestCase):
    def setUp(self):
        import json
        import os
        from unittest import mock
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.root = Path(self.folder.name).resolve()
        self.runner = self.root / "runner"
        self.runner.mkdir()
        self.cache = self.runner / "cache"
        self.cache.mkdir()
        (self.cache / "bolt.db").write_text("index")
        (self.cache / "cache").mkdir()
        self.blob = self.cache / "cache/blob"
        self.blob.write_bytes(b"blob")
        self.work = self.runner / "_work"
        self.work.mkdir()
        self.now = 2000000000
        os.utime(self.work, (self.now - 121, self.now - 121))
        self.state = self.root / "janitor-state"
        self.runner_config = self.root / "runner.yml"
        self.runner_config.write_text("runner:\n  shutdown_timeout: 3h\ncache:\n  dir: %s\n" % self.cache)
        self.config = {
            "failure_checks": 1, "recovery_checks": 2,
            "runner_command": ["forgejo-runner", "daemon", "--config", str(self.runner_config)],
            "runner_directory": str(self.runner), "janitor": {"state_dir": str(self.state)},
        }
        standby.configure_janitor(self.config)
        self.rows = [(42, 1, "controller")]
        self.started = []

        def spawn(*args, **kwargs):
            child = Child()
            child.pid = 48000 + len(self.started)
            self.started.append(child)
            self.rows.append((child.pid, 42, "forgejo-runner"))
            return child

        self.controller = standby.Standby(self.config, spawn=spawn, log=lambda *a, **k: None)
        self.janitor = standby.UserJanitor(self.config, clock=lambda: self.now,
                                          processes=lambda: self.rows)
        self.controller.janitor = self.janitor
        self.disk = mock.patch.object(standby.shutil, "disk_usage", return_value=
                                      standby.shutil._ntuple_diskusage(100 << 30, 40 << 30, 60 << 30))
        self.disk_mock = self.disk.start()
        self.addCleanup(self.disk.stop)

    def pressure(self):
        # Sparse logical data exercises full-cache removal without allocating
        # gigabytes in the test workspace.
        with self.blob.open("wb") as stream:
            stream.truncate(21 << 30)

    def start_runner(self):
        self.controller.child = self.controller.spawn()
        return self.controller.child

    def finish_runner(self, keep_descendant=False):
        self.controller.child.returncode = 0
        self.rows = [row for row in self.rows
                     if row[0] != self.controller.child.pid and
                     (keep_descendant or row[1] != self.controller.child.pid)]

    def receipt(self):
        import json
        return json.loads((self.state / "last-run.json").read_text())

    def test_idle_under_budget_does_not_drain_or_reset(self):
        child = self.start_runner()
        self.controller.step(False)
        self.assertEqual(child.signals, 0)
        self.assertEqual(self.blob.read_bytes(), b"blob")
        self.assertEqual(self.receipt()["state"], "healthy")
        self.assertEqual(self.receipt()["instances"], 1)
        self.assertEqual(self.receipt()["reset"], 0)

    def test_active_work_defers_cache_reset_without_signalling_job(self):
        self.pressure()
        child = self.start_runner()
        self.rows.append((49000, child.pid, "xcodebuild"))
        self.controller.step(False)
        self.assertEqual(child.signals, 0)
        self.assertTrue((self.cache / "bolt.db").exists())
        self.assertTrue(self.blob.exists())
        self.assertEqual(self.receipt()["state"], "waiting_for_idle")
        self.assertTrue(self.janitor.pending)

    def test_cache_reset_waits_for_graceful_exit_and_resets_index_and_blobs_together(self):
        self.pressure()
        child = self.start_runner()
        self.controller.step(False)
        self.assertEqual(child.signals, 1)
        self.assertTrue((self.cache / "bolt.db").exists())
        self.assertEqual(self.receipt()["state"], "waiting_for_exit")
        self.controller.step(False)
        self.assertEqual(child.signals, 1)
        self.assertTrue(self.blob.exists())
        self.finish_runner()
        self.controller.step(True)
        self.assertFalse(self.cache.exists())
        self.assertFalse((self.runner / "cache.janitor-retired").exists())
        self.assertTrue(self.work.exists())
        self.assertEqual(self.receipt()["state"], "reset")
        self.assertEqual(self.receipt()["reset"], 1)
        self.assertEqual(self.receipt()["failures"], 0)

    def test_fresh_work_and_orphaned_job_keep_spawn_and_deletion_blocked(self):
        import os
        self.pressure()
        child = self.start_runner()
        self.rows.append((49000, child.pid, "xcodebuild"))
        self.controller.step(True)
        self.controller.step(True)  # Normal recovery may gracefully drain.
        self.assertEqual(child.signals, 1)
        self.finish_runner(keep_descendant=True)
        self.rows = [(pid, 1 if pid == 49000 else parent, command)
                     for pid, parent, command in self.rows]
        self.controller.step(False)
        self.assertTrue(self.cache.exists())
        self.assertEqual(len(self.started), 1)
        self.rows = [row for row in self.rows if row[0] != 49000]
        os.utime(self.work, (self.now, self.now))
        self.controller.step(False)
        self.assertTrue(self.cache.exists())
        self.assertEqual(len(self.started), 1)
        self.now += 121
        self.controller.step(False)
        self.assertFalse(self.cache.exists())
        self.assertEqual(len(self.started), 2)

    def test_operator_pause_is_preserved_through_reset(self):
        marker = self.root / "operator-pause"
        marker.write_text("operator maintenance")
        self.config["pause_marker"] = str(marker)
        self.pressure()
        child = self.start_runner()
        self.controller.step(False)
        self.assertEqual(child.signals, 1)
        self.finish_runner()
        self.controller.step(False)
        self.assertFalse(self.cache.exists())
        self.assertEqual(marker.read_text(), "operator maintenance")
        for _ in range(3):
            self.controller.step(False)
        self.assertEqual(len(self.started), 1)

    def test_resume_does_not_bypass_pending_maintenance(self):
        marker = self.root / "operator-pause"
        marker.touch()
        self.config["pause_marker"] = str(marker)
        self.pressure()
        child = self.start_runner()
        self.controller.step(False)
        marker.unlink()  # ci-fleet Resume cannot release controller state.
        for _ in range(3):
            self.controller.step(False)
        self.assertEqual(len(self.started), 1)
        self.assertEqual(child.signals, 1)
        self.assertTrue(self.cache.exists())
        self.finish_runner()
        self.controller.step(False)
        self.assertFalse(self.cache.exists())
        self.assertEqual(len(self.started), 2)

    def test_failed_deletion_never_restarts_over_half_an_index(self):
        self.pressure()
        child = self.start_runner()
        self.controller.step(False)
        self.finish_runner()

        def fail_delete(path):
            self.assertFalse(self.cache.exists())
            self.assertEqual((path / "bolt.db").read_text(), "index")
            self.assertTrue((path / "cache/blob").exists())
            raise PermissionError("fixture deletion refused")

        self.janitor.remove = fail_delete
        self.controller.step(False)
        retired = self.runner / "cache.janitor-retired"
        self.assertTrue((retired / "bolt.db").exists())
        self.assertFalse(self.cache.exists())
        self.assertEqual(len(self.started), 2)
        self.assertEqual(self.receipt()["state"], "error")
        self.assertEqual(self.receipt()["failures"], 1)
        # A subsequent pass must expose the retained failed cleanup, even
        # when the replacement runner has recreated a small healthy cache.
        self.cache.mkdir()
        (self.cache / "bolt.db").write_text("new index")
        self.now += 3601
        self.controller.step(False)
        self.assertEqual(self.receipt()["state"], "error")
        self.assertEqual((self.cache / "bolt.db").read_text(), "new index")

    def test_unknown_process_state_defers_without_terminating_or_deleting(self):
        self.pressure()
        child = self.start_runner()
        self.rows = []  # Missing tracked PID is unknown, never proof of idle.
        self.controller.step(False)
        self.assertEqual(child.signals, 0)
        self.assertTrue(self.blob.exists())
        self.assertEqual(self.receipt()["state"], "error")
        self.assertEqual(self.receipt()["failures"], 1)

    def test_below_reserve_only_resets_a_useful_cache(self):
        self.disk_mock.return_value = standby.shutil._ntuple_diskusage(100 << 30, 80 << 30, 20 << 30)
        self.controller.step(False)
        self.assertEqual(self.receipt()["state"], "below_reserve_small_cache")
        self.assertEqual(self.receipt()["over_budget"], 1)
        self.assertTrue(self.blob.exists())
        self.now += 3601
        with self.blob.open("wb") as stream:
            stream.truncate(2 << 30)
        child = self.controller.child
        self.controller.step(False)
        self.assertEqual(child.signals, 1)
        self.assertEqual(self.receipt()["state"], "waiting_for_exit")

    def test_zero_graceful_shutdown_is_rejected(self):
        self.runner_config.write_text("runner:\n  shutdown_timeout: 0s\ncache:\n  dir: %s\n" % self.cache)
        with self.assertRaisesRegex(ValueError, "nonzero runner.shutdown_timeout"):
            standby.configure_janitor(self.config)

    def test_unsafe_cache_paths_and_short_quiet_window_are_rejected(self):
        import copy
        self.runner_config.write_text("runner:\n  shutdown_timeout: 3h\ncache:\n  dir: /outside/cache\n")
        with self.assertRaisesRegex(ValueError, "cache.dir"):
            standby.configure_janitor(copy.deepcopy(self.config))
        self.runner_config.write_text("runner:\n  shutdown_timeout: 3h\ncache:\n  dir: %s\n" % self.cache)
        config = copy.deepcopy(self.config)
        config["janitor"]["quiet_seconds"] = 119
        with self.assertRaisesRegex(ValueError, "at least 120"):
            standby.configure_janitor(config)
        import shutil
        shutil.rmtree(self.cache)
        external = self.root / "external"
        external.mkdir()
        (external / "bolt.db").write_text("preserve external")
        self.cache.symlink_to(external, target_is_directory=True)
        self.controller.step(False)
        self.assertEqual((external / "bolt.db").read_text(), "preserve external")
        self.assertEqual(self.receipt()["state"], "error")


if __name__ == "__main__":
    unittest.main()
