"""ci-fleetd: poll the forge, take agent reports, serve the console and API.

One process, standard library only. A poll thread refreshes the forge state
every POLL_SECONDS and rebuilds the snapshot; an HTTP server answers the
console, the CLI, Prometheus and agents from the last snapshot. Controls go to
the tool that owns each resource: the forge (cancel a run, delete a stale
registration) or the agent on the runner's host (drain, resume, start a VM),
which in turn drives the runner watchdog or the Apple standby controller.
"""

from __future__ import annotations

import argparse
import hashlib
import http.server
import ipaddress
import json
import mimetypes
import os
from pathlib import Path
import secrets
import threading
import time
import traceback
import urllib.parse

from validation.runner_fleet import load_roster, parse_workflows

from . import explain, fleet, metrics
from .forgejo import Forgejo, ForgejoError
from .store import Store

POLL_SECONDS = fleet.POLL_SECONDS
TASK_SECONDS = 30
SOURCE_SECONDS = 600
STATS_SECONDS = 60
MAINTAIN_SECONDS = 3600
BACKFILL_DAYS = 90
BACKFILL_EVERY = 3  # polls between backfill steps
BACKFILL_RUNS = 5  # runs (or placements) per step: <= 0.4 extra forge calls/s
AGENT_STALE = 60
OPERATOR_CACHE = 300
FINISHED = {"success", "failure", "cancelled", "skipped"}
UI = Path(__file__).resolve().parent.parent / "ui"


class TaskLookup:
    """Dict-like view the snapshot builder reads needs' stop times through."""

    def __init__(self, store: Store):
        self.store = store

    def get(self, task_id, default=None):
        if not task_id:
            return default
        return self.store.task(int(task_id)) or default


