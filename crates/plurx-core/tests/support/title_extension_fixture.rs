//! Exact published title-extension witnesses shared by historical test fixtures.
#![allow(dead_code)]

pub(crate) const TITLE_FIXTURE_SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'dv_title_jobs_*' AND sql IS NOT NULL ORDER BY name";

pub(crate) fn require_fixture_marker(connection: &rusqlite::Connection, table: &str, version: i64) {
    let counts: (i64, i64) = connection
        .query_row(
            &format!("SELECT count(*),COALESCE(sum(singleton=1 AND version=?1),0) FROM {table}"),
            [version],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("fixture marker inventory");
    assert_eq!(
        counts,
        (1, 1),
        "one exact published marker required for {table}"
    );
}

/// Canonical read-only removal plan shared by SQLite and replicated fixtures.
#[allow(dead_code)]
pub(crate) fn validated_empty_title_rewind(
    actual: &[(String, String)],
    marker_version: i64,
    marker: i64,
    retained: i64,
) -> Vec<String> {
    let v1 = title_fixture_shape(1);
    let v2 = title_fixture_shape(2);
    assert!(
        (marker_version == 1 && actual == v1) || (marker_version == 2 && actual == v2),
        "exact published title shape and matching generation required"
    );
    assert_eq!(marker, 1, "current title marker required");
    assert_eq!(
        retained, 0,
        "title demand, accepted work and aggregates cannot be rewound"
    );
    let mut drops = actual
        .iter()
        .filter(|(_, sql)| sql.starts_with("CREATE TRIGGER"))
        .map(|(name, _)| format!("DROP TRIGGER {name}"))
        .collect::<Vec<_>>();
    drops.push("DROP TABLE dv_title_jobs_demand".into());
    drops.push("DROP TABLE dv_title_jobs_schema".into());
    drops
}

pub(crate) fn title_fixture_shape(version: i64) -> Vec<(String, String)> {
    assert!(
        matches!(version, 1 | 2),
        "published title generation required"
    );
    let expected = rusqlite::Connection::open_in_memory().expect("title generation witness");
    expected.execute_batch("CREATE TABLE files(id INTEGER PRIMARY KEY,path TEXT,size INTEGER,mtime INTEGER,video_codec TEXT,dv_profile INTEGER,dv_rpu_present INTEGER,probe_json TEXT,scanned_at INTEGER); CREATE TABLE background_jobs(id TEXT,kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT,owner_node_id TEXT,owner_boot_id TEXT,claim_id TEXT,lease_expires_ms INTEGER,result_ref TEXT,updated_at_ms INTEGER); CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT); CREATE TABLE background_job_waiters(job_id TEXT,state TEXT,result_ref TEXT,updated_at_ms INTEGER,consumer_kind TEXT,consumer_ref TEXT);").expect("title witness dependencies");
    expected
        .execute_batch(include_str!("../../src/store/dv_title_jobs.sql"))
        .expect("published title v1");
    if version == 2 {
        expected
            .execute_batch(include_str!("../../src/store/dv_title_jobs_v2.sql"))
            .expect("published title v2");
    }
    title_fixture_objects(&expected)
}

pub(crate) fn title_fixture_objects(connection: &rusqlite::Connection) -> Vec<(String, String)> {
    connection
        .prepare(TITLE_FIXTURE_SHAPE_SQL)
        .expect("title object inventory")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("title object rows")
        .collect::<Result<_, _>>()
        .expect("complete title objects")
}

pub(crate) fn assert_title_fixture_generation(connection: &rusqlite::Connection, version: i64) {
    assert_eq!(
        title_fixture_objects(connection),
        title_fixture_shape(version),
        "exact historical title topology required"
    );
    require_fixture_marker(connection, "dv_title_jobs_schema", version);
}

/// Restore only v132's replaced objects from the immutable v114 witness.
/// Demand rows and their AUTOINCREMENT history are never dropped or reseeded.
pub(crate) fn published_title_v1_restore_plan() -> String {
    let witness = title_fixture_shape(1);
    let mut plan = Vec::new();
    for name in [
        "dv_title_jobs_terminal",
        "dv_title_jobs_publish",
        "dv_title_jobs_schema",
    ] {
        let sql = &witness
            .iter()
            .find(|(object, _)| object == name)
            .expect("published v1 object")
            .1;
        let kind = if name == "dv_title_jobs_schema" {
            "TABLE"
        } else {
            "TRIGGER"
        };
        plan.push(format!("DROP {kind} {name};"));
        plan.push(format!("{};", sql.trim_end_matches(';')));
    }
    plan.push("INSERT INTO dv_title_jobs_schema(singleton,version) VALUES(1,1);".into());
    plan.join("\n")
}
