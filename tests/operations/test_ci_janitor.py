"""The fleet janitor bounds what a CI job cannot, and never costs a slot.

A Forgejo runner's cache server evicts nothing (forgejo-runner 13.1.0 has no
size cap, TTL or GC for `cache.dir`), and a job cannot delete from it: index
rows and blobs have to go together, which means stopping the runner, which a
job running on that runner cannot do. So the janitor does it from outside, and
the three things that must hold are that it never resets a runner that is
working, that it always brings a runner it stopped back, and that it does
nothing at all while the cache is inside its budget.
"""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "deploy/runner-janitor/plurx-ci-janitor"
UNIT = "forgejo-runner-gha-test-01.service"

SYSTEMCTL = """#!/bin/sh
log() { printf '%s\\n' "$*" >> "$FIXTURE_LOG"; }
case "$1 $2" in
  "list-units --type=service")
    printf '%s loaded active running Forgejo runner\\n' "$FIXTURE_UNIT"
    exit 0 ;;
esac
if [ "$1" = show ]; then
  case "$3" in
    ExecStart) printf '{ path=/usr/local/bin/forgejo-runner ; argv[]=/usr/local/bin/forgejo-runner daemon -c %s ; }\\n' "$FIXTURE_CONFIG" ;;
    MainPID) printf '%s\\n' "$FIXTURE_MAIN_PID" ;;
    ControlGroup) printf '/system.slice/%s\\n' "$FIXTURE_UNIT" ;;
  esac
  exit 0
fi
case "$1" in
  stop) log "stop $2"; exit "$FIXTURE_STOP_STATUS" ;;
  start) log "start $2"; exit 0 ;;
esac
exit 0
"""

DF = """#!/bin/sh
printf '%s\\n' 'Filesystem 1024-blocks Used Available Capacity Mounted'
printf 'fixture %s 0 %s 50%% /fixture\\n' "$FIXTURE_FS_KB" "$FIXTURE_AVAIL_KB"
"""

DU = """#!/bin/sh
shift $(($# - 1))
printf '%s\\t%s\\n' "$FIXTURE_USED_KB" "$1"
"""

DOCKER = """#!/bin/sh
exit 1
"""


