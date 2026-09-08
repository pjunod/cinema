"""The fleet janitor bounds what a CI job cannot, and never costs a slot.

A Forgejo runner's cache server evicts nothing (forgejo-runner 13.1.0 has no
size cap, TTL or GC for `cache.dir`), and a job cannot delete from it: index
rows and blobs have to go together, which means stopping the runner, which a
job running on that runner cannot do. So the janitor does it from outside, and
the three things that must hold are that it never resets a runner that is
working, that it always brings a runner it stopped back, and that it does
nothing at all while the cache is inside its budget.

A fourth was added on 2026-09-08, after `gha-nuc4-general-01` refused two jobs
in a row while this janitor reported it healthy: **the reserve it keeps has to
be at least the free space a job is refused for not having.** The preflight
demanded 25 G and the reserve was 20 % of the filesystem, 15.6 G on a 78 GB
guest, so at 18 G free both were satisfied at once -- one by refusing work, the
other by doing nothing about it. That band is what `test_the_reserve_is_never`
`_below_what_a_job_is_refused_for` and the parity test below exist to close.
"""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "deploy/runner-janitor/plurx-ci-janitor"
UNIT = "forgejo-runner-gha-test-01.service"
UNIT2 = "forgejo-runner-gha-test-02.service"

SYSTEMCTL = """#!/bin/sh
log() { printf '%s\\n' "$*" >> "$FIXTURE_LOG"; }
case "$1 $2" in
  "list-units --type=service")
    printf '%s loaded active running Forgejo runner\\n' "$FIXTURE_UNIT"
    # A second runner on the same host, when a test asks for one. Four is the
    # number `docs/ci/RUNNER-DISK.md` shows in its sample receipt, so one is
    # the unrepresentative case and anything asserted "once per pass" has to
    # be asserted against more than one.
    [ -n "$FIXTURE_UNIT2" ] &&
      printf '%s loaded active running Forgejo runner\\n' "$FIXTURE_UNIT2"
    exit 0 ;;
esac
if [ "$1" = show ]; then
  config=$FIXTURE_CONFIG
  [ "$5" = "$FIXTURE_UNIT2" ] && config=$FIXTURE_CONFIG2
  case "$3" in
    ExecStart) printf '{ path=/usr/local/bin/forgejo-runner ; argv[]=/usr/local/bin/forgejo-runner daemon -c %s ; }\\n' "$config" ;;
    MainPID) printf '%s\\n' "$FIXTURE_MAIN_PID" ;;
    ControlGroup) printf '/system.slice/%s\\n' "$5" ;;
  esac
  exit 0
fi
case "$1" in
  stop) log "stop $2"; exit "$FIXTURE_STOP_STATUS" ;;
  start) log "start $2"; exit 0 ;;
esac
exit 0
"""

# Path-aware, because Docker is usually on its own filesystem and a single
# static answer cannot express the shape where a demand is met on the runner
# volume and dropped on the Docker one.
DF = """#!/bin/sh
fs=$FIXTURE_FS_KB
avail=$FIXTURE_AVAIL_KB
case "$*" in
  *docker*)
    [ -n "$FIXTURE_DOCKER_FS_KB" ] && fs=$FIXTURE_DOCKER_FS_KB
    [ -n "$FIXTURE_DOCKER_AVAIL_KB" ] && avail=$FIXTURE_DOCKER_AVAIL_KB ;;
esac
printf '%s\\n' 'Filesystem 1024-blocks Used Available Capacity Mounted'
printf 'fixture %s 0 %s 50%% /fixture\\n' "$fs" "$avail"
"""

DU = """#!/bin/sh
shift $(($# - 1))
printf '%s\\t%s\\n' "$FIXTURE_USED_KB" "$1"
"""

