"""Prometheus exposition for a snapshot.

Labels are pool, host, runner, class and severity only — never a PR, run or
task — so cardinality is bounded by the size of the fleet.
"""

from __future__ import annotations


def _escape(value) -> str:
    return str(value).replace("\\", "\\\\").replace('"', '\\"').replace("\n", " ")


def render(snapshot: dict, *, poll_seconds: float, api_calls: int, forge_up: bool) -> str:
    lines: list[str] = []

    def family(name: str, kind: str, help_text: str, samples):
        lines.append(f"# HELP {name} {help_text}")
        lines.append(f"# TYPE {name} {kind}")
        for labels, value in samples:
            if value is None:
                continue
            rendered = ",".join(f'{key}="{_escape(val)}"' for key, val in labels.items())
            lines.append(f"{name}{{{rendered}}} {value}" if rendered else f"{name} {value}")

    counts = snapshot["counts"]
    family("ci_fleet_forgejo_up", "gauge", "1 when the last poll of the forge succeeded.", [({}, int(forge_up))])
    family("ci_fleet_poll_duration_seconds", "gauge", "Wall time of the last poll.", [({}, round(poll_seconds, 3))])
    family("ci_fleet_forgejo_api_calls_total", "counter", "Forge API calls made by the collector.", [({}, api_calls)])
    family("ci_fleet_jobs", "gauge", "Jobs by state.",
           [({"state": state}, counts[state]) for state in ("running", "waiting", "blocked")])
    family("ci_fleet_waiting_jobs", "gauge", "Waiting and blocked jobs by why-waiting class.",
           [({"class": name}, value) for name, value in snapshot["classes"].items()])
    family("ci_fleet_unsatisfiable_jobs", "gauge", "Jobs no registered runner can take.", [({}, counts["unsatisfiable"])])
    family("ci_fleet_unsatisfiable_pools", "gauge", "Label sets workflows request that no registered runner carries.",
           [({}, sum(1 for pool in snapshot["pools"] if pool["requested"] and not pool["registered"]))])
    family("ci_fleet_oldest_wait_seconds", "gauge", "Oldest waiting job's wait.", [({}, snapshot["oldest_wait"])])
    requested = [pool for pool in snapshot["pools"] if pool["requested"] or pool["waiting"]]
    family("ci_fleet_pool_runners", "gauge", "Runners per requested pool by state.",
           [({"pool": pool["name"], "state": state}, pool[state])
            for pool in requested for state in ("registered", "online", "busy", "free", "waiting")])
    family("ci_fleet_pool_oldest_wait_seconds", "gauge", "Oldest wait per requested pool.",
           [({"pool": pool["name"]}, pool["oldest_wait"]) for pool in requested])
    family("ci_fleet_pool_next_schedule_timestamp", "gauge", "Next scheduled run that requests the pool.",
           [({"pool": pool["name"]}, pool["next_schedule"]) for pool in requested if pool["next_schedule"]])
    family("ci_fleet_runner_up", "gauge", "1 when the runner is online.",
           [({"runner": r["name"], "host": r["alias"]}, int(r["online"])) for r in snapshot["runners"]])
    family("ci_fleet_runner_busy", "gauge", "Jobs the runner is executing.",
           [({"runner": r["name"], "host": r["alias"]}, r["busy"]) for r in snapshot["runners"]])
    family("ci_fleet_runner_paused", "gauge", "1 when the runner is drained or paused.",
           [({"runner": r["name"], "host": r["alias"]}, int(r["paused"])) for r in snapshot["runners"]])
    severities: dict[str, int] = {}
    for finding in snapshot["drift"]:
        severities[finding["severity"]] = severities.get(finding["severity"], 0) + 1
    family("ci_fleet_drift_findings", "gauge", "Roster-vs-live drift findings by severity.",
           [({"severity": severity}, severities.get(severity, 0)) for severity in ("blocking", "warn", "info")])
    host_cpu, host_mem, ages = [], [], []
    for host in snapshot["hosts"]:
        metrics = host.get("metrics") or {}
        for owner, cores in (metrics.get("cpu_used") or {}).items():
            host_cpu.append(({"host": host["alias"], "owner": owner}, round(cores, 3)))
        for owner, used in (metrics.get("mem_used") or {}).items():
            host_mem.append(({"host": host["alias"], "owner": owner}, int(used)))

    family("ci_fleet_host_cpu_cores", "gauge", "Cores in use per host by owner (ci, plurx, other).", host_cpu)
    family("ci_fleet_host_memory_bytes", "gauge", "Memory in use per host by owner.", host_mem)
    for agent, info in sorted((snapshot.get("agents") or {}).items()):
        ages.append(({"agent": agent, "host": info.get("host") or ""}, info.get("age")))
    family("ci_fleet_agent_age_seconds", "gauge",
           "Seconds since each agent last reported; kept for every agent ever seen, so silence grows it.", ages)
    return "\n".join(lines) + "\n"
