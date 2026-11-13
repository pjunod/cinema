use super::*;

#[test]
fn refit_schema_installs_whole_and_refuses_a_partial_shape() {
    let conn = rusqlite::Connection::open_in_memory().expect("fixture");
    conn.execute_batch(DEPENDENCIES).expect("dependencies");
    assert_eq!(version(&rows(&conn).expect("rows")).expect("absent"), None);
    conn.execute_batch(SCHEMA).expect("install");
    verify_sqlite(&conn).expect("complete");
    #[cfg(feature = "hiqlite-store")]
    for sql in statements() {
        conn.execute_batch(&sql)
            .expect("idempotent replica statement");
    }
    verify_sqlite(&conn).expect("same schema");
    conn.execute_batch("DROP TRIGGER background_dv_refit_terminal")
        .expect("partial fixture");
    assert!(verify_sqlite(&conn).is_err());
}

#[test]
fn a_migrated_catalogue_carries_the_exact_extension() {
    let conn = rusqlite::Connection::open_in_memory().expect("catalogue");
    super::super::sqlite::SqliteStore::migrate(&conn).expect("migrate");
    verify_sqlite(&conn).expect("v130 installs the F+ extension");
    assert_eq!(
        super::super::sqlite::MIGRATIONS[SQLITE_INTRODUCED_SCHEMA as usize - 1],
        SCHEMA
    );
}

#[test]
fn time_bases_and_fit_versions_are_bounded_identifiers() {
    for good in ["1/24000", "1001/24000", "1/90000"] {
        assert!(valid_time_base(good), "{good}");
    }
    for bad in [
        "",
        "1",
        "0/1",
        "1/0",
        "01/2",
        "1/2/3",
        "-1/2",
        "1/99999999999",
        "a/b",
    ] {
        assert!(!valid_time_base(bad), "{bad}");
    }
    assert!(valid_fit_version(crate::transcode::dv_refit::FIT_VERSION));
    assert!(!valid_fit_version("has space"));
}

#[test]
fn error_numbers_round_trip_as_decimal_text() {
    let mut x = 0.1_f64;
    for _ in 0..10_000 {
        x = (x * 1.618_033_988_749_895 + 0.000_123_456_789).fract() * 3.7e9 - 1.2e9;
        for v in [x, x * 1e-300, x * 1e290, 1.0 / x] {
            let text = decimal::text(v);
            assert!(text.len() <= 64, "{text}");
            assert_eq!(decimal::parse(&text).map(f64::to_bits), Some(v.to_bits()));
            // Through JSON and back, as a request and a stored TEXT column.
            let json = serde_json::to_string(&serde_json::json!({ "v": text })).expect("json");
            let back: serde_json::Value = serde_json::from_str(&json).expect("json");
            assert_eq!(
                decimal::parse(back["v"].as_str().expect("text")).map(f64::to_bits),
                Some(v.to_bits())
            );
        }
    }
    for bad in ["", "nan", "inf", "-inf", "1e999", &"1".repeat(65)] {
        assert_eq!(decimal::parse(bad), None, "{bad}");
    }
}

fn catalogue() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("catalogue");
    super::super::sqlite::SqliteStore::migrate(&conn).expect("migrate");
    conn.execute_batch(
        "INSERT INTO libraries(id,name,kind,paths) VALUES(1,'l','movies','[]');
         INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(1,1,'movie','t','t');
         INSERT INTO files(id,item_id,path,size,mtime) VALUES(1,1,'/a.mkv',10,1);",
    )
    .expect("file");
    conn
}

fn job(conn: &rusqlite::Connection, id: &str, identity: &str, state: &str, size: i64) {
    let payload = serde_json::json!({"file_id":1,"source_size":size,"source_mtime":1,"video_index":0,
        "probe_revision":1,"helper_identity":"a".repeat(64),"fit_version":"v"})
    .to_string();
    let owned = matches!(state, "running" | "cancelling");
    conn.execute(
        "INSERT INTO background_jobs(id,kind,payload_version,payload_json,dedupe_key,priority,state,not_before_ms,created_at_ms,updated_at_ms,revision,
          owner_node_id,owner_boot_id,claim_id,fence,lease_expires_ms)
         VALUES(?1,'dv_refit',1,?2,?3,0,?4,1,1,1,1,
          CASE WHEN ?5 THEN 'n' END,CASE WHEN ?5 THEN 'b' END,CASE WHEN ?5 THEN ?1 END,1,CASE WHEN ?5 THEN 99999999999 END)",
        rusqlite::params![id, payload, identity, state, owned],
    )
    .expect("job");
}

fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).expect("count")
}

