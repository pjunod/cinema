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
    conn.execute_batch("DROP TRIGGER background_dv_refit_attempt_end")
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

/// The attempt-end trigger, on the real catalogue: staged rows of an attempt
/// go when it stops running or changes claim; a published claim's stay.
#[test]
fn an_ending_attempt_takes_its_staged_scenes_and_a_published_one_keeps_them() {
    let conn = rusqlite::Connection::open_in_memory().expect("catalogue");
    super::super::sqlite::SqliteStore::migrate(&conn).expect("migrate");
    conn.execute_batch(
        "INSERT INTO libraries(id,name,kind,paths) VALUES(1,'l','movies','[]');
         INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(1,1,'movie','t','t');
         INSERT INTO files(id,item_id,path,size,mtime) VALUES(1,1,'/a.mkv',10,1);",
    )
    .expect("file");
    let job = |id: &str, claim: &str| {
        conn.execute(
            "INSERT INTO background_jobs(id,kind,payload_version,payload_json,dedupe_key,priority,state,not_before_ms,created_at_ms,updated_at_ms,claim_id,owner_node_id,owner_boot_id,fence,revision,lease_expires_ms)
             VALUES(?1,'dv_refit',1,'{\"file_id\":1}',?1,0,'running',1,1,1,?2,'n','b',1,1,99999999999)",
            [id, claim],
        )
        .expect("running attempt");
        conn.execute(
            "INSERT INTO background_dv_refit_scenes(claim_id,scene_index,file_id,first_frame,frames,first_pts,end_pts,time_base,fit_json,held_out_json,curves_json,reason)
             VALUES(?1,0,1,0,48,0,2002,'1/1000','{}','{}','{}',NULL)",
            [claim],
        )
        .expect("staged scene");
    };
    let staged = |claim: &str| -> i64 {
        conn.query_row(
            "SELECT count(*) FROM background_dv_refit_scenes WHERE claim_id=?1",
            [claim],
            |row| row.get(0),
        )
        .expect("count")
    };
    job("job-cancel", "claim-cancel");
    conn.execute(
        "UPDATE background_jobs SET state='cancelling' WHERE id='job-cancel'",
        [],
    )
    .expect("cancel");
    assert_eq!(staged("claim-cancel"), 0, "cancel");
    job("job-lost", "claim-lost");
    conn.execute(
        "UPDATE background_jobs SET claim_id='claim-next',fence=2 WHERE id='job-lost'",
        [],
    )
    .expect("reclaimed after a lost lease");
    assert_eq!(staged("claim-lost"), 0, "a new claim");
    job("job-kept", "claim-kept");
    conn.execute(
        "INSERT INTO background_dv_refit_titles(file_id,claim_id,job_id,source_size,source_mtime,probe_revision,fit_version,helper_digest,curve_digest,verdict,reason,gain,scene_count,frames,created_at_ms)
         VALUES(1,'claim-kept','job-kept',10,1,1,'v',?1,?1,'fitted',NULL,0.5,1,48,1)",
        ["a".repeat(64)],
    )
    .expect("published title");
    conn.execute(
        "UPDATE background_jobs SET state='succeeded',claim_id=NULL,owner_node_id=NULL,
         owner_boot_id=NULL,lease_expires_ms=NULL WHERE id='job-kept'",
        [],
    )
    .expect("settled");
    assert_eq!(staged("claim-kept"), 1, "published scenes stay");
    conn.execute("DELETE FROM background_jobs WHERE id='job-kept'", [])
        .expect("retention");
    assert_eq!(staged("claim-kept"), 1, "retention keeps a published fit");
    conn.execute("UPDATE files SET size=11 WHERE id=1", [])
        .expect("source change");
    assert_eq!(staged("claim-kept"), 0, "a changed source keeps nothing");
    let titles: i64 = conn
        .query_row(
            "SELECT count(*) FROM background_dv_refit_titles",
            [],
            |row| row.get(0),
        )
        .expect("titles");
    assert_eq!(titles, 0);
}
