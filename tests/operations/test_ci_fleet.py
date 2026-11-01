"""ci-fleet: the classifier, the forge client's paging, the agent, the collector.

The classifier runs on Forgejo responses captured on 2026-11-01 (host names
rewritten to the roster's neutral names), against this checkout's workflows
and roster, so the cases are the fleet's real ones: four Apple jobs that ask
for a label no runner carries, a pool whose only runners are offline, stale
registrations, and a version skew.
"""

from __future__ import annotations

import ast
from datetime import datetime, timezone
import io
import json
from pathlib import Path
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/ci-fleet"))

from cifleet import agent as agent_module, explain, fleet, metrics  # noqa: E402
from cifleet.collector import Collector, make_handler  # noqa: E402
from cifleet.forgejo import Forgejo, parse_runner_line  # noqa: E402
from cifleet.store import Store  # noqa: E402
from validation import runner_fleet  # noqa: E402

FIXTURE = json.loads((ROOT / "tests/operations/fixtures/ci_fleet/forgejo-2026-11-01.json").read_text())
NOW = int(datetime(2026, 11, 1, 17, 40, tzinfo=timezone.utc).timestamp())
APPLE = ["self-hosted", "macOS", "ARM64", "lab", "apple", "xcode-27.1"]


def workflows():
    return runner_fleet.parse_workflows({p.name: p.read_text() for p in (ROOT / ".github/workflows").glob("*.yml")})


def roster():
    return runner_fleet.load_roster(ROOT / "validation/runner-fleet.toml")


def inputs(**overrides) -> fleet.Inputs:
    values = dict(
        now=NOW,
        runners=json.loads(json.dumps(FIXTURE["runners"])),
        queue=json.loads(json.dumps(FIXTURE["queue"])),
        runs={int(k): v["run"] for k, v in FIXTURE["runs"].items()},
        run_jobs={int(k): v["jobs"] for k, v in FIXTURE["runs"].items()},
        workflows=workflows(),
        roster=roster(),
    )
    values.update(overrides)
    return fleet.Inputs(**values)


class WorkflowParsingCase(unittest.TestCase):
    def test_fast_lane_jobs_carry_key_name_needs_and_condition(self):
        jobs = {job.key: job for wf in workflows() if wf.file == "main-fast-lane.yml" for job in wf.jobs}
        apple = jobs["apple_compile"]
        self.assertEqual(apple.name, "Apple compile and native TV navigation")
        self.assertEqual(apple.needs, ("scope", "preflight"))
        self.assertTrue(apple.conditional)
        self.assertFalse(jobs["paperwork"].conditional)
        self.assertIn("apple_compile", jobs["promotion_gate"].needs)

    def test_scheduled_pool_with_no_online_runner_is_requested(self):
        pools = runner_fleet.requested_pools(workflows())
        wanted = frozenset({"self-hosted", "Linux", "X64", "lab", "general", "ffmpeg-6"})
        self.assertIn(wanted, pools)
        self.assertIn("release-readiness.yml", {job.workflow for job in pools[wanted]})

    def test_next_cron_is_monday_six_utc(self):
        after = datetime(2026, 11, 1, 17, 40, tzinfo=timezone.utc)
        self.assertEqual(runner_fleet.next_cron("0 6 * * 1", after), datetime(2026, 11, 2, 6, 0, tzinfo=timezone.utc))
        self.assertEqual(runner_fleet.next_cron("*/15 * * * *", after), datetime(2026, 11, 1, 17, 45, tzinfo=timezone.utc))
        self.assertIsNone(runner_fleet.next_cron("bad", after))


