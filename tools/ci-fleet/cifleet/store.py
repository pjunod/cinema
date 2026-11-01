"""Bounded SQLite history: task placement, durations, pool load, usage, audit.

Forgejo keeps no ready time, no placement and no resource usage, so the
collector records them. Every table is bounded by constants below; nothing
here grows per commit without a cap.
"""

from __future__ import annotations

import json
import sqlite3
import statistics
import threading
import time

TASK_DAYS = 90
POOL_DAYS = 30
FINE_SECONDS = 24 * 3600  # 5 s usage samples kept this long
COARSE_DAYS = 30  # then one row per minute
ROWS_PER_SERIES = FINE_SECONDS // 5 + COARSE_DAYS * 24 * 60  # hard cap per (scope, key)
AUDIT_ROWS = 10_000

SCHEMA = """
CREATE TABLE IF NOT EXISTS tasks (
  task_id INTEGER PRIMARY KEY, job_id INTEGER, run_id INTEGER, run_index INTEGER,
  workflow TEXT, pr TEXT, name TEXT, runs_on TEXT, status TEXT,
  runner TEXT, runner_uuid TEXT, created INTEGER, started INTEGER, stopped INTEGER, ready INTEGER);
CREATE INDEX IF NOT EXISTS tasks_name ON tasks(name, stopped);
CREATE INDEX IF NOT EXISTS tasks_stopped ON tasks(stopped);
CREATE TABLE IF NOT EXISTS pool_minutes (
  pool TEXT, minute INTEGER, online INTEGER, busy INTEGER, waiting INTEGER, max_wait INTEGER,
  PRIMARY KEY (pool, minute));
CREATE TABLE IF NOT EXISTS usage (
  scope TEXT, key TEXT, ts INTEGER, cpu REAL, mem REAL, extra TEXT);
CREATE INDEX IF NOT EXISTS usage_series ON usage(scope, key, ts);
CREATE TABLE IF NOT EXISTS audit (
  ts INTEGER, actor TEXT, action TEXT, target TEXT, reason TEXT, result TEXT);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);
"""


