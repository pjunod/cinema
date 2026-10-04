#!/usr/bin/env python3
"""Reproduce the October 1 fragment-index prune cost without writing production.

Requires a read-only-accessible node database and Python linked to SQLite 3.53.2.
The selected tables, indexes and planner statistics are copied into :memory:.
Only that disposable copy receives DDL or DELETE statements. No row data leaves
this process; stdout contains counts, query plans and measurements only.

The SQL is frozen from 3a512c8909ee8c2814ab2004ed724c7b39bdd8ac:
crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs,
prune_cluster_fragment_indexes, final DELETE statement.
"""

import argparse
import hashlib
import json
from pathlib import Path
import sqlite3
import time
from urllib.parse import quote

SQL = "DELETE FROM cluster_fragment_index_jobs\n              WHERE (cache_key, target_node_id) IN (\n                SELECT terminal_job.cache_key, terminal_job.target_node_id\n                  FROM cluster_fragment_index_jobs terminal_job\n                 WHERE (terminal_job.state IN ('ready', 'cancelled') OR (\n                   terminal_job.state = 'failed' AND (\n                     NOT EXISTS (SELECT 1 FROM files current_file\n                       WHERE current_file.id = terminal_job.file_id\n                         AND current_file.size = terminal_job.source_size\n                         AND current_file.mtime = terminal_job.source_mtime)\n                     OR EXISTS (SELECT 1 FROM analysis_requests request\n                       WHERE request.result_cache_key = terminal_job.cache_key\n                         AND request.target_node_id = terminal_job.target_node_id\n                         AND request.force_rebuild = 1))))\n                   AND terminal_job.updated_at_ms < $1\n                   AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts artifact\n                     WHERE artifact.cache_key = terminal_job.cache_key)\n                   AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_heads head\n                     WHERE head.generation_cache_key = terminal_job.cache_key)\n                   AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations location\n                     WHERE location.cache_key = terminal_job.cache_key)\n                   AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_jobs active_job\n                     WHERE active_job.cache_key = terminal_job.cache_key\n                       AND (active_job.state IN ('queued', 'running')\n                         OR active_job.updated_at_ms >= $1))\n                   AND NOT EXISTS (SELECT 1 FROM analysis_requests active_request\n                     WHERE active_request.result_cache_key = terminal_job.cache_key\n                       AND active_request.state IN ('queued', 'running', 'submitted'))\n                 ORDER BY terminal_job.updated_at_ms, terminal_job.cache_key,\n                          terminal_job.target_node_id\n                 LIMIT $2\n              )"


TABLES = [
    "cluster_fragment_index_jobs", "cluster_fragment_index_artifacts",
    "cluster_fragment_index_heads", "cluster_fragment_index_locations",
    "analysis_requests", "files",
]


def emit(record):
    print(json.dumps(record), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", required=True)
    parser.add_argument("--samples", type=int, default=3, choices=range(1, 6))
    args = parser.parse_args()
    if sqlite3.sqlite_version != "3.53.2":
        parser.error("use SQLite 3.53.2 to match the pinned dependency")
    path = quote(str(Path(args.database).resolve()), safe="/")
    source = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    source.execute("BEGIN")
    memory = sqlite3.connect(":memory:")
    memory.execute("PRAGMA foreign_keys=OFF")
    counts = {}
    for table in TABLES:
        schema = source.execute(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name=?", (table,)
        ).fetchone()[0]
        memory.execute(schema)
        rows = source.execute("SELECT * FROM " + table).fetchall()
        counts[table] = len(rows)
        if rows:
            placeholders = ",".join("?" for _ in rows[0])
            memory.executemany(f"INSERT INTO {table} VALUES ({placeholders})", rows)
        for (schema,) in source.execute(
            "SELECT sql FROM sqlite_master "
            "WHERE type='index' AND tbl_name=? AND sql IS NOT NULL", (table,)
        ):
            memory.execute(schema)
    params = {"1": int(time.time() * 1000) - 30 * 86_400_000, "2": 128}
    production_file_plan = list(source.execute("EXPLAIN QUERY PLAN " + SQL, params))
    stats = list(source.execute(
        "SELECT name FROM sqlite_master WHERE name IN ('sqlite_stat1','sqlite_stat4')"
    ))
    memory.execute("ANALYZE")
    statistics_counts = {}
    for (table,) in stats:
        marks = ",".join("?" for _ in TABLES)
        rows = source.execute(f"SELECT * FROM {table} WHERE tbl IN ({marks})", TABLES).fetchall()
        memory.execute("DELETE FROM " + table)
        if rows:
            placeholders = ",".join("?" for _ in rows[0])
            memory.executemany(f"INSERT INTO {table} VALUES ({placeholders})", rows)
        statistics_counts[table] = len(rows)
    memory.execute("ANALYZE sqlite_schema")
    triggers = list(source.execute(
        "SELECT name FROM sqlite_master "
        "WHERE type='trigger' AND tbl_name='cluster_fragment_index_jobs'"
    ))
    source.rollback()
    source.close()
    memory.commit()
    emit({"sqlite": sqlite3.sqlite_version, "counts": counts,
          "compile_options": [row[0] for row in memory.execute("PRAGMA compile_options")],
          "plan_origin": "Reproduction engine: production_file_plan reads the source file; trial plans read the in-memory copy. Neither is exported by the daemon.",
          "cutoff_ms": params["1"], "copied_statistics": statistics_counts,
          "production_file_plan": production_file_plan, "uncopied_job_triggers": triggers,
          "sql_sha256": hashlib.sha256(SQL.encode()).hexdigest()})

    def run(label, statement):
        plan = list(memory.execute("EXPLAIN QUERY PLAN " + statement, params))
        steps = 0
        start = time.perf_counter()

        def progress():
            nonlocal steps
            steps += 10_000
            return int(time.perf_counter() - start > 30)

        memory.set_progress_handler(progress, 10_000)
        try:
            memory.execute("BEGIN")
            cursor = memory.execute(statement, params)
            elapsed = time.perf_counter() - start
            affected = cursor.rowcount
        finally:
            memory.set_progress_handler(None, 0)
            memory.rollback()
        emit({"variant": label, "seconds": elapsed, "deleted": affected,
              "vm_steps_approx": steps, "plan": plan,
              "zero_row_witness": affected == 0})

    for repeat in range(args.samples):
        run(f"baseline_{repeat + 1}", SQL)
    memory.execute(
        "CREATE INDEX repro_requests_result_target_force "
        "ON analysis_requests(result_cache_key,target_node_id,force_rebuild)"
    )
    for repeat in range(args.samples):
        run(f"only_requests_result_target_force_{repeat + 1}", SQL)
    memory.execute("DROP INDEX repro_requests_result_target_force")
    alternative = SQL.replace(
        "WHERE request.result_cache_key = terminal_job.cache_key",
        "WHERE request.result_cache_key <> '' AND "
        "request.result_cache_key = terminal_job.cache_key",
    ).replace(
        "WHERE active_request.result_cache_key = terminal_job.cache_key",
        "WHERE active_request.result_cache_key <> '' AND "
        "active_request.result_cache_key = terminal_job.cache_key",
    )
    for repeat in range(args.samples):
        run(f"explicit_existing_partial_index_predicates_{repeat + 1}", alternative)
    memory.close()


if __name__ == "__main__":
    main()