class LiveFixtureCase(unittest.TestCase):
    """The fleet as it stood on 2026-11-01, classified."""

    @classmethod
    def setUpClass(cls):
        cls.snapshot = fleet.build(inputs())

    def test_every_apple_job_is_unsatisfiable_for_xcode_27_1(self):
        waiting = self.snapshot["jobs"]["waiting"]
        self.assertEqual(len(waiting), 4)
        for job in waiting:
            self.assertEqual(job["class"], "unsatisfiable")
            self.assertEqual(job["missing_labels"], ["xcode-27.1"])
            closest = {item["runner"]: item["missing"] for item in job["closest"]}
            self.assertEqual(closest["gha-macb-apple-01"], ["xcode-27.1"])

    def test_superseded_and_dead_on_pickup_flags(self):
        flags = {job["run_index"]: set(job["flags"]) for job in self.snapshot["jobs"]["waiting"]}
        self.assertEqual(flags[6263], {"dead_on_pickup", "superseded"})
        self.assertEqual(flags[6264], {"dead_on_pickup", "superseded"})
        self.assertEqual(flags[6266], set())
        self.assertEqual(flags[6272], {"dead_on_pickup"})

    def test_blocked_jobs_come_from_the_runs_not_the_queue(self):
        blocked = self.snapshot["jobs"]["blocked"]
        self.assertEqual({job["name"] for job in blocked}, {"Main promotion gate"})
        self.assertEqual(len(blocked), 4)
        self.assertTrue(all(job["class"] == "blocked" for job in blocked))

    def test_pools_show_the_dead_ffmpeg_pool_and_its_next_run(self):
        pools = {pool["name"]: pool for pool in self.snapshot["pools"]}
        dead = pools["ffmpeg-6 · general"]
        self.assertEqual((dead["registered"], dead["online"]), (3, 0))
        self.assertEqual(dead["next_schedule"], int(datetime(2026, 11, 2, 6, tzinfo=timezone.utc).timestamp()))
        self.assertEqual((pools["general · high-cpu"]["registered"], pools["general · high-cpu"]["online"]), (8, 5))
        self.assertFalse(pools["apple · xcode-27.1"]["requested"])
        self.assertEqual(pools["apple · xcode-27.1"]["waiting"], 4)
        unused = {pool["name"] for pool in self.snapshot["pools"] if not pool["requested"] and not pool["waiting"]}
        self.assertTrue({"ci-coverage", "primary", "async-proof-owned"} <= unused)
        self.assertFalse({"incus", "baremetal", "ubuntu-24.04"} & unused)

    def test_drift_findings(self):
        kinds = {finding["kind"]: finding for finding in self.snapshot["drift"]}
        self.assertEqual(self.snapshot["drift"][0]["severity"], "blocking")
        self.assertEqual(set(kinds["stale"]["runners"]),
                         {"gha-media1-android-01", "gha-lab6-general-03", "gha-lab6-general-04"})
        self.assertEqual(set(kinds["expected_online"]["runners"]) >= {"gha-lab6-general-01", "gha-lab4-general-01",
                                                                     "gha-lab4-general-02"}, True)
        self.assertEqual(kinds["versions"]["versions"], {"v13.2.0": ["gha-macb-apple-01"]})
        mismatch = {item["runner"]: item for item in kinds["labels"]["runners"]}
        self.assertEqual(mismatch["gha-maca-linux-arm-01"]["roster_only"], ["backup", "ci-arm64", "laptop-vm"])

    def test_counts_agree_with_the_lists(self):
        counts, jobs = self.snapshot["counts"], self.snapshot["jobs"]
        self.assertEqual((counts["registered"], counts["online"], counts["offline"]), (25, 17, 8))
        self.assertEqual(counts["waiting"], len(jobs["waiting"]))
        self.assertEqual(counts["blocked"], len(jobs["blocked"]))
        self.assertEqual(counts["stale"], 3)
        self.assertEqual(counts["drift"], len(self.snapshot["drift"]))
        self.assertEqual(self.snapshot["alerts"][0]["kind"], "unsatisfiable")

    def test_real_host_names_are_reported_as_not_neutral(self):
        live = json.loads(json.dumps(FIXTURE["runners"]))
        for runner in live:
            runner["name"] = runner["name"].replace("gha-lab5-", "gha-rog-")
        snapshot = fleet.build(inputs(runners=live, hosts={"rog": {"alias": "lab5"}}))
        kinds = {finding["kind"]: finding for finding in snapshot["drift"]}
        self.assertEqual(kinds["names"]["hosts"], ["rog"])
        self.assertNotIn("stale", {f["kind"] for f in snapshot["drift"] if "gha-rog-general-01" in f.get("runners", [])})

    def test_why_text_names_the_missing_label(self):
        text = explain.why(self.snapshot, pr="1340")
        self.assertIn("UNSATISFIABLE", text)
        self.assertIn("lacks xcode-27.1", text)