# Absent by default: `docker info` fails, so the Docker block is skipped
# entirely. A test that wants it sets FIXTURE_DOCKER_ROOT.
DOCKER = """#!/bin/sh
[ -n "$FIXTURE_DOCKER_ROOT" ] || exit 1
case "$1 $2" in
  "info --format") printf '%s\\n' "$FIXTURE_DOCKER_ROOT"; exit 0 ;;
esac
[ "$1" = info ] && exit 0
printf '%s %s\\n' "$1" "$2" >> "$FIXTURE_DOCKER_LOG"
exit 0
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
                "FIXTURE_UNIT2": "",
                "FIXTURE_CONFIG": str(self.config),
                "FIXTURE_CONFIG2": "",
                "FIXTURE_LOG": str(self.log),
                "FIXTURE_MAIN_PID": "4242",
                "FIXTURE_STOP_STATUS": "0",
                "FIXTURE_DOCKER_ROOT": "",
                "FIXTURE_DOCKER_LOG": str(fixture / "docker.log"),
                "FIXTURE_DOCKER_FS_KB": "",
                "FIXTURE_DOCKER_AVAIL_KB": "",
                "FIXTURE_FS_KB": str(78 * 1024 * 1024),
                "FIXTURE_AVAIL_KB": str(40 * 1024 * 1024),
                "FIXTURE_USED_KB": str(4 * 1024 * 1024),
            }
        )

    def add_second_runner(self):
        """A second runner unit on the same host, with its own cache server.

        `docs/ci/RUNNER-DISK.md`'s own sample receipt says `"instances":4`, so
        a fixture with exactly one runner is the unrepresentative case, and
        anything claimed "once per pass" is unfalsifiable against it.
        """
        root = self.runner_root.parent / "forgejo-runner-2"
        cache = root / "cache"
        (cache / "cache/0a").mkdir(parents=True)
        (cache / "bolt.db").write_bytes(b"index")
        config = root / "config.yml"
        config.write_text(
            f"runner:\n  capacity: 1\ncache:\n  enabled: true\n  dir: {cache}\n",
            encoding="utf-8",
        )
        (self.cgroup / "system.slice" / UNIT2).mkdir(parents=True)
        (self.cgroup / "system.slice" / UNIT2 / "cgroup.procs").write_text(
            "4242\n", encoding="utf-8"
        )
        self.environment["FIXTURE_UNIT2"] = UNIT2
        self.environment["FIXTURE_CONFIG2"] = str(config)
        return cache

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

    def test_the_reserve_is_never_below_what_a_job_is_refused_for(self):
        """`gha-nuc4-general-01`, 2026-09-08, the exact numbers.

        A 78 GB guest with 18 G free and a 4 G cache. The preflight refused two
        jobs in a row for wanting 25 G; 20 % of 78 GB is 15.6 G, so the janitor
        called the same runner healthy and reclaimed nothing, and every re-push
        landed on the same guest because the job's labels pin it there. Both
        rules were satisfied at once and the fleet was stuck between them.
        """
        self.environment["FIXTURE_AVAIL_KB"] = str(18 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("within budget", result.stdout)
        self.assertEqual(self.systemctl_calls(), ["stop", UNIT, "start", UNIT])
        self.assertFalse(self.cache.exists())
        self.assertEqual(self.last_run()["reset"], 1)

    def test_the_janitor_reserve_and_the_preflight_default_are_one_number(self):
        """Two files, one figure, and nothing but this test holding them level.

        The janitor is installed standalone on each host -- it cannot read the
        workflow at run time -- so the shared constant has to be a contract
        rather than an import. If `disk-gb`'s default is ever raised without
        raising `REQUIRED_GB`, the band that refused `gha-nuc4-general-01`
        reopens silently and every runner reports healthy while jobs are turned
        away.

        The number read here is the INPUT DEFAULT, not the shell fallback.
        `action.yml` sets `DISK_GB: ${{ inputs.disk-gb }}` unconditionally, so
        `"${DISK_GB:-25}"` in the step body can never fire -- an earlier
        version of this test read that dead literal and stayed green while the
        governing default moved.
        """
        janitor_gb = self.janitor_required_gb()
        self.assertEqual(janitor_gb, self.preflight_default_gb())

    def test_no_lane_asks_for_more_disk_than_the_janitor_reserves(self):
        """`disk-gb` is per lane, and the janitor has one number for the host.

        A lane that asks for more than the janitor reserves has its own band:
        the fleet refuses that lane while the janitor calls the runner healthy,
        which is the whole defect, narrowed to one workflow. Such a lane is
        allowed, because raising `REQUIRED_GB` to meet it would put the reserve
        over half the disk on a 78 GB guest -- but it has to be named here, so
        that adding another one is a decision rather than an accident.

        `vod_web` asks for 45 G because it carries Playwright browsers and an
        ffmpeg build on top of the workspace. Nothing reclaims the gap between
        25 G and 45 G on its behalf; that lane needs a runner with the room,
        which is placement rather than pruning.

        The exception is counted, not just named. Keying on the value alone
        let a *second*, unrelated lane copy `disk-gb: "45"` and pass in
        silence -- and copying the heavy lane that already exists is the
        likeliest way another one appears.
        """
        known_larger = {"45": ("vod_web: Playwright browsers plus an ffmpeg build", 1)}

        reserved = int(self.janitor_required_gb())
        asked = re.findall(
            # A trailing comment must not hide a value. Requiring end-of-line
            # straight after it meant `disk-gb: "70"  # heavy lane` was never
            # even collected, which is worse than allowing it: the number never
            # reached the comparison at all.
            r"^\s*disk-gb:\s*[\"']?(\d+)[\"']?\s*(?:#.*)?$",
            self.every_workflow_source(),
            re.M,
        )
        larger = [value for value in asked if int(value) > reserved]

        unexplained = sorted(set(value for value in larger if value not in known_larger))
        self.assertEqual(
            unexplained,
            [],
            "a lane asks for more free space than the janitor reserves, and is "
            "not named as a known exception: " + ", ".join(unexplained),
        )
        for value, (reason, expected) in known_larger.items():
            self.assertTrue(reason, "a known exception needs a reason, not a key")
            self.assertEqual(
                larger.count(value),
                expected,
                f"{larger.count(value)} lanes ask for {value}G; {expected} is "
                f"documented ({reason}). Name the new one or lower it.",
            )

    def janitor_required_gb(self):
        found = re.search(
            r"^REQUIRED_GB=\$\{PLURX_JANITOR_REQUIRED_GB:-(\d+)\}",
            SCRIPT.read_text(encoding="utf-8"),
            re.M,
        )
        self.assertIsNotNone(found, "the janitor must name its required GB")
        return found.group(1)

    def preflight_default_gb(self):
        action = (
            ROOT / ".github/actions/cargo-cache/action.yml"
        ).read_text(encoding="utf-8")
        block = re.search(
            r"^  disk-gb:\n(?:.*\n)*?^    default:\s*[\"']?(\d+)[\"']?\s*$",
            action,
            re.M,
        )
        self.assertIsNotNone(block, "the action must declare a disk-gb default")
        return block.group(1)

    def every_workflow_source(self):
        # `.yaml` as well as `.yml`: none exists under `.github/` today, and a
        # guard that silently stops covering a file the day someone spells the
        # extension differently is not a guard.
        return "\n".join(
            path.read_text(encoding="utf-8")
            for extension in ("*.yml", "*.yaml")
            for path in sorted((ROOT / ".github").rglob(extension))
        )

    def test_a_requirement_larger_than_the_disk_is_said_out_loud_not_chased(self):
        """The original bug was a fixed reserve that could never be satisfied.

        100 GiB on a 78 GB guest meant the pruner deleted everything it was
        allowed to and failed anyway. Raising the reserve to meet a job's
        demand must not reintroduce that, so a demand above half the filesystem
        is capped -- and reported, because a bound that silently cannot be met
        is exactly how this went wrong the first time.
        """
        self.environment["PLURX_JANITOR_REQUIRED_GB"] = "60"
        # 25 G free on a 78 GB guest. Capping the demand to half the disk would
        # make the reserve 39 G and reset this cache; ignoring it leaves the
        # 20 % rule, 15.6 G, and 25 G is comfortably inside that. The
        # difference between "capped" and "ignored" is exactly this assertion.
        self.environment["FIXTURE_AVAIL_KB"] = str(25 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no amount of pruning reaches that", result.stderr)
        self.assertIn("cannot run that lane", result.stderr)
        self.assertEqual(self.systemctl_calls(), [])
        self.assertTrue((self.cache / "bolt.db").is_file())

    def test_an_unreachable_demand_is_said_once_a_pass_across_every_runner(self):
        """The assertion that could not fail, made able to.

        A "have I said this already" flag set inside `floor_kb_for` never
        reaches the parent, because that function is only ever called as
        `$(floor_kb_for ...)` and a command substitution is a subshell. With
        one runner and a `docker` fake that exits 1 the floor is computed once
        a pass, so the message appeared once whether the flag worked or not and
        a `count == 1` assertion passed either way. Two runners is what makes
        it a test.
        """
        self.add_second_runner()
        self.environment["PLURX_JANITOR_REQUIRED_GB"] = "60"
        self.environment["FIXTURE_AVAIL_KB"] = str(25 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.last_run()["instances"], 2)
        self.assertEqual(result.stderr.count("no amount of pruning"), 1)
        # Said once, counted every check. Both runners share one `df` answer
        # here, so 2 is two checks against one filesystem -- which is what the
        # field measures and what the doc now says it measures. It is a flag
        # ("something here is on the percentage rule"), not a census.
        self.assertEqual(self.last_run()["demand_dropped"], 2)

    def test_the_reported_reserve_is_the_one_the_script_actually_kept(self):
        """Two copies of the reserve rule is one too many.

        The message used to recompute the floor and skip the 10 GiB minimum
        `floor_kb_for` applies, so on a 40 GiB volume it announced 8 G while
        the script kept 10 G -- on exactly the small hosts the message exists
        for, and with nothing to catch a future change made in one place and
        not the other.
        """
        self.environment["FIXTURE_FS_KB"] = str(40 * 1024 * 1024)
        self.environment["FIXTURE_AVAIL_KB"] = str(19 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        # 20 % of 40 GiB is 8 G, floored at the 10 GiB minimum: 19 G free is
        # inside it, so nothing is taken and the message must say 10, not 8.
        self.assertIn("keeps its 10G reserve", result.stderr)
        self.assertEqual(self.systemctl_calls(), [])
        self.assertEqual(self.last_run()["demand_dropped"], 1)

    def test_a_runner_that_was_not_reset_is_never_reported_as_still_short(self):
        """`reset: 0, short_after: 1` is a state the script must not write.

        `reset_cache` returns 0 whether it deleted anything or declined, so
        checking free space unconditionally after it reported "still short
        after resetting what it may" about a cache nothing had touched. A
        runner merely busy at the top of every hour -- the normal state of a
        working CI host -- would have carried that forever, and the doc tells
        an operator to read it as a host needing more disk. That is the
        opposite conclusion.
        """
        self.environment["FIXTURE_AVAIL_KB"] = str(18 * 1024 * 1024)
        self.environment["FIXTURE_USED_KB"] = str(3 * 1024 * 1024)
        self.procs.write_text("4242\n5150\n", encoding="utf-8")

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("running a job", result.stdout)
        self.assertEqual(self.last_run()["over_budget"], 1)
        self.assertEqual(self.last_run()["reset"], 0)
        self.assertEqual(self.last_run()["short_after"], 0)
        self.assertNotIn("cannot reach it by pruning", result.stderr)

    def test_a_demand_of_exactly_half_the_disk_is_kept_not_dropped(self):
        """`-gt`, not `-ge`, now that the branch drops instead of capping.

        25 G on a 50 GiB volume is entirely attainable -- the host need only
        hold under 25 G -- so dropping the demand at exactly half would
        silently return that host to the 20 % rule this change exists to
        replace, while printing "cannot run that lane", which would be false.
        """
        self.environment["FIXTURE_FS_KB"] = str(50 * 1024 * 1024)
        self.environment["FIXTURE_AVAIL_KB"] = str(20 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("no amount of pruning", result.stderr)
        # 20 G free against a 25 G reserve: short, so it acts.
        self.assertEqual(self.systemctl_calls(), ["stop", UNIT, "start", UNIT])
        self.assertEqual(self.last_run()["required_gb"], 25)
        self.assertEqual(self.last_run()["demand_dropped"], 0)

    def test_a_reset_that_cannot_reach_the_floor_says_so_every_time(self):
        """The invisible hourly loop, made visible.

        The janitor can free the cache and Docker. The OS, the toolchains and
        the 30 G Cargo cache are not its to take, so a host whose freeable
        bytes are smaller than the gap gets stopped, wiped cold and left short
        -- every hour, with `done:` reporting a successful reset each time. The
        heuristic cannot predict that; the receipt can report it.
        """
        # 18 G free against a 25 G reserve, and a 3 G cache. The fixture's
        # `df` is static, so the reset cannot move it -- which is exactly the
        # host that can never reach the floor.
        self.environment["FIXTURE_AVAIL_KB"] = str(18 * 1024 * 1024)
        self.environment["FIXTURE_USED_KB"] = str(3 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.last_run()["reset"], 1)
        self.assertIn("cannot reach it by pruning", result.stderr)
        self.assertEqual(self.last_run()["short_after"], 1)
        self.assertIn("1 still short", result.stdout)

    def test_a_small_host_does_not_get_the_most_aggressive_reserve(self):
        """The pathology a half-the-disk cap would have created.

        `min(REQUIRED_GB, half)` IS `half` for every filesystem under 50 GiB,
        so the smallest hosts in the fleet would have carried a 50 % reserve --
        the most aggressive this script has ever kept -- and pruned hourly,
        forever, chasing a figure the same message calls unreachable. A host
        that cannot free enough for a lane is a placement problem.
        """
        # A 40 GiB guest with 19 G free and a 4 G cache. Half is 20 G, so a cap
        # would reset here; the 20 % rule is 8 G, floored at 10 G, so nothing
        # should happen.
        self.environment["FIXTURE_FS_KB"] = str(40 * 1024 * 1024)
        self.environment["FIXTURE_AVAIL_KB"] = str(19 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no amount of pruning reaches that", result.stderr)
        self.assertEqual(self.systemctl_calls(), [])
        self.assertTrue((self.cache / "bolt.db").is_file())

    def test_a_required_size_that_is_not_whole_gigabytes_is_refused(self):
        """An empty or negative value must not degrade the reserve in silence.

        `BUDGET_GB` has always been validated; `REQUIRED_GB` was not, and an
        unusable value there does not fail -- it drops the floor back to the
        20 % rule that let `gha-nuc4-general-01` sit refused at 18 G while this
        reported healthy.
        """
        # An empty value is NOT in this list and must not be: `${VAR:-25}`
        # treats empty as unset, so it falls back to the default rather than
        # degrading anything. The dangerous shapes are the ones bash accepts as
        # arithmetic or as a word.
        for value in ("-5", "0", "25G", "abc"):
            with self.subTest(value=value):
                self.environment["PLURX_JANITOR_REQUIRED_GB"] = value
                result = self.run_janitor()
                self.assertEqual(result.returncode, 2, result.stdout)
                self.assertIn("whole gigabytes", result.stderr)

    def test_a_delete_that_fails_still_brings_the_runner_back(self):
        """The one invariant this script promises unconditionally.

        `set -e` aborts the shell on a failed `rm -rf`, and a RETURN trap does
        not run on shell exit -- so before this, an EBUSY on a leftover mount
        or an EACCES on another uid's file took the runner out of the fleet
        with nothing left to start it again. The hourly timer would not
        recover it either: nothing here ever starts a runner it did not itself
        stop.
        """
        if os.geteuid() == 0:
            self.skipTest("root deletes anything; the mode below proves nothing")
        self.environment["FIXTURE_USED_KB"] = str(41 * 1024 * 1024)
        undeletable = self.cache / "cache/0a"
        undeletable.chmod(0o500)
        self.addCleanup(undeletable.chmod, 0o700)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("could not fully delete", result.stdout)
        self.assertEqual(self.systemctl_calls(), ["stop", UNIT, "start", UNIT])
        # And the pass still finishes: the receipt and the summary are how a
        # host nobody is watching stays attributable.
        self.assertIn("done:", result.stdout)
        self.assertTrue((self.state / "last-run.json").is_file())

    def test_a_demand_dropped_on_the_docker_filesystem_is_not_silent(self):
        """The same defect this branch is named for, on the widest deleter.

        Docker usually sits on its own filesystem. A 45 GiB Docker volume
        drops a 25 G demand -- 25 is more than half of 45 -- and falls back to
        the 10 GiB minimum, so 12 G free reports "within reserve" and
        `docker image prune -af` never runs, while the reserve configured for
        this host says it should have. None of that reached stderr or the
        receipt, on the one path in this script that deletes images.
        """
        docker_root = Path(self._directory.name) / "var/lib/docker"
        docker_root.mkdir(parents=True)
        self.environment["FIXTURE_DOCKER_ROOT"] = str(docker_root)
        # The runner volume is large and healthy; only Docker's is short.
        self.environment["FIXTURE_FS_KB"] = str(200 * 1024 * 1024)
        self.environment["FIXTURE_AVAIL_KB"] = str(120 * 1024 * 1024)
        self.environment["FIXTURE_DOCKER_FS_KB"] = str(45 * 1024 * 1024)
        self.environment["FIXTURE_DOCKER_AVAIL_KB"] = str(12 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no amount of pruning reaches that", result.stderr)
        self.assertIn(str(docker_root), result.stderr)
        self.assertEqual(self.last_run()["demand_dropped"], 1)
        # And it is still the runner cache's turn first: the runner volume is
        # healthy, so nothing there is touched.
        self.assertEqual(self.systemctl_calls(), [])
        # Reporting the dropped demand must not change the decision. 12 G free
        # clears the 10 G reserve the drop leaves behind, so nothing is pruned
        # — saying so out loud is the entire change on this path.
        self.assertFalse(self.last_run()["docker_pruned"])
        self.assertIn("Docker within reserve", result.stdout)
        docker_log = Path(self.environment["FIXTURE_DOCKER_LOG"])
        self.assertFalse(docker_log.exists(), docker_log.read_text() if docker_log.exists() else "")

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

    def test_the_bootstrap_can_actually_fetch(self):
        """A string in a file is not a fetch, and that distinction cost a day.

        `noirr/plurx` is private: every raw URL answers 404 to an anonymous
        request and 200 with an `Authorization: token` header. The bootstrap
        used a bare `curl`, so the one-command install documented at the top of
        that file could never have worked on any host. It was published, handed
        to an operator, and failed on first use with `curl: (22) ... 404` and
        `bash: /dev/fd/63: Bad file descriptor`, which names neither the private
        repository nor the missing credential.

        The check that was supposed to catch this asserted the URL *string*
        appeared in the file — which it did, correctly, the whole time. This one
        runs the bootstrap against a fake forge that refuses unauthenticated
        requests, so it fails if the header is ever dropped again.
        """
        fixture = Path(self._directory.name) / "bootstrap"
        forge = fixture / "forge/noirr/plurx/raw/branch/main/deploy/runner-janitor"
        forge.mkdir(parents=True)
        for name in (
            "plurx-ci-janitor",
            "plurx-ci-janitor.service",
            "plurx-ci-janitor.timer",
        ):
            (forge / name).write_text(f"# {name}\n", encoding="utf-8")
        # The installer the bootstrap execs, replaced by something that only
        # records that it ran with the four files beside it.
        (forge / "install").write_text(
            '#!/bin/sh\nls "$(dirname -- "$0")" | sort > "$FIXTURE_INSTALLED"\n',
            encoding="utf-8",
        )

        bin_dir = fixture / "bin"
        bin_dir.mkdir()
        # A forge that answers 404 unless the token header is present, which is
        # the only property of the real one this test depends on.
        (bin_dir / "curl").write_text(
            "#!/bin/sh\n"
            "auth=no; url=; out=; fail=no\n"
            "while [ $# -gt 0 ]; do\n"
            '  case "$1" in\n'
            '    -H) case "$2" in "Authorization: token "?*) auth=yes ;; esac; shift 2 ;;\n'
            '    -o) out=$2; shift 2 ;;\n'
            '    -*f*) fail=yes; shift ;;\n'
            '    -*) shift ;;\n'
            '    *) url=$1; shift ;;\n'
            "  esac\n"
            "done\n"
            'if [ "$auth" != yes ]; then\n'
            '  echo "curl: (22) The requested URL returned error: 404" >&2\n'
            "  exit 22\n"
            "fi\n"
            # A missing file models the other half of the real failure: without
            # `--fail`, curl writes the 404 body to the destination and exits
            # 0, so `install` would run an HTML error page as a shell script.
            'src=${url#file://}\n'
            'if [ ! -f "$src" ]; then\n'
            '  if [ "$fail" = yes ]; then\n'
            '    echo "curl: (22) The requested URL returned error: 404" >&2\n'
            "    exit 22\n"
            "  fi\n"
            '  printf \'404 page not found\\n\' > "$out"\n'
            "  exit 0\n"
            "fi\n"
            'cp "$src" "$out"\n',
            encoding="utf-8",
        )
        (bin_dir / "curl").chmod(0o755)
        # `id -u` must answer 0 without this test running as root.
        (bin_dir / "id").write_text("#!/bin/sh\necho 0\n", encoding="utf-8")
        (bin_dir / "id").chmod(0o755)

        installed = fixture / "installed.txt"
        environment = os.environ.copy()
        environment.update(
            {
                "PATH": f"{bin_dir}{os.pathsep}{environment['PATH']}",
                "PLURX_FORGE": f"file://{fixture / 'forge'}",
                "FIXTURE_INSTALLED": str(installed),
                # The bootstrap's own `mktemp -d` lands here rather than in
                # whatever the ambient TMPDIR is, so the test is hermetic and
                # does not fail on a host whose /tmp is full.
                "TMPDIR": str(fixture),
            }
        )
        bootstrap_path = str(ROOT / "deploy/runner-janitor/bootstrap")

        # No token: refused before anything is fetched, and it says why.
        without = subprocess.run(
            ["bash", bootstrap_path], env=environment, capture_output=True, text=True
        )
        self.assertEqual(without.returncode, 2, without.stdout)
        self.assertIn("PLURX_TOKEN is required", without.stderr)
        self.assertIn("private", without.stderr)
        self.assertFalse(installed.exists())

        # With one: all four files land and the installer runs beside them.
        environment["PLURX_TOKEN"] = "fixture-token"
        with_token = subprocess.run(
            ["bash", bootstrap_path], env=environment, capture_output=True, text=True
        )
        self.assertEqual(with_token.returncode, 0, with_token.stderr)
        self.assertEqual(
            installed.read_text(encoding="utf-8").split(),
            [
                "install",
                "plurx-ci-janitor",
                "plurx-ci-janitor.service",
                "plurx-ci-janitor.timer",
            ],
        )

        # And the other half of the same failure: a file that is not there.
        # Without `--fail`, curl writes the 404 body to the destination and
        # exits 0, so the bootstrap would hand `install` an HTML error page and
        # run it as a shell script. It must abort instead.
        installed.unlink()
        (forge / "plurx-ci-janitor.timer").unlink()
        missing = subprocess.run(
            ["bash", bootstrap_path], env=environment, capture_output=True, text=True
        )
        self.assertEqual(missing.returncode, 1, missing.stdout)
        self.assertIn("could not fetch plurx-ci-janitor.timer", missing.stderr)
        self.assertFalse(installed.exists())


class MacosJanitorContractCase(unittest.TestCase):
    """The Apple runner has no systemd, and launchd will not wait for a job.

    `systemctl stop` drains -- the Linux installer raises TimeoutStopSec to
    thirty minutes so it can -- and `launchctl bootout` does not: SIGTERM, then
    SIGKILL about twenty seconds later, which on this runner would be a killed
    Xcode build. So on macOS the idle check is the entire safety mechanism, and
    these tests are mostly about it.
    """

    SCRIPT = ROOT / "deploy/runner-janitor/macos/plurx-ci-janitor"
    LABEL = "org.forgejo.actions.runner.plurx.gha-mba-apple-01"

    def setUp(self):
        subprocess.run(["bash", "-n", str(self.SCRIPT)], check=True)
        self._directory = tempfile.TemporaryDirectory()
        self.addCleanup(self._directory.cleanup)
        fixture = Path(self._directory.name)

        self.runner_root = fixture / "Users/githubrunner/forgejo-runner"
        # A `_work` at all is what reaches `daemon_is_idle`'s quiet window --
        # the macOS janitor's stricter half, which nothing here executed until
        # this directory existed.
        self.work = self.runner_root / "_work/plurx"
        self.work.mkdir(parents=True)
        (self.work / "checkout").write_bytes(b"an Xcode build's tree")
        self.cache = self.runner_root / "cache"
        (self.cache / "cache/0a").mkdir(parents=True)
        (self.cache / "bolt.db").write_bytes(b"index")
        self.config = self.runner_root / "config.yml"
        self.config.write_text(
            f"runner:\n  capacity: 1\ncache:\n  enabled: true\n  dir: {self.cache}\n",
            encoding="utf-8",
        )

        self.daemons = fixture / "LaunchDaemons"
        self.daemons.mkdir()
        self.plist = self.daemons / f"{self.LABEL}.plist"
        self.plist.write_text("<plist/>", encoding="utf-8")

        self.bin = fixture / "bin"
        self.bin.mkdir()
        self.log = fixture / "launchctl.log"
        tools = {
            "plutil": (
                "#!/bin/sh\nprintf '%s\\n' \"$FIXTURE_PLIST_JSON\"\n"
            ),
            "launchctl": (
                "#!/bin/sh\n"
                'if [ "$1" = print ]; then printf \'\\tpid = %s\\n\' '
                '"$FIXTURE_PID"; exit 0; fi\n'
                'printf \'%s %s\\n\' "$1" "$2" >> "$FIXTURE_LOG"\n'
            ),
            "pgrep": "#!/bin/sh\nexit $FIXTURE_PGREP_STATUS\n",
            "df": (
                "#!/bin/sh\n"
                "printf '%s\\n' "
                "'Filesystem 1024-blocks Used Available Capacity Mounted'\n"
                "printf 'fixture %s 0 %s 50%% /fixture\\n' "
                '"$FIXTURE_FS_KB" "$FIXTURE_AVAIL_KB"\n'
            ),
            "du": (
                "#!/bin/sh\nshift $(($# - 1))\n"
                'printf \'%s\\t%s\\n\' "$FIXTURE_USED_KB" "$1"\n'
            ),
            # BSD `stat -f '%m'` prints an epoch second, which is correct for
            # the platform that script runs on. GNU `stat` spells that `-c` and
            # reads `-f` as --file-system, so on a Linux runner the real binary
            # answers a "File: ..." block that bash then evaluates
            # arithmetically. Faked so the quiet window can be tested at all.
            "stat": "#!/bin/sh\nprintf '%s\\n' \"$FIXTURE_WORK_MTIME\"\n",
        }
        for name, body in tools.items():
            path = self.bin / name
            path.write_text(body, encoding="utf-8")
            path.chmod(0o755)

        self.state = fixture / "state"
        self.environment = os.environ.copy()
        self.environment.update(
            {
                "PATH": f"{self.bin}{os.pathsep}{self.environment['PATH']}",
                "PLURX_JANITOR_STATE_DIR": str(self.state),
                "PLURX_JANITOR_DAEMON_DIR": str(self.daemons),
                # plutil escapes every forward slash, and the real plist
                # on gha-mba-apple-01 does exactly that. A fixture that did not
                # would pass a script that reads a path naming nothing.
                "FIXTURE_PLIST_JSON": self.plist_json(),
                "FIXTURE_LOG": str(self.log),
                "FIXTURE_PID": "4242",
                "FIXTURE_PGREP_STATUS": "1",
                "FIXTURE_FS_KB": str(460 * 1024 * 1024),
                "FIXTURE_AVAIL_KB": str(119 * 1024 * 1024),
                "FIXTURE_USED_KB": str(4 * 1024 * 1024),
                # Long past the 120-second quiet window: idle by default, so
                # each test says for itself when it wants a busy runner.
                "FIXTURE_WORK_MTIME": "1",
            }
        )

    def plist_json(self):
        escaped = str(self.config).replace("/", "\\/")
        program = "\\/usr\\/local\\/bin\\/forgejo-runner-13.1.0"
        return (
            f'{{"Label":"{self.LABEL}","ProgramArguments":'
            f'["{program}","daemon","-c","{escaped}"]}}'
        )

    def run_janitor(self, *arguments):
        return subprocess.run(
            [str(self.SCRIPT), *arguments],
            env=self.environment,
            capture_output=True,
            text=True,
        )

    def launchctl_calls(self):
        if not self.log.exists():
            return []
        return self.log.read_text(encoding="utf-8").split()

    def last_run(self):
        return json.loads((self.state / "last-run.json").read_text(encoding="utf-8"))

    def test_a_cache_inside_its_budget_is_left_alone(self):
        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("within budget", result.stdout)
        self.assertEqual(self.launchctl_calls(), [])
        self.assertTrue((self.cache / "bolt.db").is_file())
        self.assertEqual(self.last_run()["reset"], 0)

    def test_an_over_budget_cache_is_reset_and_the_daemon_comes_back(self):
        self.environment["FIXTURE_USED_KB"] = str(41 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.launchctl_calls(),
            ["bootout", f"system/{self.LABEL}", "bootstrap", "system"],
        )
        self.assertFalse(self.cache.exists())
        self.assertEqual(self.last_run()["reclaimed_gb"], 41)

    def test_a_daemon_with_child_processes_keeps_its_cache(self):
        # The whole safety mechanism: launchd will not wait for the job, so a
        # daemon with children must never be unloaded.
        self.environment["FIXTURE_USED_KB"] = str(41 * 1024 * 1024)
        self.environment["FIXTURE_PGREP_STATUS"] = "0"

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("running a job", result.stdout)
        self.assertEqual(self.launchctl_calls(), [])
        self.assertTrue((self.cache / "bolt.db").is_file())
        self.assertEqual(self.last_run()["over_budget"], 1)
        self.assertEqual(self.last_run()["reset"], 0)

    def test_a_recently_touched_work_tree_keeps_the_cache(self):
        """The quiet window, which nothing in this suite reached before.

        `pgrep -P` sees a job only once the daemon has spawned a child. Between
        being handed a job and that first child appearing there is a gap, and
        on this platform a wrong answer in that gap is a SIGKILLed Xcode build
        rather than a drained one. So the daemon must ALSO have left `_work`
        alone for QUIET_SECONDS, and a fresh mtime is a refusal.
        """
        self.environment["FIXTURE_USED_KB"] = str(41 * 1024 * 1024)
        self.environment["FIXTURE_WORK_MTIME"] = str(int(time.time()))

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("running a job", result.stdout)
        self.assertEqual(self.launchctl_calls(), [])
        self.assertTrue((self.cache / "bolt.db").is_file())
        self.assertEqual(self.last_run()["over_budget"], 1)
        self.assertEqual(self.last_run()["reset"], 0)

    def test_the_macos_janitor_leaves_the_work_tree_alone(self):
        """Both triggers at once, and the checkouts survive both.

        `_work`'s mtime is this janitor's quiet signal, so a janitor that
        emptied it would be destroying the evidence its own next pass reasons
        from -- and `launchctl bootout` has no graceful drain to fall back on
        if that reasoning is ever wrong.
        """
        self.environment["FIXTURE_USED_KB"] = str(41 * 1024 * 1024)
        self.environment["FIXTURE_AVAIL_KB"] = str(9 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.work / "checkout").is_file())

    def test_a_daemon_with_no_pid_keeps_its_cache(self):
        self.environment["FIXTURE_USED_KB"] = str(41 * 1024 * 1024)
        self.environment["FIXTURE_PID"] = ""

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.launchctl_calls(), [])
        self.assertTrue((self.cache / "bolt.db").is_file())

    def test_a_cache_dir_the_plist_points_elsewhere_is_refused(self):
        elsewhere = Path(self._directory.name) / "Users/githubrunner/Library"
        elsewhere.mkdir(parents=True)
        (elsewhere / "bolt.db").write_bytes(b"not a runner cache")
        self.config.write_text(
            f"cache:\n  enabled: true\n  dir: {elsewhere}\n", encoding="utf-8"
        )
        self.environment["FIXTURE_USED_KB"] = str(41 * 1024 * 1024)

        result = self.run_janitor()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.launchctl_calls(), [])
        self.assertTrue((elsewhere / "bolt.db").is_file())
        self.assertEqual(self.last_run()["instances"], 0)

    def test_dry_run_decides_out_loud_and_deletes_nothing(self):
        self.environment["FIXTURE_USED_KB"] = str(41 * 1024 * 1024)

        result = self.run_janitor("--dry-run")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("would reset", result.stdout)
        self.assertEqual(self.launchctl_calls(), [])
        self.assertTrue((self.cache / "bolt.db").is_file())

    def test_the_daemon_plist_and_installer_agree_with_the_script(self):
        plist = (
            ROOT / "deploy/runner-janitor/macos/tv.plurx.ci-janitor.plist"
        ).read_text(encoding="utf-8")
        installer = (ROOT / "deploy/runner-janitor/macos/install").read_text(
            encoding="utf-8"
        )

        self.assertIn("<string>/usr/local/bin/plurx-ci-janitor</string>", plist)
        self.assertIn("<integer>3600</integer>", plist)
        self.assertIn("bootstrap system /Library/LaunchDaemons", installer)
        self.assertIn("--dry-run", installer)
        # The Linux installer must not be run here and vice versa: the Linux one
        # writes systemd drop-ins that do not exist on a Mac.
        self.assertIn('[ "$(uname -s)" = Darwin ]', installer)


if __name__ == "__main__":
    unittest.main()
