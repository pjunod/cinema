"""Plain-words answers for agent sessions: `ci-fleet why` and `ci-fleet status`."""

from __future__ import annotations

CLASS_WORDS = {
    "blocked": "BLOCKED",
    "held": "HELD (concurrency)",
    "unsatisfiable": "UNSATISFIABLE",
    "offline_only": "OFFLINE ONLY",
    "busy": "BUSY",
    "not_picked_up": "NOT PICKED UP",
    "queued": "QUEUED",
}


def duration(seconds) -> str:
    if seconds is None:
        return "—"
    seconds = int(seconds)
    if seconds < 60:
        return f"{seconds}s"
    minutes, seconds = divmod(seconds, 60)
    if minutes < 60:
        return f"{minutes}m {seconds:02d}s"
    hours, minutes = divmod(minutes, 60)
    return f"{hours}h {minutes:02d}m"


def job_line(job: dict) -> str:
    head = f"{job['name']} · {job.get('pr') or ''} run #{job.get('run_index')}"
    if "class" not in job:
        runner = job.get("runner") or "unplaced"
        return f"RUNNING  {head} · on {runner} · {duration(job.get('elapsed'))}" + (
            f" (p50 {duration(job['p50'])})" if job.get("p50") else "")
    words = [f"{CLASS_WORDS[job['class']]:<15} {head} · waited {duration(job.get('waited'))}", f"  why: {job['reason']}"]
    for note in job.get("notes", []):
        words.append(f"  note: {note}")
    for item in job.get("closest", []):
        words.append(f"  closest: {item['runner']} lacks {', '.join(item['missing'])}")
    for item in job.get("offline", []):
        words.append(f"  offline: {item['runner']} — {item['reason']}")
    for item in job.get("busy", []):
        words.append(f"  busy: {item['runner']} running {item.get('job')}")
    return "\n".join(words)


def why(snapshot: dict, *, pr: str | None = None, run: int | None = None, task: int | None = None) -> str:
    jobs = snapshot["jobs"]
    selected = []
    for job in jobs["waiting"] + jobs["blocked"] + jobs["running"]:
        if pr and (job.get("pr") or "").lstrip("#") != str(pr).lstrip("#"):
            continue
        if run and run not in (job.get("run_index"), job.get("run_id")):
            continue
        if task and job.get("task_id") != task:
            continue
        selected.append(job)
    if not selected:
        return "nothing running, waiting or blocked matches"
    return "\n".join(job_line(job) for job in selected)


def status(snapshot: dict) -> str:
    counts = snapshot["counts"]
    lines = [
        f"runners {counts['online']}/{counts['registered']} online · {counts['busy']} busy · "
        f"jobs {counts['running']} running, {counts['waiting']} waiting, {counts['blocked']} blocked · "
        f"oldest wait {duration(snapshot['oldest_wait'])}",
    ]
    for alert in snapshot["alerts"]:
        if alert["kind"] == "unsatisfiable":
            lines.append(f"CRITICAL {alert['count']} job(s) can never start: no runner carries {', '.join(alert['labels'])}")
        elif alert["kind"] == "pool_offline":
            lines.append(f"WARNING pool {alert['pool']} has 0 of {alert['registered']} runners online")
        elif alert["kind"] == "pool_degraded":
            lines.append(f"WARNING pool {alert['pool']} has {alert['online']} of {alert['registered']} online")
    for job in snapshot["jobs"]["waiting"]:
        lines.append(job_line(job))
    return "\n".join(lines)


def drift(snapshot: dict) -> str:
    if not snapshot["drift"]:
        return "roster and live fleet agree"
    lines = []
    for finding in snapshot["drift"]:
        lines.append(f"{finding['severity'].upper():<8} {finding['title']}")
        for runner in finding.get("runners", []):
            lines.append(f"  {runner if isinstance(runner, str) else runner}")
        lines.append(f"  fix: {finding['fix']}")
    return "\n".join(lines)