class Store:
    def __init__(self, path: str):
        self.db = sqlite3.connect(path, check_same_thread=False, isolation_level=None)
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.executescript(SCHEMA)
        self.lock = threading.Lock()

    def close(self) -> None:
        self.db.close()

    def _x(self, sql: str, args=()):
        with self.lock:
            return self.db.execute(sql, args).fetchall()

    # -- meta ------------------------------------------------------------------
    def meta(self, key: str, default=None):
        rows = self._x("SELECT value FROM meta WHERE key=?", (key,))
        return json.loads(rows[0][0]) if rows else default

    def set_meta(self, key: str, value) -> None:
        self._x("INSERT OR REPLACE INTO meta VALUES (?,?)", (key, json.dumps(value)))

    # -- tasks -----------------------------------------------------------------
    def upsert_task(self, task: dict) -> None:
        """Insert or fill a task row; known values are never overwritten by None."""
        columns = ("task_id", "job_id", "run_id", "run_index", "workflow", "pr", "name", "runs_on", "status",
                   "runner", "runner_uuid", "created", "started", "stopped", "ready")
        values = [task.get(column) for column in columns]
        if isinstance(values[7], (list, tuple)):
            values[7] = json.dumps(list(values[7]))
        updates = ", ".join(f"{c}=COALESCE(excluded.{c}, {c})" for c in columns[1:])
        self._x(f"INSERT INTO tasks ({', '.join(columns)}) VALUES ({', '.join('?' * len(columns))}) "
                f"ON CONFLICT(task_id) DO UPDATE SET {updates}", values)

    def task(self, task_id: int) -> dict | None:
        rows = self._x("SELECT * FROM tasks WHERE task_id=?", (task_id,))
        return self._task_row(rows[0]) if rows else None

    def _task_row(self, row) -> dict:
        names = ("task_id", "job_id", "run_id", "run_index", "workflow", "pr", "name", "runs_on", "status",
                 "runner", "runner_uuid", "created", "started", "stopped", "ready")
        record = dict(zip(names, row))
        record["runs_on"] = json.loads(record["runs_on"]) if record["runs_on"] else []
        return record

    def tasks_since(self, since: int, limit: int = 500) -> list[dict]:
        rows = self._x("SELECT * FROM tasks WHERE stopped >= ? ORDER BY stopped DESC LIMIT ?", (since, limit))
        return [self._task_row(row) for row in rows]

    def recent_runs_of(self, name: str, limit: int = 10) -> list[dict]:
        rows = self._x("SELECT * FROM tasks WHERE name=? AND status='success' AND stopped IS NOT NULL "
                       "ORDER BY stopped DESC LIMIT ?", (name, limit))
        return [self._task_row(row) for row in rows]

    def unplaced(self, limit: int) -> list[dict]:
        rows = self._x("SELECT * FROM tasks WHERE runner IS NULL AND job_id IS NOT NULL AND status != 'skipped' "
                       "ORDER BY task_id DESC LIMIT ?", (limit,))
        return [self._task_row(row) for row in rows]

    def durations(self, since: int) -> dict[str, dict]:
        """p50/p95 run time per job name over successful, really-run tasks."""
        rows = self._x("SELECT name, stopped - started FROM tasks WHERE status='success' AND started IS NOT NULL "
                       "AND stopped >= ? AND stopped > started", (since,))
        grouped: dict[str, list[int]] = {}
        for name, seconds in rows:
            grouped.setdefault(name, []).append(seconds)
        return {name: _percentiles(values) for name, values in grouped.items()}

    def waits_by_pool(self, since: int, pool_of) -> dict[str, dict]:
        rows = self._x("SELECT runs_on, started - ready FROM tasks WHERE ready IS NOT NULL AND started IS NOT NULL "
                       "AND status != 'skipped' AND started >= ?", (since,))
        grouped: dict[str, list[int]] = {}
        for runs_on, seconds in rows:
            grouped.setdefault(pool_of(json.loads(runs_on or "[]")), []).append(max(0, seconds))
        return {pool: _percentiles(values) for pool, values in grouped.items()}

    # -- pools -----------------------------------------------------------------
    def record_pools(self, minute: int, pools: list[dict]) -> None:
        for pool in pools:
            if not pool.get("requested") and not pool.get("waiting"):
                continue
            self._x("INSERT INTO pool_minutes VALUES (?,?,?,?,?,?) ON CONFLICT(pool, minute) DO UPDATE SET "
                    "online=MIN(online, excluded.online), busy=MAX(busy, excluded.busy), "
                    "waiting=MAX(waiting, excluded.waiting), max_wait=MAX(max_wait, excluded.max_wait)",
                    (pool["name"], minute, pool["online"], pool["busy"], pool["waiting"], pool["oldest_wait"]))

    def pool_hours(self, since: int) -> dict[str, list[dict]]:
        rows = self._x("SELECT pool, minute / 60 AS hour, AVG(CASE WHEN online > 0 THEN 1.0 * busy / online END), "
                       "MAX(max_wait), MAX(waiting) FROM pool_minutes WHERE minute >= ? GROUP BY pool, hour "
                       "ORDER BY hour", (since // 60,))
        out: dict[str, list[dict]] = {}
        for pool, hour, busy, max_wait, waiting in rows:
            out.setdefault(pool, []).append({"hour": hour * 3600, "busy": round(busy or 0, 3),
                                             "max_wait": max_wait or 0, "waiting": waiting or 0})
        return out

    # -- usage samples ---------------------------------------------------------
    def record_usage(self, scope: str, key: str, ts: int, cpu: float | None, mem: float | None, extra=None) -> None:
        self._x("INSERT INTO usage VALUES (?,?,?,?,?,?)",
                (scope, key, ts, cpu, mem, json.dumps(extra) if extra is not None else None))

    def series(self, scope: str, key: str, since: int = 0) -> list[dict]:
        rows = self._x("SELECT ts, cpu, mem, extra FROM usage WHERE scope=? AND key=? AND ts >= ? ORDER BY ts",
                       (scope, key, since))
        return [{"ts": ts, "cpu": cpu, "mem": mem, **(json.loads(extra) if extra else {})} for ts, cpu, mem, extra in rows]

    # -- audit -----------------------------------------------------------------
    def audit(self, actor: str, action: str, target: str, reason: str, result: str) -> None:
        self._x("INSERT INTO audit VALUES (?,?,?,?,?,?)", (int(time.time()), actor, action, target, reason, result))

    def audit_log(self, limit: int = 100) -> list[dict]:
        rows = self._x("SELECT * FROM audit ORDER BY ts DESC LIMIT ?", (limit,))
        return [dict(zip(("ts", "actor", "action", "target", "reason", "result"), row)) for row in rows]

    # -- bounds ----------------------------------------------------------------
    def maintain(self, now: int) -> None:
        """Downsample, then enforce every retention bound. Runs hourly."""
        cutoff = now - FINE_SECONDS
        with self.lock:
            db = self.db
            db.execute("BEGIN")
            db.execute(
                "INSERT INTO usage SELECT scope, key, (ts / 60) * 60 - 1, AVG(cpu), MAX(mem), NULL FROM usage "
                "WHERE ts < ? AND ts % 60 != 59 GROUP BY scope, key, ts / 60", (cutoff,))
            db.execute("DELETE FROM usage WHERE ts < ? AND ts % 60 != 59", (cutoff,))
            db.execute("DELETE FROM usage WHERE ts < ?", (now - COARSE_DAYS * 86400,))
            db.execute("DELETE FROM pool_minutes WHERE minute < ?", ((now - POOL_DAYS * 86400) // 60,))
            db.execute("DELETE FROM tasks WHERE COALESCE(stopped, created, 0) < ? AND stopped IS NOT NULL",
                       (now - TASK_DAYS * 86400,))
            db.execute("DELETE FROM audit WHERE rowid NOT IN (SELECT rowid FROM audit ORDER BY ts DESC LIMIT ?)",
                       (AUDIT_ROWS,))
            for scope, key, count in db.execute(
                    "SELECT scope, key, COUNT(*) FROM usage GROUP BY scope, key HAVING COUNT(*) > ?",
                    (ROWS_PER_SERIES,)).fetchall():
                db.execute("DELETE FROM usage WHERE rowid IN (SELECT rowid FROM usage WHERE scope=? AND key=? "
                           "ORDER BY ts LIMIT ?)", (scope, key, count - ROWS_PER_SERIES))
            db.execute("COMMIT")


def _percentiles(values: list[int]) -> dict:
    ordered = sorted(values)
    if not ordered:
        return {"n": 0}
    p95 = ordered[min(len(ordered) - 1, int(round(0.95 * (len(ordered) - 1))))]
    return {"n": len(ordered), "p50": int(statistics.median(ordered)), "p95": int(p95)}