/// Decision E: a failed or cancelled identity stays terminal after its job
/// row goes, and only a new job for the identity (a retry) lifts it.
#[test]
fn a_terminal_outcome_outlives_its_job_and_a_retry_lifts_it() {
    let conn = catalogue();
    job(&conn, "job-a", "dv-refit:a", "queued", 10);
    conn.execute("UPDATE background_jobs SET state='cancelled',last_error_code=NULL WHERE id='job-a'", [])
        .expect("user cancel");
    let outcome = |identity: &str| -> Option<String> {
        conn.query_row(
            "SELECT state FROM background_dv_refit_outcomes WHERE identity=?1",
            [identity],
            |row| row.get(0),
        )
        .ok()
    };
    assert_eq!(outcome("dv-refit:a").as_deref(), Some("cancelled"));
    conn.execute("DELETE FROM background_jobs WHERE id='job-a'", [])
        .expect("retention prunes the job row");
    assert_eq!(outcome("dv-refit:a").as_deref(), Some("cancelled"), "outlives the row");
    job(&conn, "job-b", "dv-refit:b", "running", 10);
    conn.execute("UPDATE background_jobs SET state='failed',last_error_code='refit_failed',owner_node_id=NULL,owner_boot_id=NULL,claim_id=NULL,lease_expires_ms=NULL WHERE id='job-b'", [])
        .expect("attempts exhausted");
    assert_eq!(outcome("dv-refit:b").as_deref(), Some("failed"));
    let row: (i64, i64, String) = conn
        .query_row(
            "SELECT video_index,probe_revision,error_code FROM background_dv_refit_outcomes WHERE identity='dv-refit:b'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("identity recorded");
    assert_eq!(row, (0, 1, "refit_failed".into()));
    job(&conn, "job-a2", "dv-refit:a", "queued", 10);
    assert_eq!(outcome("dv-refit:a"), None, "a retry of the identity lifts it");
    assert_eq!(outcome("dv-refit:b").as_deref(), Some("failed"), "only that identity");
    // A yield or a retry back to queued is not terminal.
    job(&conn, "job-c", "dv-refit:c", "running", 10);
    conn.execute("UPDATE background_jobs SET state='queued',owner_node_id=NULL,owner_boot_id=NULL,claim_id=NULL,lease_expires_ms=NULL WHERE id='job-c'", [])
        .expect("yield");
    assert_eq!(outcome("dv-refit:c"), None);
}

/// Decision H: a changed source supersedes the file's jobs and drops its
/// tables and outcomes; a path change alone does not.
#[test]
fn a_changed_source_supersedes_its_jobs_and_keeps_nothing() {
    let conn = catalogue();
    job(&conn, "job-q", "dv-refit:q", "queued", 10);
    job(&conn, "job-r", "dv-refit:r", "running", 10);
    conn.execute(
        "INSERT INTO background_dv_refit_titles(file_id,curve_digest,job_id,source_size,source_mtime,video_index,probe_revision,helper_identity,fit_version,verdict,reason,gain,scene_count,frames,created_at_ms)
         VALUES(1,?1,'job-x',10,1,0,1,?1,'v','identity','r',NULL,0,0,1)",
        ["a".repeat(64)],
    )
    .expect("table");
    conn.execute("UPDATE files SET path='/b.mkv' WHERE id=1", [])
        .expect("rename");
    assert_eq!(count(&conn, "SELECT count(*) FROM background_dv_refit_titles"), 1);
    assert_eq!(
        count(&conn, "SELECT count(*) FROM background_jobs WHERE state IN ('queued','running')"),
        2
    );
    conn.execute("UPDATE files SET size=11 WHERE id=1", [])
        .expect("source change");
    let states: Vec<(String, String)> = conn
        .prepare("SELECT state,last_error_code FROM background_jobs ORDER BY id")
        .expect("jobs")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("states");
    assert_eq!(
        states,
        [
            ("cancelled".to_owned(), "source_changed".to_owned()),
            ("cancelling".to_owned(), "source_changed".to_owned())
        ]
    );
    assert_eq!(count(&conn, "SELECT count(*) FROM background_dv_refit_titles"), 0);
    assert_eq!(count(&conn, "SELECT count(*) FROM background_dv_refit_outcomes"), 0);
    // The running attempt settling later records only an identity of the
    // old source, which prune drops.
    conn.execute("UPDATE background_jobs SET state='cancelled',owner_node_id=NULL,owner_boot_id=NULL,claim_id=NULL,lease_expires_ms=NULL WHERE id='job-r'", [])
        .expect("settled");
    assert_eq!(count(&conn, "SELECT count(*) FROM background_dv_refit_outcomes"), 1);
    conn.execute(
        "INSERT INTO background_job_commands(id,operation,request_json,result_json)
         VALUES('p','prune_dv_refit','{\"now_ms\":5}','{}')",
        [],
    )
    .expect("prune");
    assert_eq!(count(&conn, "SELECT count(*) FROM background_dv_refit_outcomes"), 0);
    assert_eq!(count(&conn, "SELECT count(*) FROM background_job_commands"), 0);
    conn.execute("DELETE FROM files WHERE id=1", []).expect("file deleted");
}
