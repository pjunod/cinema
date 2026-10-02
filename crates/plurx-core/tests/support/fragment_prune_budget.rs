//! Test the shipped statements on schemas supplied by the full backend chains.
use rusqlite::{params, Connection, StatementStatus};

fn work(conn: &Connection, sql: &str, select: bool) -> i32 {
    let mut statement = conn.prepare(sql).expect("production prune statement");
    if select {
        let rows = statement
            .query_map(params![100, 128], |row| row.get::<_, String>(0))
            .expect("candidate SELECT")
            .collect::<Result<Vec<_>, _>>()
            .expect("candidates");
        assert!(rows.is_empty(), "current failed sources are retained");
    } else {
        assert_eq!(
            statement
                .execute(params![100, 128])
                .expect("terminal DELETE"),
            0
        );
    }
    statement.get_status(StatementStatus::VmStep)
}

pub fn assert_plans_and_work(conn: &Connection, candidates: &str, terminal: &str) {
    let index: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'analysis_requests_result_target_force'",
            [],
            |row| row.get(0),
        )
        .expect("full migration chain retained lookup index");
    assert!(
        !index.to_uppercase().contains(" WHERE "),
        "lookup includes empty keys"
    );
    for sql in [candidates, terminal] {
        let plan = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .expect("plan")
            .query_map(params![100, 128], |row| row.get::<_, String>(3))
            .expect("plan rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("plan details")
            .join("\n");
        assert!(plan.contains("SEARCH request USING COVERING INDEX analysis_requests_result_target_force (result_cache_key=? AND target_node_id=? AND force_rebuild=?)"), "{plan}");
        assert!(plan.contains("SEARCH active_request USING INDEX analysis_requests_result_target_force (result_cache_key=?)"), "{plan}");
    }
    conn.execute_batch("BEGIN").expect("rollback-only workload");
    let populated: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM libraries WHERE id = 991)",
            [],
            |row| row.get(0),
        )
        .expect("upgrade workload presence");
    if !populated {
        conn.execute_batch(include_str!("../fixtures/fragment-prune-worst.sql"))
            .expect("retained-failure workload");
    }
    conn.execute_batch("ANALYZE").expect("planner statistics");
    let mut fixed = Vec::new();
    for (sql, select) in [(candidates, true), (terminal, false)] {
        let steps = work(conn, sql, select);
        assert!(
            steps < 4000 * 150,
            "indexed work must grow by rows, not row pairs: {steps}"
        );
        fixed.push(steps);
    }
    conn.execute_batch("DROP INDEX analysis_requests_result_target_force")
        .expect("baseline fixture only");
    for ((sql, select), steps) in [(candidates, true), (terminal, false)]
        .into_iter()
        .zip(fixed)
    {
        let baseline = work(conn, sql, select);
        assert!(
            baseline > steps * 10,
            "fixture must reproduce the defect: baseline={baseline}, indexed={steps}"
        );
        eprintln!(
            "fragment prune {}: indexed={steps} baseline={baseline} VM steps",
            if select { "SELECT" } else { "DELETE" }
        );
    }
    let started = std::time::Instant::now();
    conn.execute_batch("CREATE INDEX analysis_requests_result_target_force ON analysis_requests(result_cache_key,target_node_id,force_rebuild)").expect("populated upgrade index build");
    eprintln!(
        "fragment request index build over 4000 rows: {:?}",
        started.elapsed()
    );
    conn.execute_batch("ROLLBACK")
        .expect("discard workload and baseline DDL");
}