class JanitorContractCase(unittest.TestCase):
    def setUp(self):
        subprocess.run(["bash", "-n", str(SCRIPT)], check=True)
        self._directory = tempfile.TemporaryDirectory()
        fixture = Path(self._directory.name)
        self.addCleanup(self._directory.cleanup)

        self.bin = fixture / "bin"
        self.bin.mkdir()
        for name, body in (
            ("systemctl", SYSTEMCTL),
            ("df", DF),
            ("du", DU),
            ("docker", DOCKER),
        ):
            path = self.bin / name
            path.write_text(body, encoding="utf-8")
            path.chmod(0o755)

        self.runner_root = fixture / "opt/forgejo-runner"
        self.cache = self.runner_root / "cache"
        (self.cache / "cache/0a").mkdir(parents=True)
        (self.cache / "bolt.db").write_bytes(b"index")
        (self.cache / "cache/0a/11").write_bytes(b"blob")
        self.work = self.runner_root / "_work/plurx"
        self.work.mkdir(parents=True)
        (self.work / "checkout").write_bytes(b"someone else's job")

        self.config = self.runner_root / "config.yml"
        self.config.write_text(
            "runner:\n  capacity: 1\ncache:\n  enabled: true\n"
            f"  dir: {self.cache}\ncontainer:\n  network: host\n",
            encoding="utf-8",
        )

        self.log = fixture / "systemctl.log"
        self.state = fixture / "state"
        self.cgroup = fixture / "cgroup"
        (self.cgroup / "system.slice" / UNIT).mkdir(parents=True)
        self.procs = self.cgroup / "system.slice" / UNIT / "cgroup.procs"
        self.procs.write_text("4242\n", encoding="utf-8")

        self.environment = os.environ.copy()
        self.environment.update(
            {
                "PATH": f"{self.bin}{os.pathsep}{self.environment['PATH']}",
                "PLURX_JANITOR_STATE_DIR": str(self.state),
                "PLURX_JANITOR_CGROUP_ROOT": str(self.cgroup),
                "PLURX_JANITOR_STOP_TIMEOUT": "5",
                "FIXTURE_UNIT": UNIT,
                "FIXTURE_CONFIG": str(self.config),
                "FIXTURE_LOG": str(self.log),
                "FIXTURE_MAIN_PID": "4242",
                "FIXTURE_STOP_STATUS": "0",
                "FIXTURE_FS_KB": str(78 * 1024 * 1024),
                "FIXTURE_AVAIL_KB": str(40 * 1024 * 1024),
                "FIXTURE_USED_KB": str(4 * 1024 * 1024),
            }
        )

    def run_janitor(self, *arguments):
        return subprocess.run(
            [str(SCRIPT), *arguments],
            env=self.environment,
            capture_output=True,
            text=True,
        )

    def systemctl_calls(self):
        if not self.log.exists():
            return []
        return self.log.read_text(encoding="utf-8").split()

    def last_run(self):
        return json.loads((self.state / "last-run.json").read_text(encoding="utf-8"))

    def test_a_cache_inside_its_budget_is_left_completely_alone(self):
        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("within budget", result.stdout)
        self.assertEqual(self.systemctl_calls(), [])
        self.assertTrue((self.cache / "bolt.db").is_file())
        self.assertEqual(self.last_run()["over_budget"], 0)
        self.assertEqual(self.last_run()["reset"], 0)
        self.assertEqual(self.last_run()["instances"], 1)

    def test_an_over_budget_cache_is_reset_whole_and_the_runner_comes_back(self):
        self.environment["FIXTURE_USED_KB"] = str(41 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        # Stopped before the delete and started after it, in that order: the
        # index and the blobs only stay consistent if nothing is serving them.
        self.assertEqual(self.systemctl_calls(), ["stop", UNIT, "start", UNIT])
        self.assertFalse(self.cache.exists())
        # `_work` is not the janitor's business: it holds other jobs' trees.
        self.assertTrue((self.work / "checkout").is_file())
        self.assertEqual(self.last_run()["reset"], 1)
        self.assertEqual(self.last_run()["reclaimed_gb"], 41)

    def test_a_runner_that_is_working_keeps_its_cache(self):
        self.environment["FIXTURE_USED_KB"] = str(41 * 1024 * 1024)
        # A job is a tree of processes under the unit's cgroup beside the
        # daemon. This is the whole idleness signal, so it has to discriminate.
        self.procs.write_text("4242\n5150\n", encoding="utf-8")

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("running a job", result.stdout)
        self.assertEqual(self.systemctl_calls(), [])
        self.assertTrue((self.cache / "bolt.db").is_file())
        # Over budget and deliberately not acted on: the state file says both,
        # so a runner that is never idle enough to reset is visible rather than
        # silently skipped forever.
        self.assertEqual(self.last_run()["over_budget"], 1)
        self.assertEqual(self.last_run()["reset"], 0)
        self.assertEqual(self.last_run()["reclaimed_gb"], 0)

    def test_a_runner_that_will_not_stop_is_never_left_stopped(self):
        self.environment["FIXTURE_USED_KB"] = str(41 * 1024 * 1024)
        self.environment["FIXTURE_STOP_STATUS"] = "1"

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("did not stop", result.stdout)
        self.assertEqual(self.systemctl_calls(), ["stop", UNIT, "start", UNIT])
        self.assertTrue((self.cache / "bolt.db").is_file())

    def test_dry_run_decides_out_loud_and_deletes_nothing(self):
        self.environment["FIXTURE_USED_KB"] = str(41 * 1024 * 1024)

        result = self.run_janitor("--dry-run")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("would reset", result.stdout)
        self.assertEqual(self.systemctl_calls(), [])
        self.assertTrue((self.cache / "bolt.db").is_file())
        self.assertFalse((self.state / "last-run.json").exists())

    def test_the_blob_tree_alone_is_enough_to_recognize_a_cache_server(self):
        """`gha-nuc4-general-01` had 13G of blobs and no `bolt.db` beside them.

        Requiring both markers made the janitor walk past the fullest runner in
        the fleet and report nothing, which is the failure mode this whole
        thing exists to end.
        """
        (self.cache / "bolt.db").unlink()
        self.environment["FIXTURE_USED_KB"] = str(41 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.systemctl_calls(), ["stop", UNIT, "start", UNIT])
        self.assertFalse(self.cache.exists())

    def test_a_directory_that_is_neither_index_nor_blobs_is_not_a_cache(self):
        (self.cache / "bolt.db").unlink()
        (self.cache / "cache/0a/11").unlink()
        (self.cache / "cache/0a").rmdir()
        (self.cache / "cache").rmdir()
        self.environment["FIXTURE_USED_KB"] = str(41 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.systemctl_calls(), [])
        self.assertEqual(self.last_run()["instances"], 0)

    def test_pressure_alone_resets_a_cache_that_is_under_budget(self):
        # 4 G cached is well inside the 20 G budget, but 9 G free on a 78 G
        # volume is below the 20 % reserve, and the reserve is what keeps the
        # next job from dying as a linker bus error.
        self.environment["FIXTURE_AVAIL_KB"] = str(9 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.systemctl_calls(), ["stop", UNIT, "start", UNIT])
        self.assertFalse(self.cache.exists())

    def test_a_cache_dir_the_config_points_somewhere_else_is_refused(self):
        """The delete is a whole directory, so the path check is the safety."""
        elsewhere = Path(self._directory.name) / "var/lib/plurxd"
        elsewhere.mkdir(parents=True)
        (elsewhere / "bolt.db").write_bytes(b"not a runner cache")
        (elsewhere / "media.db").write_bytes(b"precious")
        self.config.write_text(
            f"cache:\n  enabled: true\n  dir: {elsewhere}\n", encoding="utf-8"
        )
        self.environment["FIXTURE_USED_KB"] = str(41 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.systemctl_calls(), [])
        self.assertTrue((elsewhere / "media.db").is_file())
        self.assertEqual(self.last_run()["instances"], 0)

    def test_the_installed_timer_and_unit_agree_with_the_script(self):
        service = (
            ROOT / "deploy/runner-janitor/plurx-ci-janitor.service"
        ).read_text(encoding="utf-8")
        timer = (ROOT / "deploy/runner-janitor/plurx-ci-janitor.timer").read_text(
            encoding="utf-8"
        )
        installer = (ROOT / "deploy/runner-janitor/install").read_text(
            encoding="utf-8"
        )

        self.assertIn("ExecStart=/usr/local/bin/plurx-ci-janitor", service)
        self.assertIn("OnCalendar=hourly", timer)
        self.assertIn("Persistent=true", timer)
        self.assertIn("systemctl enable --now plurx-ci-janitor.timer", installer)
        # Without this drop-in systemd kills a running job 90 seconds into a
        # graceful stop, and the janitor becomes the thing that breaks CI.
        self.assertIn("TimeoutStopSec=30min", installer)
        self.assertIn("--dry-run", installer)

        # A host with no checkout still installs in one command, because a
        # fleet fix that takes four gets applied to two hosts.
        bootstrap = (ROOT / "deploy/runner-janitor/bootstrap").read_text(
            encoding="utf-8"
        )
        for name in (
            "plurx-ci-janitor",
            "plurx-ci-janitor.service",
            "plurx-ci-janitor.timer",
            "install",
        ):
            self.assertIn(name, bootstrap)
        self.assertIn("raw/branch/$REF/deploy/runner-janitor", bootstrap)
        self.assertIn('exec "$work/install"', bootstrap)


if __name__ == "__main__":
    unittest.main()
