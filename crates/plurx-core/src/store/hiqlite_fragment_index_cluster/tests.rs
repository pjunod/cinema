#[test]
fn replicated_v47_new_statements_pass_placeholder_census() {
    for statement in super::SUBTITLE_SOURCE_SCHEMA_STATEMENTS {
        super::super::hiqlite::validate_sql(statement)
            .expect("v47 migration statement uses ordered placeholders");
    }
}

/// Domain adapters no longer reopen execution rows. The shared queue's
/// per-interest ledger supplies both counters and history; runtime coverage
/// is `background_jobs_fragment_retry_keeps_its_window_history_and_configured_limit`.
#[test]
fn every_attempts_reset_resets_the_attempt_history() {
    const SOURCE: &str = include_str!("../hiqlite_fragment_index_cluster.rs");
    let production = SOURCE
        .split_once("\n#[cfg(test)]")
        .expect("test boundary")
        .0;
    assert!(
        !production.contains("cluster_fragment_index_jobs.attempts END"),
        "a domain adapter must not reset the common execution budget"
    );
    let shared = include_str!("../background_jobs_schema.sql");
    assert!(shared.contains("SELECT MAX(interest.failed_attempts"));
    assert!(shared.contains("SELECT interest.attempt_errors"));
}

use rusqlite::Connection;

use super::{
    ANALYSIS_ATTEMPT_ERRORS_STATEMENTS, ANALYSIS_COMPONENT_STATEMENTS,
    ANALYSIS_HISTORY_INDEX_STATEMENTS, ANALYSIS_REQUEST_SCHEMA_STATEMENTS,
    ANALYSIS_TERMINAL_IDENTITY_INDEX_STATEMENTS, FRAGMENT_INDEX_SCHEMA_STATEMENTS,
};
use crate::store::fragment_index_cluster::{
    ANALYSIS_ATTEMPT_ERRORS_SCHEMA, ANALYSIS_COMPONENTS_SCHEMA, ANALYSIS_HISTORY_INDEX_SCHEMA,
    ANALYSIS_REQUESTS_SCHEMA, ANALYSIS_TERMINAL_IDENTITY_INDEX_SCHEMA,
    CLUSTER_FRAGMENT_INDEX_SCHEMA,
};

fn fixture() -> Connection {
    let connection = Connection::open_in_memory().expect("in-memory schema fixture");
    connection
        .execute_batch(
            "CREATE TABLE files (
                    id INTEGER PRIMARY KEY,
                    size INTEGER NOT NULL,
                    mtime INTEGER NOT NULL,
                    scanned_at INTEGER NOT NULL DEFAULT 0
                 ) STRICT;",
        )
        .expect("files prerequisite");
    // The component step widens `timeline_annotation_sets` on its way
    // past, so the comparison needs that table present on both sides.
    connection
        .execute_batch(crate::store::timeline_annotations::TIMELINE_ANNOTATIONS_SCHEMA)
        .expect("timeline annotations prerequisite");
    connection
}

fn schema_objects(connection: &Connection) -> Vec<(String, String, String, String)> {
    let mut statement = connection
        .prepare(
            // Everything the compared steps create or rebuild. The old
            // two patterns missed `analysis_attempts` and
            // `analysis_lifecycle_counters` — a table full of enumerated
            // `reason` literals, which is exactly the shape that drifts —
            // and the widened `timeline_annotation_sets`.
            "SELECT type, name, tbl_name, COALESCE(sql, '')
                   FROM sqlite_master
                  WHERE name LIKE 'cluster_fragment_index_%'
                     OR name LIKE 'analysis_requests%'
                     OR name LIKE 'analysis_attempts%'
                     OR name LIKE 'analysis_lifecycle_counters%'
                     OR name LIKE 'timeline_annotation_%'
                  ORDER BY type, name",
        )
        .expect("schema object query");
    statement
        .query_map([], |row| {
            let sql: String = row.get(3)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                sql.split_whitespace().collect::<Vec<_>>().join(" "),
            ))
        })
        .expect("schema object rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("schema object values")
}

#[test]
fn replicated_migration_statements_match_sqlite_schema_versions() {
    assert_eq!(FRAGMENT_INDEX_SCHEMA_STATEMENTS.len(), 8);
    assert_eq!(ANALYSIS_REQUEST_SCHEMA_STATEMENTS.len(), 7);
    assert_eq!(ANALYSIS_HISTORY_INDEX_STATEMENTS.len(), 2);
    assert_eq!(ANALYSIS_ATTEMPT_ERRORS_STATEMENTS.len(), 1);
    assert_eq!(ANALYSIS_TERMINAL_IDENTITY_INDEX_STATEMENTS.len(), 1);

    let sqlite = fixture();
    sqlite
        .execute_batch(CLUSTER_FRAGMENT_INDEX_SCHEMA)
        .expect("SQLite v30 fragment-index schema");
    sqlite
        .execute_batch(ANALYSIS_REQUESTS_SCHEMA)
        .expect("SQLite v31 analysis-request schema");
    sqlite
        .execute_batch(ANALYSIS_HISTORY_INDEX_SCHEMA)
        .expect("SQLite v33 analysis-history indexes");
    // The component rebuild (v22 replicated / v41 embedded) was never in
    // this comparison, so the two backends' largest divergence — a table
    // rebuild with a data-carrying INSERT — went unchecked.
    sqlite
        .execute_batch(ANALYSIS_COMPONENTS_SCHEMA)
        .expect("SQLite v41 analysis-component schema");
    sqlite
        .execute_batch(ANALYSIS_ATTEMPT_ERRORS_SCHEMA)
        .expect("SQLite v46 attempt-history column");
    sqlite
        .execute_batch(ANALYSIS_TERMINAL_IDENTITY_INDEX_SCHEMA)
        .expect("SQLite v50 terminal-identity index");

    let replicated = fixture();
    for sql in FRAGMENT_INDEX_SCHEMA_STATEMENTS
        .iter()
        .chain(ANALYSIS_REQUEST_SCHEMA_STATEMENTS)
        .chain(ANALYSIS_HISTORY_INDEX_STATEMENTS)
        .chain(ANALYSIS_COMPONENT_STATEMENTS)
        .chain(ANALYSIS_ATTEMPT_ERRORS_STATEMENTS)
        .chain(ANALYSIS_TERMINAL_IDENTITY_INDEX_STATEMENTS)
    {
        replicated
            .execute_batch(sql)
            .expect("replicated migration statement");
    }

    assert_eq!(schema_objects(&replicated), schema_objects(&sqlite));
    // Objects alone would not see a column, and a column is exactly what
    // v26 adds.
    for connection in [&sqlite, &replicated] {
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('cluster_fragment_index_jobs')
                          WHERE name = 'attempt_errors'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("attempt history column"),
            1
        );
    }
}