class Collector:
    def __init__(self, config: dict, *, forge: Forgejo | None = None, store: Store | None = None, clock=time.time):
        self.config = config
        token = config.get("token") or Path(os.path.expandvars(config["token_file"])).read_text().strip()
        self.forge = forge or Forgejo(config["forge_url"], config["repo"], token)
        self.store = store or Store(config.get("db", "/var/lib/ci-fleet/ci-fleet.db"))
        self.clock = clock
        self.agent_token = config.get("agent_token") or (
            Path(os.path.expandvars(config["agent_token_file"])).read_text().strip()
            if config.get("agent_token_file") else "")
        self.networks = [ipaddress.ip_network(net) for net in config.get("control_networks", ["127.0.0.0/8"])]
        self.hosts = config.get("hosts", {})
        self.lock = threading.Lock()
        self.runners: list[dict] = []
        self.queue: list[dict] = []
        self.runs: dict[int, dict] = {}
        self.run_jobs: dict[int, list[dict]] = {}
        self.workflows = []
        self.roster: dict | None = None
        self.source_head = ""
        self.first_seen: dict[int, int] = {}
        self.idle_since: dict[str, int] = {}
        self.agents: dict[str, dict] = {}
        self.commands: dict[str, list[dict]] = {}
        self.operators: dict[str, tuple[float, str, bool]] = {}
        self.durations: dict[str, dict] = {}
        self.pool_stats: dict[str, dict] = {}
        self.snapshot: dict | None = None
        self.forge_ok = False
        self.forge_error = ""
        self.forge_version = ""
        self.poll_seconds = 0.0
        self.passes = 0
        self.last = {"tasks": 0, "sources": 0, "stats": 0, "maintain": 0, "minute": 0}

    # -- polling ---------------------------------------------------------------
    def poll(self) -> None:
        started = self.clock()
        now = int(started)
        try:
            if not self.forge_version:
                self.forge_version = self.forge.version()
            if now - self.last["sources"] >= SOURCE_SECONDS:
                self._refresh_sources()
                self.last["sources"] = now
            runners = self.forge.runners()
            queue = self.forge.queue()
            active = {run["id"]: run for status in ("running", "waiting")
                      for run in self.forge.runs(status, pages=1)}
            for job in queue:
                if job["run_id"] not in active:
                    active[job["run_id"]] = self.runs.get(job["run_id"]) or self.forge.run(job["run_id"])
            run_jobs = {run_id: self.forge.run_jobs(run_id) for run_id in active}
            running_tasks = {task["id"]: task for task in self.forge.tasks(1, status="running")}
            with self.lock:
                self.runners, self.queue = runners, queue
                for run_id, run in active.items():
                    self.runs[run_id] = run
                self._expire_runs(now, active)
                self.run_jobs = run_jobs
                self._note_waits(now, run_jobs)
                self._note_idle(now, runners)
            self._place_running(queue, running_tasks, active, run_jobs)
            self._link_tasks(active, run_jobs)
            if now - self.last["tasks"] >= TASK_SECONDS:
                self._ingest_finished(now)
                self.last["tasks"] = now
            self._backfill(now)
            self.forge_ok, self.forge_error = True, ""
        except (ForgejoError, OSError, ValueError) as error:
            self.forge_ok, self.forge_error = False, str(error)[:300]
        if now - self.last["stats"] >= STATS_SECONDS:
            self._refresh_stats(now)
            self.last["stats"] = now
        self.poll_seconds = self.clock() - started
        self.rebuild(now)
        if now // 60 != self.last["minute"] and self.snapshot:
            self.store.record_pools(now // 60, self.snapshot["pools"])
            self.last["minute"] = now // 60
        if now - self.last["maintain"] >= MAINTAIN_SECONDS:
            self.store.maintain(now)
            self.last["maintain"] = now

    def _refresh_sources(self) -> None:
        head = self.forge.head("main")
        if head and head == self.source_head:
            return
        files = {}
        for path in self.forge.directory(".github/workflows", head or "main"):
            if path.endswith((".yml", ".yaml")):
                files[path.rsplit("/", 1)[-1]] = self.forge.file(path, head or "main")
        roster = load_roster(self.forge.file("validation/runner-fleet.toml", head or "main"))
        with self.lock:
            self.workflows = parse_workflows(files)
            self.roster = roster
            self.source_head = head

    def _expire_runs(self, now: int, active: dict) -> None:
        # Keep finished runs for two hours so "superseded" can see newer runs.
        for run_id in list(self.runs):
            stopped = fleet.iso_seconds(self.runs[run_id].get("stopped"))
            if run_id not in active and stopped and now - stopped > 7200:
                del self.runs[run_id]

    def _note_waits(self, now: int, run_jobs: dict) -> None:
        live = set()
        for jobs in run_jobs.values():
            for job in jobs:
                if job["status"] in ("waiting", "blocked", "running"):
                    live.add(job["id"])
                    self.first_seen.setdefault(job["id"], now)
        for job_id in list(self.first_seen):
            if job_id not in live:
                del self.first_seen[job_id]

    def _note_idle(self, now: int, runners: list[dict]) -> None:
        for runner in runners:
            if runner["status"] == "idle":
                self.idle_since.setdefault(runner["name"], now)
            else:
                self.idle_since.pop(runner["name"], None)

    def _place_running(self, queue, running_tasks, active, run_jobs) -> None:
        uuids = {runner["uuid"]: runner["name"] for runner in self.runners}
        for job in queue:
            task_id = job.get("task_id")
            if job["status"] != "running" or not task_id:
                continue
            known = self.store.task(task_id)
            if known and known.get("runner"):
                continue
            placed = self.forge.job_runner(job["id"])
            run = active.get(job["run_id"], {})
            task = running_tasks.get(task_id, {})
            self.store.upsert_task(self._task_record(job, run, task, placed, uuids, run_jobs.get(job["run_id"], [])))

    def _link_tasks(self, active, run_jobs) -> None:
        """Tie every task of an active run to its job, so a job that started and
        finished between two polls is still placed later from its log."""
        for run_id, jobs in run_jobs.items():
            run = active.get(run_id, {})
            for job in jobs:
                if job.get("task_id") and job["status"] in FINISHED | {"running"}:
                    self.store.upsert_task(self._task_record(job, run, {}, None, {}, jobs))

    def _task_record(self, job, run, task, placed, uuids, siblings) -> dict:
        record = {
            "task_id": job.get("task_id"),
            "job_id": job["id"],
            "run_id": job["run_id"],
            "run_index": run.get("index_in_repo"),
            "workflow": run.get("workflow_id"),
            "pr": run.get("prettyref"),
            "name": job["name"],
            "runs_on": job.get("runs_on") or [],
            "status": job["status"],
            "created": fleet.iso_seconds(task.get("created_at")),
            "started": fleet.iso_seconds(task.get("run_started_at")),
            "stopped": fleet.iso_seconds(task.get("updated_at")) if job["status"] in FINISHED else None,
            "ready": self._ready(job, run, siblings),
        }
        if placed:
            record["runner_uuid"] = placed[0]
            record["runner"] = uuids.get(placed[0])
        return record

    def _ready(self, job, run, siblings) -> int | None:
        needs = job.get("needs") or []
        if not needs:
            return fleet.iso_seconds(run.get("started"))
        definitions = {(d.workflow, d.name): d.key for wf in self.workflows for d in wf.jobs}
        keyed = {definitions.get((run.get("workflow_id"), sibling["name"])): sibling for sibling in siblings}
        stops = []
        for key in needs:
            sibling = keyed.get(key)
            stored = self.store.task(sibling["task_id"]) if sibling and sibling.get("task_id") else None
            if not stored or not stored.get("stopped"):
                return None
            stops.append(stored["stopped"])
        return max(stops) if stops else None

    def _ingest_finished(self, now: int) -> None:
        for task in self.forge.tasks(1):
            if task.get("status") in FINISHED:
                self.store.upsert_task(self._task_times(task))

    def _backfill(self, now: int) -> None:
        """History on first start, rate-limited to every BACKFILL_EVERY polls.

        Task timestamps first (so ready times resolve), then runs and their
        jobs (which link each task to its job), then placement from job logs.
        """
        self.passes += 1
        if self.passes % BACKFILL_EVERY:
            return
        state = self.store.meta("backfill", {"tasks_page": 1, "runs_page": 1, "tasks_done": False, "runs_done": False})
        uuids = {runner["uuid"]: runner["name"] for runner in self.runners}
        horizon = now - BACKFILL_DAYS * 86400
        if not state["tasks_done"]:
            rows = self.forge.tasks(state["tasks_page"])
            for task in rows:
                self.store.upsert_task(self._task_times(task))
            oldest = min((fleet.iso_seconds(task.get("created_at")) or now for task in rows), default=0)
            state["tasks_done"] = not rows or oldest < horizon
            state["tasks_page"] += 1
        elif not state["runs_done"]:
            runs = self.forge.get(self.forge.repo_path("/actions/runs"), page=state["runs_page"],
                                  limit=BACKFILL_RUNS).get("workflow_runs", [])
            for run in runs:
                jobs = self.forge.run_jobs(run["id"])
                for job in jobs:
                    if job.get("task_id"):
                        self.store.upsert_task(self._task_record(job, run, {}, None, uuids, jobs))
            oldest = min((fleet.iso_seconds(run.get("created")) or now for run in runs), default=0)
            state["runs_done"] = not runs or oldest < horizon
            state["runs_page"] += 1
        else:
            for task in self.store.unplaced(BACKFILL_RUNS):
                placed = self.forge.job_runner(task["job_id"])
                self.store.upsert_task({"task_id": task["task_id"], "runner_uuid": placed[0] if placed else "",
                                        "runner": uuids.get(placed[0], placed[0]) if placed else ""})
        self.store.set_meta("backfill", state)

    def _task_times(self, task: dict) -> dict:
        return {
            "task_id": task["id"], "name": task.get("name"), "status": task.get("status"),
            "run_index": task.get("run_number"),
            "pr": task.get("head_branch") if str(task.get("head_branch", "")).startswith("#") else None,
            "created": fleet.iso_seconds(task.get("created_at")),
            "started": fleet.iso_seconds(task.get("run_started_at")),
            "stopped": fleet.iso_seconds(task.get("updated_at")) if task.get("status") in FINISHED else None,
        }

    def _refresh_stats(self, now: int) -> None:
        self.durations = self.store.durations(now - 30 * 86400)
        waits = self.store.waits_by_pool(now - 86400, fleet.pool_name)
        hours = self.store.pool_hours(now - 86400)
        stats = {}
        for pool in set(waits) | set(hours):
            series = hours.get(pool, [])
            busy = [hour["busy"] for hour in series]
            stats[pool] = {
                "wait": waits.get(pool, {"n": 0}),
                "busy_pct": round(100 * sum(busy) / len(busy)) if busy else None,
                "hours": series,
            }
        self.pool_stats = stats

    def rebuild(self, now: int) -> dict:
        with self.lock:
            agents = {name: report for name, report in self.agents.items()
                      if now - int(report.get("received", 0)) <= AGENT_STALE}
            placements = {}
            for job in self.queue:
                if job["status"] == "running" and job.get("task_id"):
                    stored = self.store.task(job["task_id"])
                    if stored:
                        placements[job["task_id"]] = {"runner": stored.get("runner"), "started": stored.get("started")}
            inputs = fleet.Inputs(
                now=now, runners=self.runners, queue=self.queue, runs=dict(self.runs), run_jobs=dict(self.run_jobs),
                workflows=self.workflows, roster=self.roster, hosts=self.hosts, placements=placements,
                tasks=TaskLookup(self.store), agents=agents, first_seen=dict(self.first_seen),
                idle_since=dict(self.idle_since), durations=self.durations, pool_stats=self.pool_stats,
                forge={"ok": self.forge_ok, "error": self.forge_error, "version": self.forge_version,
                       "source_head": self.source_head, "api_calls": self.forge.calls,
                       "poll_seconds": round(self.poll_seconds, 3)})
        snapshot = fleet.build(inputs)
        snapshot["finished"] = self.store.tasks_since(now - 1800, 50)
        snapshot["audit"] = self.store.audit_log(20)
        snapshot["agents"] = {name: {"host": r.get("host"), "role": r.get("role"), "age": now - int(r.get("received", now))}
                              for name, r in self.agents.items()}
        self.snapshot = snapshot
        return snapshot

    # -- agents ----------------------------------------------------------------
    def accept_report(self, report: dict) -> dict:
        now = int(self.clock())
        agent = str(report.get("agent", ""))[:64]
        if not agent:
            raise ValueError("report has no agent id")
        report["received"] = now
        for result in report.get("results", []):
            self.store.audit(result.get("actor", "agent"), result.get("action", ""), result.get("target", ""),
                             result.get("reason", ""), f"{agent}: {result.get('result', '')}"[:500])
        metrics_ = report.get("host_metrics") or {}
        if metrics_:
            used = metrics_.get("cpu_used") or {}
            mem = metrics_.get("mem_used") or {}
            self.store.record_usage("host", report.get("host", agent), now, sum(used.values()), sum(mem.values()),
                                    {"cpu": used, "mem": mem})
        placements = {}
        for job in self.queue:
            if job["status"] == "running" and job.get("task_id"):
                stored = self.store.task(job["task_id"])
                if stored and stored.get("runner"):
                    placements[stored["runner"]] = job["task_id"]
        for task in report.get("tasks", []):
            usage = task.get("usage") or {}
            self.store.record_usage("task", str(task["task_id"]), now, usage.get("cpu"), usage.get("rss"), usage)
        for runner in report.get("runners", []):
            task_id = placements.get(runner.get("name"))
            usage = runner.get("usage")
            if task_id and usage and not any(t.get("task_id") == task_id for t in report.get("tasks", [])):
                self.store.record_usage("task", str(task_id), now, usage.get("cpu"), usage.get("rss"), usage)
        with self.lock:
            self.agents[agent] = report
            pending = self.commands.pop(agent, [])
        return {"commands": pending, "poll_seconds": POLL_SECONDS}

    # -- controls --------------------------------------------------------------
    def operator(self, token: str) -> tuple[str, bool]:
        key = hashlib.sha256(token.encode()).hexdigest()
        cached = self.operators.get(key)
        if cached and self.clock() - cached[0] < OPERATOR_CACHE:
            return cached[1], cached[2]
        login, can_write = self.forge.operator(token)
        self.operators[key] = (self.clock(), login, can_write)
        return login, can_write

    def allowed_address(self, address: str) -> bool:
        try:
            ip = ipaddress.ip_address(address)
        except ValueError:
            return False
        return any(ip in network for network in self.networks)

    def control(self, actor: str, token: str, request: dict) -> dict:
        action = request.get("action")
        target = str(request.get("target", ""))
        reason = str(request.get("reason", ""))[:300]
        snapshot = self.snapshot or self.rebuild(int(self.clock()))
        runners = {runner["name"]: runner for runner in snapshot["runners"]}
        try:
            if action == "cancel_run":
                self.forge.cancel_run(int(target), token)
                result = "cancelled"
            elif action == "delete_runner":
                runner = runners.get(target)
                stale = {name for finding in snapshot["drift"] if finding["kind"] == "stale"
                         for name in finding.get("runners", [])}
                if not runner or runner["online"] or target not in stale or runner.get("agent"):
                    raise PermissionError("only offline, unrostered runners with no agent unit can be deleted")
                self.forge.delete_runner(runner["id"], token)
                result = "deleted"
            elif action in ("drain", "resume", "start_vm"):
                agent = self._owner(snapshot, action, target)
                if not agent:
                    raise PermissionError(f"no reporting agent can {action} {target}")
                command = {"id": secrets.token_hex(8), "action": action, "target": target,
                           "actor": actor, "reason": reason}
                with self.lock:
                    self.commands.setdefault(agent, []).append(command)
                result = f"queued for {agent}"
            else:
                raise ValueError(f"unknown action {action!r}")
        except (PermissionError, ValueError, ForgejoError) as error:
            self.store.audit(actor, str(action), target, reason, f"refused: {error}")
            return {"ok": False, "error": str(error)}
        self.store.audit(actor, action, target, reason, result)
        return {"ok": True, "result": result}

    def _owner(self, snapshot: dict, action: str, target: str) -> str | None:
        with self.lock:
            reports = dict(self.agents)
        for name, report in reports.items():
            if action == "start_vm":
                if any(vm.get("name") == target and "start_vm" in vm.get("controls", []) for vm in report.get("vms", [])):
                    return name
            elif any(r.get("name") == target and action in r.get("controls", []) for r in report.get("runners", [])):
                return name
        return None

    def job_detail(self, task_id: int) -> dict:
        task = self.store.task(task_id) or {}
        recent = self.store.recent_runs_of(task.get("name", ""), 10) if task else []
        return {"task": task, "recent": recent, "usage": self.store.series("task", str(task_id)),
                "host_usage": self.store.series("host", fleet.host_of(task.get("runner") or ""),
                                                (task.get("started") or 0) - 60) if task.get("runner") else []}


def make_handler(collector: Collector):
    class Handler(http.server.BaseHTTPRequestHandler):
        server_version = "ci-fleetd"

        def log_message(self, *args):
            pass

        def _send(self, status: int, body, content_type="application/json"):
            data = body if isinstance(body, bytes) else (
                body.encode() if isinstance(body, str) else json.dumps(body).encode())
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(data)

        def _body(self) -> dict:
            length = min(int(self.headers.get("Content-Length") or 0), 2_000_000)
            return json.loads(self.rfile.read(length) or b"{}")

        def do_GET(self):
            url = urllib.parse.urlsplit(self.path)
            query = dict(urllib.parse.parse_qsl(url.query))
            snapshot = collector.snapshot or collector.rebuild(int(collector.clock()))
            if url.path == "/api/snapshot":
                return self._send(200, snapshot)
            if url.path.startswith("/api/job/"):
                return self._send(200, collector.job_detail(int(url.path.rsplit("/", 1)[-1])))
            if url.path == "/api/why":
                text = explain.why(snapshot, pr=query.get("pr"), run=int(query["run"]) if query.get("run") else None,
                                   task=int(query["task"]) if query.get("task") else None)
                return self._send(200, text + "\n", "text/plain; charset=utf-8")
            if url.path == "/api/status":
                return self._send(200, explain.status(snapshot) + "\n", "text/plain; charset=utf-8")
            if url.path == "/api/drift":
                return self._send(200, explain.drift(snapshot) + "\n", "text/plain; charset=utf-8")
            if url.path == "/api/audit":
                return self._send(200, collector.store.audit_log(200))
            if url.path == "/metrics":
                return self._send(200, metrics.render(snapshot, poll_seconds=collector.poll_seconds,
                                                      api_calls=collector.forge.calls, forge_up=collector.forge_ok),
                                  "text/plain; version=0.0.4")
            if url.path == "/healthz":
                return self._send(200 if collector.forge_ok else 503, {"forge": collector.forge_ok})
            name = "index.html" if url.path in ("/", "") else url.path.lstrip("/")
            path = (UI / name).resolve()
            if UI.resolve() in path.parents and path.is_file():
                kind = mimetypes.guess_type(path.name)[0] or "application/octet-stream"
                return self._send(200, path.read_bytes(), kind)
            return self._send(404, {"error": "not found"})

        def do_POST(self):
            url = urllib.parse.urlsplit(self.path)
            try:
                body = self._body()
            except (ValueError, json.JSONDecodeError):
                return self._send(400, {"error": "invalid JSON"})
            if url.path == "/api/agent":
                supplied = (self.headers.get("Authorization") or "").removeprefix("Bearer ").strip()
                if not collector.agent_token or not secrets.compare_digest(supplied, collector.agent_token):
                    return self._send(401, {"error": "agent token required"})
                try:
                    return self._send(200, collector.accept_report(body))
                except ValueError as error:
                    return self._send(400, {"error": str(error)})
            if url.path == "/api/control":
                if not collector.allowed_address(self.client_address[0]):
                    return self._send(403, {"error": "controls are not accepted from this address"})
                token = (self.headers.get("Authorization") or "").removeprefix("token ").strip()
                if not token:
                    return self._send(401, {"error": "sign in with your Forgejo token"})
                try:
                    login, can_write = collector.operator(token)
                except ForgejoError:
                    return self._send(401, {"error": "the forge rejected that token"})
                if not can_write:
                    return self._send(403, {"error": f"{login} cannot write {collector.forge.repo}"})
                result = collector.control(login, token, body)
                return self._send(200 if result["ok"] else 409, result)
            if url.path == "/api/whoami":
                token = (self.headers.get("Authorization") or "").removeprefix("token ").strip()
                try:
                    login, can_write = collector.operator(token)
                except ForgejoError:
                    return self._send(401, {"error": "the forge rejected that token"})
                return self._send(200, {"login": login, "can_write": can_write,
                                        "address_allowed": collector.allowed_address(self.client_address[0])})
            return self._send(404, {"error": "not found"})

    return Handler


def run_forever(collector: Collector, listen: str) -> None:
    host, port = listen.rsplit(":", 1)
    server = http.server.ThreadingHTTPServer((host, int(port)), make_handler(collector))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    while True:
        started = time.time()
        try:
            collector.poll()
        except Exception:  # a poll bug must not take the console down
            traceback.print_exc()
        time.sleep(max(0.5, POLL_SECONDS - (time.time() - started)))


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="ci-fleet collector")
    credentials = os.environ.get("CREDENTIALS_DIRECTORY")
    default = os.environ.get("CI_FLEET_CONFIG") or (
        f"{credentials}/collector.json" if credentials else "/etc/ci-fleet/collector.json")
    parser.add_argument("--config", default=default)
    args = parser.parse_args(argv)
    config = json.loads(Path(args.config).read_text())
    Path(config.get("db", "/var/lib/ci-fleet/ci-fleet.db")).parent.mkdir(parents=True, exist_ok=True)
    run_forever(Collector(config), config.get("listen", "0.0.0.0:8790"))
    return 0