def single_job(runs_on, *, status="waiting", needs=None):
    run = {"id": 1, "index_in_repo": 1, "status": "running", "prettyref": "#1", "workflow_id": "x.yml",
           "started": "2026-11-01T17:00:00Z"}
    job = {"id": 10, "run_id": 1, "name": "build", "needs": needs, "runs_on": runs_on, "task_id": 0, "status": status}
    return run, job


def runner(name, labels, status="idle", uuid=None):
    return {"id": hash(name) % 1000, "name": name, "uuid": uuid or name, "status": status, "version": "v1",
            "labels": ["self-hosted", "Linux", "X64", "lab", *labels]}


class ClassifierCase(unittest.TestCase):
    def classify(self, runners, *, status="waiting", needs=None, siblings=(), **extra):
        run, job = single_job(["self-hosted", "Linux", "X64", "lab", "general"], status=status, needs=needs)
        queue = [job] if status != "blocked" else []
        return fleet.build(fleet.Inputs(now=NOW, runners=runners, queue=queue + list(extra.pop("queue", [])),
                                        runs={1: run}, run_jobs={1: [job, *siblings]}, workflows=[], roster=None,
                                        **extra))

    def test_offline_only(self):
        snapshot = self.classify([runner("gha-lab4-general-01", ["general"], "offline")])
        job = snapshot["jobs"]["waiting"][0]
        self.assertEqual(job["class"], "offline_only")
        self.assertEqual(job["offline"][0]["runner"], "gha-lab4-general-01")

    def test_a_paused_runner_counts_as_unavailable(self):
        agents = {"lab5": {"agent": "lab5", "host": "lab5", "runners": [{"name": "gha-lab5-general-01", "paused": True}]}}
        snapshot = self.classify([runner("gha-lab5-general-01", ["general"])], agents=agents)
        self.assertEqual(snapshot["jobs"]["waiting"][0]["class"], "offline_only")

    def test_busy_with_an_eta_from_p50(self):
        busy_job = {"id": 99, "run_id": 1, "name": "fast Rust gate", "runs_on": [], "task_id": 500, "status": "running"}
        snapshot = self.classify([runner("gha-media1-general-02", ["general"], "active")], queue=[busy_job],
                                 placements={500: {"runner": "gha-media1-general-02", "started": NOW - 240}},
                                 durations={"fast Rust gate": {"p50": 850}})
        job = snapshot["jobs"]["waiting"][0]
        self.assertEqual(job["class"], "busy")
        self.assertEqual(job["eta"], NOW - 240 + 850)
        self.assertEqual(snapshot["jobs"]["running"][0]["runner"], "gha-media1-general-02")

    def test_a_runner_idle_under_a_minute_is_just_queued(self):
        snapshot = self.classify([runner("gha-lab5-general-05", ["general"])],
                                 idle_since={"gha-lab5-general-05": NOW - 30})
        self.assertEqual(snapshot["jobs"]["waiting"][0]["class"], "queued")

    def test_not_picked_up(self):
        snapshot = self.classify([runner("gha-lab5-general-05", ["general"])],
                                 idle_since={"gha-lab5-general-05": NOW - 120})
        # The run started 40 minutes before NOW, so the job has waited long enough.
        self.assertEqual(snapshot["jobs"]["waiting"][0]["class"], "not_picked_up")

    def test_held_when_needs_have_finished(self):
        workflow = runner_fleet.parse_workflow("x.yml", "jobs:\n  a:\n    name: first\n  b:\n    name: build\n    needs: a\n")
        done = {"id": 11, "run_id": 1, "name": "first", "runs_on": [], "task_id": 7, "status": "success"}
        run, job = single_job([], status="blocked", needs=["a"])
        snapshot = fleet.build(fleet.Inputs(now=NOW, runners=[], queue=[], runs={1: run}, run_jobs={1: [job, done]},
                                            workflows=[workflow], roster=None))
        self.assertEqual(snapshot["jobs"]["blocked"][0]["class"], "held")
        done["status"] = "running"
        snapshot = fleet.build(fleet.Inputs(now=NOW, runners=[], queue=[], runs={1: run}, run_jobs={1: [job, done]},
                                            workflows=[workflow], roster=None))
        self.assertEqual(snapshot["jobs"]["blocked"][0]["class"], "blocked")

    def test_agent_reports_for_one_runner_merge(self):
        agents = {
            "lab6": {"agent": "lab6", "host": "lab6", "runners": [{"name": "r", "controls": ["drain"], "paused": False}]},
            "lab6-g1": {"agent": "lab6-g1", "host": "lab6", "role": "guest",
                        "runners": [{"name": "r", "controls": [], "usage": {"cpu": 2.5}}]},
        }
        merged = fleet._agent_runners(agents)["r"]
        self.assertEqual(merged["controls"], ["drain"])
        self.assertEqual(merged["usage"], {"cpu": 2.5})
        self.assertEqual(merged["agent"], "lab6")


