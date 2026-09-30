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


if __name__ == "__main__":
    unittest.main()
