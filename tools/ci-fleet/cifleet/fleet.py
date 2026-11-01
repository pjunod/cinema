"""Join the forge, the roster, the workflows and agent reports into one snapshot.

Everything here is a pure function of its inputs: the collector feeds it live
data every poll, `scripts/ci-fleet --direct` feeds it one fetch, and the tests
feed it captured responses. One snapshot is the only source of every count the
console, the CLI and `/metrics` show, so no two of them can disagree.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
import re

from validation.runner_fleet import (
    IMPLICIT,
    Workflow,
    eligible,
    next_cron,
    requested_pools,
    rostered_runners,
)

# Labels every Linux lab job carries; a pool's short name leaves them out.
QUIET_LABELS = {IMPLICIT, "lab", "Linux", "X64"}
# Labels that describe how a runner is hosted rather than select work for it.
# They never count as "unused" just because no workflow asks for them.
DESCRIPTIVE = re.compile(r"^(incus|baremetal|laptop-vm|ubuntu-[0-9.]+|macOS|Linux|X64|ARM64|lab|self-hosted)$")
NAME = re.compile(r"^gha-(?P<host>[a-z0-9]+)-(?P<role>.+)$")
POLL_SECONDS = 5
NOT_PICKED_UP_POLLS = 12  # an idle eligible runner for 60 s
LOADED_HOST = 0.40  # non-CI share of a host's cores

CLASS_ORDER = ("blocked", "held", "unsatisfiable", "offline_only", "busy", "not_picked_up", "queued")


def pool_name(labels) -> str:
    rest = set(labels) - QUIET_LABELS
    if "macOS" in rest:
        rest -= {"macOS", "ARM64"}
    rest = sorted(rest)
    return " · ".join(rest) if rest else "lab"


def iso_seconds(value: str | None) -> int | None:
    """Forgejo timestamps; the zero time it uses for 'not yet' becomes None."""
    if not value or value.startswith("0001") or value.startswith("1970"):
        return None
    return int(datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp())


@dataclass
class Inputs:
    now: int
    runners: list[dict]
    queue: list[dict]
    runs: dict[int, dict]  # run id -> run record
    run_jobs: dict[int, list[dict]]  # run id -> its jobs
    workflows: list[Workflow]
    roster: dict | None
    hosts: dict[str, dict] = field(default_factory=dict)  # forge host token -> {"alias": ...}
    placements: dict[int, dict] = field(default_factory=dict)  # task id -> {"runner", "started", "stopped"}
    tasks: dict[int, dict] = field(default_factory=dict)  # task id -> {"stopped", "status", ...}
    agents: dict[str, dict] = field(default_factory=dict)  # agent id -> latest report
    first_seen: dict[int, int] = field(default_factory=dict)  # job id -> first poll it was waiting
    idle_since: dict[str, int] = field(default_factory=dict)  # runner -> first poll it was idle
    durations: dict[str, dict] = field(default_factory=dict)  # job name -> {"p50", "p95", "n"}
    pool_stats: dict[str, dict] = field(default_factory=dict)  # pool -> 24 h stats
    forge: dict = field(default_factory=dict)


def host_of(name: str) -> str:
    match = NAME.match(name)
    return match.group("host") if match else name


def neutral_name(name: str, hosts: dict[str, dict]) -> str:
    match = NAME.match(name)
    if not match:
        return name
    alias = hosts.get(match.group("host"), {}).get("alias", match.group("host"))
    return f"gha-{alias}-{match.group('role')}"


def _agent_runners(agents: dict[str, dict]) -> dict[str, dict]:
    """One entry per runner, merged across agents.

    An Incus runner is reported twice: by its host's agent (VM state, the
    controls) and by the agent inside the guest (usage, hygiene). Neither
    report's facts may erase the other's.
    """
    found: dict[str, dict] = {}
    for report in agents.values():
        for runner in report.get("runners", []):
            entry = found.setdefault(runner["name"], {"controls": [], "agents": []})
            for key, value in runner.items():
                if key == "controls":
                    entry["controls"] = sorted(set(entry["controls"]) | set(value))
                elif value not in (None, "", {}, []) or key not in entry:
                    entry[key] = value
            entry["agents"].append(report.get("agent"))
            if "controls" in runner and runner["controls"]:
                entry["agent"] = report.get("agent")
            entry.setdefault("agent", report.get("agent"))
    return found


def _agent_tasks(agents: dict[str, dict]) -> dict[int, dict]:
    found: dict[int, dict] = {}
    for report in agents.values():
        for task in report.get("tasks", []):
            found[int(task["task_id"])] = task
    return found


def build(inputs: Inputs) -> dict:
    hosts_cfg = inputs.hosts
    roster = inputs.roster or {"runners": [], "voter_hosts": []}
    voters = set(roster.get("voter_hosts", []))
    agent_runners = _agent_runners(inputs.agents)
    agent_tasks = _agent_tasks(inputs.agents)

    # -- runners --------------------------------------------------------------
    by_uuid = {runner["uuid"]: runner["name"] for runner in inputs.runners}
    running_on: dict[str, list[dict]] = {}
    jobs_by_task: dict[int, dict] = {}
    for run_id, jobs in inputs.run_jobs.items():
        for job in jobs:
            if job.get("task_id"):
                jobs_by_task[job["task_id"]] = dict(job, run_id=run_id)
    for job in inputs.queue:
        if job["status"] == "running" and job.get("task_id"):
            placement = inputs.placements.get(job["task_id"])
            if placement and placement.get("runner"):
                running_on.setdefault(placement["runner"], []).append(job)

    runners = []
    for live in sorted(inputs.runners, key=lambda r: r["name"]):
        labels = [label if isinstance(label, str) else label["name"] for label in live["labels"]]
        host = host_of(live["name"])
        alias = hosts_cfg.get(host, {}).get("alias", host)
        agent = agent_runners.get(live["name"], {})
        capacity = int(agent.get("capacity") or 1)
        busy = running_on.get(live["name"], [])
        paused = bool(agent.get("paused"))
        status = live["status"]
        runners.append({
            "id": live["id"],
            "name": live["name"],
            "neutral": neutral_name(live["name"], hosts_cfg),
            "uuid": live.get("uuid"),
            "host": host,
            "alias": alias,
            "voter": alias in voters,
            "labels": labels,
            "status": status,
            "online": status != "offline",
            "paused": paused,
            "version": live.get("version", ""),
            "capacity": capacity,
            "busy": len(busy) if busy else (1 if status == "active" else 0),
            "tasks": [job["task_id"] for job in busy],
            "idle_since": inputs.idle_since.get(live["name"]),
            "offline_reason": agent.get("offline_reason") or ("" if status != "offline" else "not reporting to the forge"),
            "agent": agent.get("agent"),
            "controls": agent.get("controls", []),
            "hygiene": agent.get("hygiene", {}),
            "usage": agent.get("usage"),
        })
    runner_by_name = {runner["name"]: runner for runner in runners}

    # -- workflows: map (workflow, display name) -> job definition --------------
    definitions = {(job.workflow.rsplit("/", 1)[-1], job.name): job for wf in inputs.workflows for job in wf.jobs}

    def definition(run: dict, name: str):
        return definitions.get(((run or {}).get("workflow_id") or "", name))

    newest_run: dict[tuple, int] = {}
    for run in inputs.runs.values():
        key = (run.get("workflow_id"), run.get("prettyref"))
        newest_run[key] = max(newest_run.get(key, 0), run["id"])

    p50 = {name: stats.get("p50") for name, stats in inputs.durations.items()}

    # -- jobs -------------------------------------------------------------------
    waiting, running, blocked = [], [], []
    seen_jobs = set()
    candidates = list(inputs.queue)
    for run_id, jobs in inputs.run_jobs.items():
        for job in jobs:
            if job["status"] == "blocked":
                candidates.append(dict(job, run_id=run_id))
    for job in candidates:
        if job["id"] in seen_jobs:
            continue
        seen_jobs.add(job["id"])
        run = inputs.runs.get(job["run_id"], {})
        base = {
            "job_id": job["id"],
            "task_id": job.get("task_id") or None,
            "name": job["name"],
            "runs_on": job.get("runs_on") or [],
            "pool": pool_name(job.get("runs_on") or []),
            "run_id": job["run_id"],
            "run_index": run.get("index_in_repo"),
            "run_status": run.get("status"),
            "workflow": run.get("workflow_id"),
            "pr": run.get("prettyref"),
            "title": run.get("title"),
            "needs": job.get("needs") or [],
        }
        if job["status"] == "running":
            placement = inputs.placements.get(job.get("task_id") or 0, {})
            started = placement.get("started") or inputs.first_seen.get(job["id"])
            task = jobs_by_task.get(job.get("task_id") or 0)
            usage = agent_tasks.get(job.get("task_id") or 0) or (runner_by_name.get(placement.get("runner"), {}) or {}).get("usage")
            running.append(dict(base,
                                runner=placement.get("runner"),
                                host=host_of(placement.get("runner") or ""),
                                started=started,
                                elapsed=(inputs.now - started) if started else None,
                                p50=p50.get(job["name"]),
                                usage=usage,
                                attributed=bool(task)))
            continue
        ready, exact = _ready_at(job, run, inputs, definition)
        verdict = classify(job, run, inputs, runners, definition, newest_run, ready, p50)
        entry = dict(base, ready=ready, ready_exact=exact, waited=(inputs.now - ready) if ready else None, **verdict)
        (blocked if verdict["class"] in ("blocked", "held") else waiting).append(entry)

    waiting.sort(key=lambda job: job.get("ready") or inputs.now)
    blocked.sort(key=lambda job: job["run_id"])
    running.sort(key=lambda job: job.get("started") or 0)

    pools = _pools(inputs, runners, waiting)
    drift = _drift(inputs, runners, roster, pools, waiting)
    hosts = _hosts(inputs, runners, voters)
    counts = {
        "registered": len(runners),
        "online": sum(runner["online"] for runner in runners),
        "offline": sum(not runner["online"] for runner in runners),
        "busy": sum(1 for runner in runners if runner["busy"]),
        "running": len(running),
        "waiting": len(waiting),
        "blocked": len(blocked),
        "unsatisfiable": sum(job["class"] == "unsatisfiable" for job in waiting),
        "drift": len(drift),
        "stale": sum(1 for finding in drift if finding["kind"] == "stale"
                     for _ in finding.get("runners", [])),
        "agents": len(inputs.agents),
    }
    by_class = {name: sum(job["class"] == name for job in waiting + blocked) for name in CLASS_ORDER}
    return {
        "generated": inputs.now,
        "forge": inputs.forge,
        "counts": counts,
        "classes": by_class,
        "oldest_wait": max((job["waited"] or 0 for job in waiting), default=0),
        "alerts": _alerts(waiting, pools, inputs.now),
        "hosts": hosts,
        "runners": runners,
        "jobs": {"running": running, "waiting": waiting, "blocked": blocked},
        "pools": pools,
        "drift": drift,
    }


def _ready_at(job: dict, run: dict, inputs: Inputs, definition) -> tuple[int | None, bool]:
    """(when the job's needs had all finished, whether that is exact).

    Until the needs' tasks are recorded, the run's start is the earliest the
    job could have been ready, so the wait shown is an upper bound ("≤").
    """
    started = iso_seconds(run.get("started"))
    needs = job.get("needs") or []
    if not needs:
        return started or inputs.first_seen.get(job["id"]), bool(started)
    by_key = _run_jobs_by_key(job["run_id"], run, inputs, definition)
    stops = []
    for key in needs:
        need = by_key.get(key)
        task = inputs.tasks.get((need or {}).get("task_id") or 0)
        if task and task.get("stopped"):
            stops.append(task["stopped"])
    if stops and len(stops) == len(needs):
        return max(stops), True
    return started or inputs.first_seen.get(job["id"]), False


def _run_jobs_by_key(run_id: int, run: dict, inputs: Inputs, definition) -> dict[str, dict]:
    keyed = {}
    for job in inputs.run_jobs.get(run_id, []):
        found = definition(run, job["name"])
        if found:
            keyed[found.key] = job
    return keyed


def classify(job: dict, run: dict, inputs: Inputs, runners: list[dict], definition, newest_run, ready, p50) -> dict:
    """One class per waiting job — the first rule that matches wins."""
    flags: list[str] = []
    notes: list[str] = []
    needs = job.get("needs") or []
    by_key = _run_jobs_by_key(job["run_id"], run, inputs, definition)
    need_status = {key: (by_key.get(key) or {}).get("status") for key in needs}
    failed = sorted(key for key, status in need_status.items() if status in ("failure", "cancelled"))
    if failed:
        flags.append("dead_on_pickup")
        notes.append(f"needed job(s) {', '.join(failed)} did not succeed, so a runner would only skip this")
    newest = newest_run.get((run.get("workflow_id"), run.get("prettyref")))
    if newest and newest > job["run_id"]:
        newer = inputs.runs.get(newest, {})
        flags.append("superseded")
        notes.append(f"superseded by run #{newer.get('index_in_repo', newest)}")

    if job["status"] == "blocked":
        unfinished = [key for key, status in need_status.items()
                      if status not in ("success", "failure", "cancelled", "skipped")]
        if unfinished or len(by_key) < len(needs):
            waiting_on = unfinished or needs
            return _verdict("blocked", f"waiting for {', '.join(waiting_on)}", flags, notes)
        return _verdict("held", "its needs have finished; a concurrency group is holding it", flags, notes)

    requested = set(job.get("runs_on") or [])
    eligible_runners = [runner for runner in runners if eligible(runner["labels"], requested)]
    loaded = {host for host, share in _foreign_share(inputs).items() if share > LOADED_HOST}
    if any(runner["host"] in loaded for runner in eligible_runners if runner["online"]):
        flags.append("loaded_host")

    if not eligible_runners:
        closest = sorted(
            ({"runner": runner["name"], "missing": sorted(requested - set(runner["labels"])),
              "has": len(requested & set(runner["labels"]))} for runner in runners),
            key=lambda item: (-item["has"], item["runner"]))
        closest = [item for item in closest if len(item["missing"]) <= max(1, len(closest[0]["missing"]) if closest else 1)][:3]
        absent = sorted(label for label in requested if not any(label in r["labels"] for r in runners))
        reason = f"no registered runner has all {len(requested)} labels"
        if absent:
            reason += f"; none carries {', '.join(absent)}"
        return _verdict("unsatisfiable", reason, flags, notes, closest=closest, missing_labels=absent)

    usable = [runner for runner in eligible_runners if runner["online"] and not runner["paused"]]
    names = [runner["name"] for runner in eligible_runners]
    if not usable:
        reasons = [{"runner": runner["name"],
                    "reason": "paused" if runner["paused"] else runner["offline_reason"]}
                   for runner in eligible_runners]
        return _verdict("offline_only", f"all {len(eligible_runners)} eligible runners are offline or paused",
                        flags, notes, eligible=names, offline=reasons)

    free = [runner for runner in usable if runner["busy"] < runner["capacity"]]
    if not free:
        etas = []
        for runner in usable:
            for task_id in runner["tasks"]:
                placement = inputs.placements.get(task_id, {})
                running_job = next((q for q in inputs.queue if q.get("task_id") == task_id), {})
                expected = p50.get(running_job.get("name"))
                start = placement.get("started")
                etas.append({"runner": runner["name"], "job": running_job.get("name"),
                             "eta": (start + expected) if start and expected else None})
        soonest = min((item["eta"] for item in etas if item["eta"]), default=None)
        return _verdict("busy", f"all {len(usable)} usable eligible runners are busy", flags, notes,
                        eligible=names, busy=etas, eta=soonest)

    waited = inputs.now - ready if ready else 0
    idle_long = [runner["name"] for runner in free
                 if runner.get("idle_since") and inputs.now - runner["idle_since"] >= NOT_PICKED_UP_POLLS * POLL_SECONDS]
    if idle_long and waited >= NOT_PICKED_UP_POLLS * POLL_SECONDS:
        return _verdict("not_picked_up", f"{len(idle_long)} eligible runner(s) idle for 60 s+ without taking it",
                        flags, notes, eligible=names, idle=idle_long)
    return _verdict("queued", "just queued; a free eligible runner should take it on its next poll",
                    flags, notes, eligible=names)


def _verdict(kind: str, reason: str, flags, notes, **extra) -> dict:
    return {"class": kind, "reason": reason, "flags": list(flags), "notes": list(notes), **extra}


def _foreign_share(inputs: Inputs) -> dict[str, float]:
    shares = {}
    for report in inputs.agents.values():
        metrics = report.get("host_metrics") or {}
        cores = metrics.get("cpu_cores") or 0
        used = metrics.get("cpu_used") or {}
        if cores and report.get("role", "host") == "host":
            shares[report.get("host", "")] = (used.get("plurx", 0) + used.get("other", 0)) / cores
    return shares


def _pools(inputs: Inputs, runners: list[dict], waiting: list[dict]) -> list[dict]:
    requested = requested_pools(inputs.workflows)
    for job in waiting:  # a live job may ask for a set no current workflow does
        requested.setdefault(frozenset(job["runs_on"]), [])
    schedules = {wf.file.rsplit("/", 1)[-1]: wf.schedules for wf in inputs.workflows}
    now = datetime.fromtimestamp(inputs.now, timezone.utc)
    pools = []
    for labels, jobs in requested.items():
        members = [runner for runner in runners if eligible(runner["labels"], labels)]
        name = pool_name(labels)
        next_runs = []
        for job in jobs:
            for expression in schedules.get(job.workflow.rsplit("/", 1)[-1], []):
                moment = next_cron(expression, now)
                if moment:
                    next_runs.append(int(moment.timestamp()))
        queued = [job for job in waiting if frozenset(job["runs_on"]) == labels]
        pools.append({
            "name": name,
            "labels": sorted(labels),
            "used_by": sorted({f"{job.name} ({job.workflow.rsplit('/', 1)[-1]})" for job in jobs}),
            "runners": [runner["name"] for runner in members],
            "registered": len(members),
            "online": sum(runner["online"] for runner in members),
            "busy": sum(1 for runner in members if runner["busy"]),
            "free": sum(max(0, runner["capacity"] - runner["busy"]) for runner in members
                        if runner["online"] and not runner["paused"]),
            "waiting": len(queued),
            "oldest_wait": max((job["waited"] or 0 for job in queued), default=0),
            "next_schedule": min(next_runs) if next_runs else None,
            "requested": bool(jobs),
            "stats": inputs.pool_stats.get(name, {}),
        })
    pools.sort(key=lambda pool: (pool["online"] > 0, -pool["waiting"], pool["name"]))
    in_use = {label for labels in requested for label in labels}
    unused = sorted(label for label in {label for runner in runners for label in runner["labels"]} - in_use
                    if not DESCRIPTIVE.match(label))
    for label in unused:
        holders = [runner for runner in runners if label in runner["labels"]]
        pools.append({"name": label, "labels": [label], "used_by": [], "runners": [r["name"] for r in holders],
                      "registered": len(holders), "online": sum(r["online"] for r in holders),
                      "busy": sum(1 for r in holders if r["busy"]), "free": 0, "waiting": 0, "oldest_wait": 0,
                      "next_schedule": None, "requested": False, "stats": {}})
    return pools


def _drift(inputs: Inputs, runners: list[dict], roster: dict, pools: list[dict], waiting: list[dict]) -> list[dict]:
    findings: list[dict] = []
    rostered = dict(rostered_runners(roster)) if roster.get("runners") else {}
    live = {runner["neutral"]: runner for runner in runners}

    absent = sorted({label for job in waiting if job["class"] == "unsatisfiable" for label in job.get("missing_labels", [])})
    if absent:
        findings.append({"severity": "blocking", "kind": "unsatisfiable",
                         "title": f"Jobs need {', '.join(absent)}; no registered runner carries it",
                         "jobs": [job["job_id"] for job in waiting if job["class"] == "unsatisfiable"],
                         "roster": sorted(name for name, labels in rostered.items() if set(absent) & labels),
                         "fix": "Register a runner with the label, or change the workflow to a label that exists."})
    for pool in pools:
        if pool["requested"] and pool["registered"] and not pool["online"]:
            findings.append({"severity": "warn", "kind": "pool_offline",
                             "title": f"Pool {pool['name']} has 0 of {pool['registered']} runners online",
                             "runners": pool["runners"], "next_schedule": pool["next_schedule"],
                             "fix": "Start or repair a runner in this pool before its next run."})
    mismatched = []
    for name, labels in sorted(rostered.items()):
        runner = live.get(name)
        if runner is None:
            continue
        actual = set(runner["labels"])
        if actual != labels:
            mismatched.append({"runner": runner["name"], "roster_only": sorted(labels - actual),
                               "live_only": sorted(actual - labels)})
    if mismatched:
        findings.append({"severity": "warn", "kind": "labels", "title": "Registered labels differ from the roster",
                         "runners": mismatched,
                         "fix": "Change the runner registration or validation/runner-fleet.toml so they agree."})
    expected_offline = [live[name]["name"] for name in rostered if name in live and not live[name]["online"]]
    if expected_offline:
        findings.append({"severity": "warn", "kind": "expected_online", "title": "Rostered runners are offline",
                         "runners": expected_offline,
                         "reasons": {live[n]["name"]: live[n]["offline_reason"] for n in rostered if n in live and not live[n]["online"]},
                         "fix": "Start the runner (Start VM), or take it out of the roster."})
    missing = sorted(name for name in rostered if name not in live)
    if missing:
        findings.append({"severity": "warn", "kind": "unregistered", "title": "Rostered runners are not registered",
                         "runners": missing, "fix": "Register them, or remove them from the roster."})
    unrostered = [runner for runner in runners if runner["neutral"] not in rostered] if rostered else []
    stale = [runner["name"] for runner in unrostered if not runner["online"]]
    extra = [runner["name"] for runner in unrostered if runner["online"]]
    if stale:
        findings.append({"severity": "warn", "kind": "stale", "title": "Stale registrations (offline, not in the roster)",
                         "runners": stale, "fix": "Delete the registrations."})
    if extra:
        findings.append({"severity": "warn", "kind": "unrostered", "title": "Online runners missing from the roster",
                         "runners": extra, "fix": "Add them to validation/runner-fleet.toml."})
    not_neutral = sorted({runner["host"] for runner in runners if runner["host"] != runner["alias"]})
    if not_neutral:
        findings.append({"severity": "info", "kind": "names",
                         "title": f"{sum(r['host'] != r['alias'] for r in runners)} registrations use real host names",
                         "hosts": not_neutral, "fix": "Re-register under the roster's neutral names."})
    versions: dict[str, list[str]] = {}
    for runner in runners:
        versions.setdefault(runner["version"], []).append(runner["name"])
    if len(versions) > 1:
        common = max(versions, key=lambda version: len(versions[version]))
        findings.append({"severity": "info", "kind": "versions", "title": "Runner versions differ",
                         "versions": {version: names for version, names in versions.items() if version != common},
                         "common": common, "fix": "Upgrade the odd runners to one version."})
    unused = [pool for pool in pools if not pool["requested"]]
    if unused:
        findings.append({"severity": "info", "kind": "unused_labels", "title": "Labels no workflow requests",
                         "labels": [pool["name"] for pool in unused], "fix": "Drop them, or note why they stay."})
    order = {"blocking": 0, "warn": 1, "info": 2}
    findings.sort(key=lambda finding: order[finding["severity"]])
    return findings


def _hosts(inputs: Inputs, runners: list[dict], voters: set[str]) -> list[dict]:
    reports = {report.get("host"): report for report in inputs.agents.values() if report.get("role", "host") == "host"}
    hosts: dict[str, dict] = {}
    for runner in runners:
        entry = hosts.setdefault(runner["host"], {"name": runner["host"], "alias": runner["alias"],
                                                  "voter": runner["alias"] in voters, "runners": []})
        entry["runners"].append(runner["name"])
    for name, report in reports.items():
        entry = hosts.setdefault(name, {"name": name, "alias": inputs.hosts.get(name, {}).get("alias", name),
                                        "voter": False, "runners": []})
        entry["agent_age"] = inputs.now - int(report.get("ts", inputs.now))
        entry["metrics"] = report.get("host_metrics")
        entry["vms"] = report.get("vms", [])
        entry["watchdog"] = report.get("watchdog")
    return sorted(hosts.values(), key=lambda host: host["name"])


def _alerts(waiting: list[dict], pools: list[dict], now: int) -> list[dict]:
    alerts = []
    unsatisfiable = [job for job in waiting if job["class"] == "unsatisfiable"]
    if unsatisfiable:
        labels = sorted({label for job in unsatisfiable for label in job.get("missing_labels", [])})
        alerts.append({"severity": "critical", "kind": "unsatisfiable", "count": len(unsatisfiable),
                       "labels": labels, "oldest": max(job["waited"] or 0 for job in unsatisfiable)})
    for pool in pools:
        if pool["requested"] and pool["registered"] and not pool["online"]:
            alerts.append({"severity": "warning", "kind": "pool_offline", "pool": pool["name"],
                           "registered": pool["registered"], "next_schedule": pool["next_schedule"]})
        elif pool["requested"] and pool["registered"] and pool["online"] * 2 < pool["registered"]:
            alerts.append({"severity": "warning", "kind": "pool_degraded", "pool": pool["name"],
                           "online": pool["online"], "registered": pool["registered"]})
    return alerts