class FakeResponse(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


class FakeForge:
    """An opener answering from the fixture; records every URL it was asked for."""

    def __init__(self):
        self.urls = []
        self.cancelled = []

    def __call__(self, request, timeout=None):
        url = request.full_url
        self.urls.append((request.get_method(), url))
        path = url.split("/api/v1", 1)[1].split("?", 1)[0]
        repo = "/repos/noirr/plurx"
        if path == "/version":
            return FakeResponse(b'{"version":"16.0.3"}')
        if path == "/user":
            return FakeResponse(b'{"login":"operator"}')
        if path == repo:
            return FakeResponse(b'{"permissions":{"push":true}}')
        if path.endswith("/actions/runners"):
            page = int(url.split("page=", 1)[1].split("&", 1)[0])
            return FakeResponse(json.dumps(FIXTURE["runners"] if page == 1 else []).encode())
        if path.endswith("/actions/runners/jobs"):
            return FakeResponse(json.dumps(FIXTURE["queue"]).encode())
        if path.endswith("/actions/runs"):
            return FakeResponse(b'{"workflow_runs":[]}')
        if path.endswith("/actions/tasks"):
            return FakeResponse(b'{"workflow_runs":[]}')
        if "/actions/runs/" in path and path.endswith("/cancel"):
            self.cancelled.append(path)
            return FakeResponse(b"")
        if "/actions/runs/" in path and path.endswith("/jobs"):
            run_id = path.split("/")[-2]
            return FakeResponse(json.dumps(FIXTURE["runs"][run_id]["jobs"]).encode())
        if "/actions/runs/" in path:
            return FakeResponse(json.dumps(FIXTURE["runs"][path.split("/")[-1]]["run"]).encode())
        if "/actions/jobs/" in path and path.endswith("/logs"):
            body = b"2026-11-01T14:52:47.70Z Runner 76841309-661d-47d3-afa7-d45f7144efe7 (version:v13.1.0) " \
                   b"received task 25024 of job preflight\n" + b"x" * 100_000
            return FakeResponse(body)
        if path.endswith("/branches/main"):
            return FakeResponse(b'{"commit":{"id":"abc"}}')
        if path.endswith("/contents/.github/workflows"):
            files = [{"type": "file", "path": f".github/workflows/{p.name}"} for p in (ROOT / ".github/workflows").glob("*.yml")]
            return FakeResponse(json.dumps(files).encode())
        if "/raw/" in path:
            relative = urllib.request.unquote(path.split("/raw/", 1)[1])
            return FakeResponse((ROOT / relative).read_bytes())
        raise urllib.error.HTTPError(url, 404, "not found", {}, io.BytesIO(b"{}"))


class ForgejoClientCase(unittest.TestCase):
    def test_list_calls_without_a_page_are_refused(self):
        forge = Forgejo("http://forge", "noirr/plurx", "t", opener=FakeForge())
        with self.assertRaises(ValueError):
            forge.get(forge.repo_path("/actions/tasks"), limit=20)
        with self.assertRaises(ValueError):
            forge.get(forge.repo_path("/actions/runs"))

    def test_tasks_and_runs_always_send_page(self):
        opener = FakeForge()
        forge = Forgejo("http://forge", "noirr/plurx", "t", opener=opener)
        forge.tasks(1, status="running")
        forge.runs("waiting", pages=1)
        forge.runners()
        for _, url in opener.urls:
            self.assertIn("page=", url)

    def test_job_runner_reads_the_first_line_only(self):
        class Head(FakeForge):
            def __call__(self, request, timeout=None):
                response = super().__call__(request, timeout)
                original = response.read
                self.read_sizes = []

                def read(size=-1):
                    self.read_sizes.append(size)
                    return original(size)
                response.read = read
                return response
        opener = Head()
        forge = Forgejo("http://forge", "noirr/plurx", "t", opener=opener)
        self.assertEqual(forge.job_runner(61011), ("76841309-661d-47d3-afa7-d45f7144efe7", "v13.1.0", 25024))
        self.assertEqual(opener.read_sizes, [512])
        self.assertIsNone(parse_runner_line("2026 job has not been executed yet"))


class StoreCase(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.store = Store(str(Path(self.folder.name) / "db.sqlite"))
        self.addCleanup(self.store.close)

    def test_upsert_never_erases_known_values(self):
        self.store.upsert_task({"task_id": 1, "runner": "gha-lab5-general-06", "started": 10})
        self.store.upsert_task({"task_id": 1, "runner": None, "stopped": 20, "status": "success"})
        task = self.store.task(1)
        self.assertEqual((task["runner"], task["started"], task["stopped"]), ("gha-lab5-general-06", 10, 20))

    def test_durations_ignore_skipped_and_failed(self):
        for task_id, (seconds, status) in enumerate([(840, "success"), (860, "success"), (5, "skipped"), (30, "failure")]):
            self.store.upsert_task({"task_id": task_id, "name": "fast Rust gate", "status": status,
                                    "started": NOW - 2000, "stopped": NOW - 2000 + seconds})
        stats = self.store.durations(NOW - 86400)["fast Rust gate"]
        self.assertEqual((stats["n"], stats["p50"]), (2, 850))

    def test_maintain_downsamples_and_caps_every_series(self):
        from cifleet import store as store_module
        for ts in range(NOW - 2 * 86400, NOW - 2 * 86400 + 600, 5):
            self.store.record_usage("task", "1", ts, 1.0, 100)
        for ts in range(NOW - 600, NOW, 5):
            self.store.record_usage("task", "1", ts, 2.0, 200)
        self.store.maintain(NOW)
        old = [row for row in self.store.series("task", "1") if row["ts"] < NOW - 86400]
        self.assertEqual(len(old), 10)  # 600 s of 5 s samples become 10 minute rows
        cap = store_module.ROWS_PER_SERIES
        try:
            store_module.ROWS_PER_SERIES = 50
            self.store.maintain(NOW)
            self.assertEqual(len(self.store.series("task", "1")), 50)
        finally:
            store_module.ROWS_PER_SERIES = cap


class MetricsCase(unittest.TestCase):
    def test_bounded_labels_and_one_header_per_family(self):
        text = metrics.render(fleet.build(inputs()), poll_seconds=0.4, api_calls=12, forge_up=True)
        for forbidden in ("pr=", "run=", "task="):
            self.assertNotIn(forbidden, text)
        helps = [line for line in text.splitlines() if line.startswith("# HELP")]
        self.assertEqual(len(helps), len(set(helps)))
        self.assertIn('ci_fleet_unsatisfiable_jobs 4', text)
        self.assertIn('ci_fleet_pool_runners{pool="ffmpeg-6 · general",state="online"} 0', text)


class AgentCase(unittest.TestCase):
    def test_agent_stays_python_3_9_compatible(self):
        source = (ROOT / "tools/ci-fleet/cifleet/agent.py").read_text()
        ast.parse(source, feature_version=(3, 9))

    def test_parsers(self):
        self.assertEqual(agent_module.parse_proc_stat("cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 1 2 3"), (150, 1000))
        self.assertEqual(agent_module.parse_meminfo("MemTotal: 2 kB\nMemAvailable: 1 kB")["MemTotal"], 2048)
        self.assertEqual(agent_module.parse_cpu_max("300000 100000"), 3.0)
        self.assertIsNone(agent_module.parse_cpu_max("max 100000"))
        size, pages = agent_module.parse_vm_stat("Mach Virtual Memory Statistics: (page size of 16384 bytes)\n"
                                                 "Pages free:                               100.\n")
        self.assertEqual((size, pages["Pages free"]), (16384, 100))
        self.assertEqual(agent_module.parse_swapusage("total = 2048.00M  used = 512.00M  free = 1536.00M"),
                         (2048 << 20, 512 << 20))
        self.assertEqual(agent_module.task_id_of(["FORGEJO-ACTIONS-TASK-25052_WORKFLOW-x_JOB-y"]), 25052)
        self.assertIsNone(agent_module.task_id_of(["plurxd"]))

    def test_cgroup_reader_measures_cores_caps_and_ooms(self):
        with tempfile.TemporaryDirectory() as folder:
            group = Path(folder)
            (group / "cpu.max").write_text("300000 100000\n")
            (group / "memory.current").write_text("6000\n")
            (group / "memory.max").write_text("7000\n")
            (group / "memory.events").write_text("oom 0\noom_kill 2\n")
            clock = iter([0.0, 2.0])
            reader = agent_module.CgroupReader(group, clock=lambda: next(clock))
            (group / "cpu.stat").write_text("usage_usec 0\nnr_periods 0\nnr_throttled 0\n")
            self.assertIsNone(reader.read(group)["cpu"])
            (group / "cpu.stat").write_text("usage_usec 6000000\nnr_periods 100\nnr_throttled 41\n")
            usage = reader.read(group)
            self.assertEqual((usage["cpu"], usage["cpu_cap"], usage["throttled_pct"]), (3.0, 3.0, 41.0))
            self.assertEqual((usage["rss"], usage["mem_cap"], usage["oom_kills"]), (6000, 7000, 2))

    def agent(self, runners, folder):
        commands = []

        def execute(*args, timeout=20):
            commands.append(args)
            return "receipt\n"
        config = {"agent": "lab6", "incus_project": "github-runners", "runners": runners,
                  "paused_dir": f"{folder}/paused", "watchdog_paused_dir": f"{folder}/watchdog"}
        return agent_module.Agent(config, execute=execute), commands

    def test_incus_drain_never_stops_a_busy_runner(self):
        with tempfile.TemporaryDirectory() as folder:
            runner = {"name": "gha-lab6-general-01", "kind": "incus", "vm": "gha-lab6-general-01",
                      "unit": "forgejo-runner-gha-lab6-general-01.service"}
            agent, commands = self.agent([runner], folder)
            busy = {"value": "4f2a9c\n"}  # a task container is running
            agent.execute = lambda *args, timeout=20: commands.append(args) or (busy["value"] if "sh" in args else "")
            result = agent.perform({"action": "drain", "target": runner["name"], "actor": "op", "reason": "r"})
            marker = Path(folder) / "watchdog" / runner["vm"]
            self.assertTrue(marker.exists(), result)
            stop = ("systemctl", "stop", "--no-block", runner["unit"])
            self.assertFalse([c for c in commands if c[-4:] == stop])
            busy["value"] = ""
            agent.progress_drains()
            self.assertEqual([c for c in commands if c[-4:] == stop][0][:5],
                             ("incus", "exec", "--project", "github-runners", runner["vm"]))
            self.assertEqual(json.loads(marker.read_text())["state"], "stopped")
            self.assertEqual(agent.results[-1]["result"], "idle: runner stopped")
            agent.progress_drains()
            self.assertEqual(len([c for c in commands if c[-4:] == stop]), 1)
            agent.perform({"action": "resume", "target": runner["name"]})
            self.assertFalse(marker.exists())
            self.assertEqual(commands[-1][1:], ("--vm", runner["vm"]))

    def test_standby_drain_is_the_controllers_marker(self):
        with tempfile.TemporaryDirectory() as folder:
            runner = {"name": "gha-macb-apple-01", "kind": "standby", "pause_marker": f"{folder}/paused"}
            agent, commands = self.agent([runner], folder)
            agent.perform({"action": "drain", "target": runner["name"]})
            self.assertTrue(Path(runner["pause_marker"]).exists())
            self.assertEqual(commands, [])

    def test_unmanaged_targets_and_actions_are_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            agent, commands = self.agent([{"name": "r", "kind": "lima", "vm": "v"}], folder)
            self.assertIn("failed: not managed", agent.perform({"action": "drain", "target": "other"})["result"])
            self.assertIn("failed:", agent.perform({"action": "drain", "target": "r"})["result"])
            self.assertEqual(commands, [])


class CollectorCase(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.opener = FakeForge()
        config = {"forge_url": "http://forge", "repo": "noirr/plurx", "token": "t", "agent_token": "agent-secret",
                  "control_networks": ["10.42.0.0/16"]}
        forge = Forgejo("http://forge", "noirr/plurx", "t", opener=self.opener)
        store = Store(str(Path(self.folder.name) / "db.sqlite"))
        self.addCleanup(store.close)
        self.collector = Collector(config, forge=forge, store=store, clock=lambda: NOW)
        self.collector.poll()

    def test_a_poll_builds_the_snapshot_from_the_forge_and_main(self):
        snapshot = self.collector.snapshot
        self.assertTrue(snapshot["forge"]["ok"], snapshot["forge"])
        self.assertEqual(snapshot["counts"]["unsatisfiable"], 4)
        self.assertEqual(len(snapshot["jobs"]["blocked"]), 4)
        for _, url in self.opener.urls:
            if "/actions/tasks" in url or url.split("?")[0].endswith("/actions/runs"):
                self.assertIn("page=", url)

    def test_controls_need_an_allowed_address_and_a_writer_token(self):
        server = self.serve()
        base = f"http://127.0.0.1:{server.server_address[1]}"
        status, _ = self.post(base + "/api/control", {"action": "cancel_run", "target": "6300"}, token="x")
        self.assertEqual(status, 403)  # 127.0.0.1 is not in 10.42.0.0/16
        self.collector.networks.append(__import__("ipaddress").ip_network("127.0.0.0/8"))
        status, _ = self.post(base + "/api/control", {"action": "cancel_run", "target": "6300"})
        self.assertEqual(status, 401)
        status, body = self.post(base + "/api/control", {"action": "cancel_run", "target": "6300", "reason": "superseded"},
                                 token="operator-token")
        self.assertEqual((status, body["ok"]), (200, True))
        self.assertTrue(self.opener.cancelled[0].endswith("/actions/runs/6300/cancel"))
        self.assertEqual(self.collector.store.audit_log(1)[0]["actor"], "operator")

    def test_drain_is_queued_for_the_agent_that_owns_the_runner(self):
        self.collector.accept_report({"agent": "maca", "host": "maca", "runners": [
            {"name": "gha-maca-apple-01", "kind": "launchd", "controls": ["drain"]}]})
        result = self.collector.control("operator", "t", {"action": "drain", "target": "gha-maca-apple-01"})
        self.assertEqual(result, {"ok": True, "result": "queued for maca"})
        reply = self.collector.accept_report({"agent": "maca", "host": "maca", "runners": []})
        self.assertEqual(reply["commands"][0]["action"], "drain")
        refused = self.collector.control("operator", "t", {"action": "drain", "target": "gha-lab5-general-01"})
        self.assertFalse(refused["ok"])

    def test_only_stale_runners_can_be_deleted(self):
        online = self.collector.control("operator", "t", {"action": "delete_runner", "target": "gha-lab5-general-01"})
        self.assertFalse(online["ok"])

    def test_agents_need_the_agent_token(self):
        server = self.serve()
        base = f"http://127.0.0.1:{server.server_address[1]}"
        status, _ = self.post(base + "/api/agent", {"agent": "lab5"}, bearer="wrong")
        self.assertEqual(status, 401)
        status, body = self.post(base + "/api/agent", {"agent": "lab5", "host": "lab5"}, bearer="agent-secret")
        self.assertEqual((status, body["commands"]), (200, []))

    def test_read_endpoints(self):
        server = self.serve()
        base = f"http://127.0.0.1:{server.server_address[1]}"
        with urllib.request.urlopen(base + "/api/why?pr=1263") as response:
            self.assertIn("superseded by run #6272", response.read().decode())
        with urllib.request.urlopen(base + "/metrics") as response:
            self.assertIn("ci_fleet_forgejo_up 1", response.read().decode())

    def serve(self):
        import http.server
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), make_handler(self.collector))
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        return server

    def post(self, url, body, *, token=None, bearer=None):
        request = urllib.request.Request(url, data=json.dumps(body).encode(), method="POST")
        if token:
            request.add_header("Authorization", "token " + token)
        if bearer:
            request.add_header("Authorization", "Bearer " + bearer)
        try:
            with urllib.request.urlopen(request) as response:
                return response.status, json.loads(response.read())
        except urllib.error.HTTPError as error:
            return error.code, json.loads(error.read() or b"{}")


if __name__ == "__main__":
    unittest.main()
