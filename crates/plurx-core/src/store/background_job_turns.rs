//! Short-lived, node-verified library demand. The triggers arbitrate on the
//! existing source grant, including grants from older nodes during rollout.
use super::background_jobs::{encode, identifier, LibraryTurnCapacity, QueueSql};
use crate::error::StoreError;

pub(super) const SCHEMA: &str = include_str!("background_job_turns.sql");
pub(super) const LIVE_TITLE_MIGRATION: &str = include_str!("background_job_turns_v2.sql");
pub(super) const CATALOG: &str =
    "SELECT name, sql FROM sqlite_master WHERE name GLOB 'dispatch_turn_*' ORDER BY name";

pub(super) fn catalog(conn: &rusqlite::Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut statement = conn.prepare(CATALOG)?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(super) fn expected_catalog(live_titles: bool) -> Result<Vec<(String, String)>, StoreError> {
    let conn = rusqlite::Connection::open_in_memory()?;
    conn.execute_batch(
        "CREATE TABLE background_jobs(id TEXT);
        CREATE TABLE analysis_requests(request_id TEXT);
        CREATE TABLE background_source_custody(custody_id TEXT);",
    )?;
    conn.execute_batch(SCHEMA)?;
    if live_titles {
        conn.execute_batch(LIVE_TITLE_MIGRATION)?;
    }
    catalog(&conn)
}

pub(super) fn verify_catalog(
    conn: &rusqlite::Connection,
    live_titles: bool,
) -> Result<(), StoreError> {
    if catalog(conn)? != expected_catalog(live_titles)? {
        return Err(StoreError::Migration(
            "incompatible dispatch turn schema".into(),
        ));
    }
    Ok(())
}

pub(super) async fn advertise<T: QueueSql>(
    store: &T,
    job_id: &str,
    revision: i64,
    node: &str,
    roots: &[std::path::PathBuf],
    capacity: LibraryTurnCapacity,
    now_ms: i64,
) -> Result<bool, StoreError> {
    if !identifier(node)
        || uuid::Uuid::parse_str(job_id).is_err()
        || revision < 0
        || !(0..=i64::MAX - 7000).contains(&now_ms)
    {
        return Err(StoreError::Task(
            "invalid library dispatch advertisement".into(),
        ));
    }
    let rows = store.queue_sql(r#"INSERT INTO dispatch_turn_ready(job_id,node_id,revision,expires_at_ms,roots)
        SELECT j.id,json_extract($1,'$.node'),j.revision,json_extract($1,'$.now') + CASE
          WHEN json_extract($1,'$.capacity')='available' THEN 7000
          -- The busy node may keep the next turn warm only while it owns a
          -- current ordinary source fence on an overlapping disk. The grant
          -- still checks physical occupancy. At release this gives the worker
          -- four seconds (one three-second discovery tick plus a poll) to
          -- acquire the local lane, without pinning other workers indefinitely.
          WHEN json_extract($1,'$.capacity')='heavy_occupied' AND EXISTS (
            SELECT 1 FROM json_each(l.paths) root
            LEFT JOIN background_storage_domains m ON m.library_id=l.id AND m.root_path=root.value
            JOIN (
                SELECT h.resource_key FROM background_job_reservations h
                JOIN background_jobs owner ON owner.id=h.job_id AND owner.fence=h.fence
                WHERE owner.owner_node_id=json_extract($1,'$.node') AND owner.state='running'
                  AND owner.lease_expires_ms>json_extract($1,'$.now') AND h.expires_at_ms>json_extract($1,'$.now')
                  AND h.resource_key LIKE 'source_io%'
                  AND NOT EXISTS(SELECT 1 FROM background_source_pauses p
                      WHERE p.job_id=owner.id AND p.fence=h.fence AND p.phase='paused')
                  -- Using the foreground slot does not promise a heavy-lane
                  -- handoff. When uncertain, wait for local capacity instead.
                  AND NOT EXISTS(SELECT 1 FROM background_job_waiters v WHERE v.job_id=owner.id
                    AND v.state IN ('pending','awaiting_hydration') AND (v.deadline_ms IS NULL OR v.deadline_ms>json_extract($1,'$.now'))
                    AND (v.consumer_kind IN ('playback_fragment','background_delivery','subtitle_translation','subtitle_translation_range')
                      OR (owner.kind='dv_title_scan' AND v.consumer_kind='playback_qualification' AND v.deadline_ms IS NOT NULL)
                      OR (v.consumer_kind='subtitle_source' AND EXISTS(SELECT 1 FROM analysis_requests sub
                        WHERE sub.request_id=v.consumer_ref AND sub.trigger='playback'))))
                UNION ALL
                SELECT h.resource_key FROM analysis_source_reservations h
                JOIN analysis_requests owner ON owner.request_id=h.request_id AND owner.fence=h.fence
                WHERE owner.owner_node_id=json_extract($1,'$.node') AND owner.state='running'
                  AND owner.cancel_requested=0 AND owner.lease_expires_ms>json_extract($1,'$.now')
                  AND h.expires_at_ms>json_extract($1,'$.now') AND h.resource_key LIKE 'source_io%'
                  AND NOT EXISTS(SELECT 1 FROM background_job_waiters v WHERE v.job_id=owner.request_id
                    AND v.request_scope='playback-analysis' AND v.state='pending' AND v.deadline_ms>json_extract($1,'$.now'))
            ) held ON m.domain_id IS NULL OR held.resource_key='source_io' OR held.resource_key='source_io:'||m.domain_id
          ) THEN 4000 ELSE 0 END,l.paths
        FROM background_jobs j JOIN libraries l ON l.id=json_extract(j.payload_json,'$.library_id')
        WHERE j.id=json_extract($1,'$.id') AND j.revision=json_extract($1,'$.revision')
        AND json(l.paths)=json_extract($1,'$.roots') AND json_array_length(l.paths)>0
        AND j.state='queued' AND j.kind IN ('library_scan','metadata_refresh')
        AND (j.target_node_id IS NULL OR j.target_node_id=json_extract($1,'$.node'))
        AND j.not_before_ms<=json_extract($1,'$.now') AND j.failed_attempts<j.attempt_limit
        AND (j.retry_deadline_ms=0 OR j.retry_deadline_ms>json_extract($1,'$.now'))
        AND NOT EXISTS (SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||json_extract($1,'$.node'))
        AND EXISTS(SELECT 1 FROM dispatch_turn_interest i WHERE i.job_id=j.id
          AND (i.deadline_ms IS NULL OR i.deadline_ms>json_extract($1,'$.now')))
        ON CONFLICT(job_id,node_id) DO UPDATE SET revision=excluded.revision,expires_at_ms=excluded.expires_at_ms,roots=excluded.roots
        RETURNING 'true' AS result_json"#.into(), encode(&serde_json::json!({"id":job_id,"revision":revision,"node":node,"now":now_ms,"roots":roots,"capacity":capacity}))?, true, true).await?;
    Ok(!rows.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SqliteStore;

    #[test]
    fn finite_playback_qualification_precedes_an_admissible_library_turn() {
        let db = rusqlite::Connection::open_in_memory().expect("fixture");
        SqliteStore::apply_migrations_for_test(&db, 135).expect("deployed schema");
        db.execute_batch(r#"
            INSERT INTO libraries(id,name,kind,paths) VALUES(1,'Movies','movies','["/media"]');
            INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(1,1,'movie','Movie','Movie');
            INSERT INTO files(id,item_id,path,size,mtime) VALUES(1,1,'/media/movie',10,20);
            INSERT INTO dv_title_jobs_demand(revision,file_id,scanner_digest) VALUES(1,1,'scanner');
            INSERT INTO background_jobs(id,kind,payload_version,payload_json,dedupe_key,priority,state,not_before_ms,created_at_ms,updated_at_ms)
            VALUES('title','dv_title_scan',1,'{"kind":"dv_title_scan","file_id":1,"source_size":10,"source_mtime":20,"probe_revision":1,"scanner_digest":"scanner"}','title',0,'queued',0,0,0),
                  ('manual','library_scan',1,'{"kind":"library_scan","library_id":1}','manual',0,'queued',0,0,0);
            INSERT INTO background_job_waiters(request_scope,request_id,request_digest,job_id,consumer_kind,consumer_ref,priority,state,receipt_expires_ms,created_at_ms,updated_at_ms,deadline_ms)
            VALUES('library','scan','digest','manual','library_work','1',0,'pending',9999,0,0,NULL),
                  ('playback-qualification','viewer','digest','title','playback_qualification','recipe',3,'pending',9999,0,0,2000),
                  ('title-maintenance','keep','digest','title','dv_title_scan','1',0,'pending',9999,0,0,NULL);
            INSERT INTO background_library_requests(request_id,library_id,job_id,input_json) VALUES('scan',1,'manual','{"kind":"full","trigger":"manual"}');
            INSERT INTO dispatch_turn_ready VALUES('manual','node',0,2000,'["/media"]');
        "#).expect("competing title and manual request");
        let claim = || {
            db.execute(crate::store::background_jobs::CLAIM_SQL, [
            r#"{"job_id":"title","expected_revision":0,"node_id":"node","boot_id":"boot","claim_id":"claim","kind":"dv_title_scan","payload_version":1,"now_ms":1000,"dispatched_at_ms":1000}"#,
        ]).expect("claim title")
        };
        assert_eq!(claim(), 0, "deployed trigger refuses live title");
        let winner: String = db
            .query_row("SELECT winner FROM dispatch_turn_attempt", [], |r| r.get(0))
            .expect("contended turn");
        assert_eq!(winner, "manual");
        SqliteStore::apply_next_migration_for_test(&db).expect("upgrade with pending work");
        verify_catalog(&db, true).expect("exact upgraded catalog");
        db.execute(
            "UPDATE background_job_waiters SET deadline_ms=1000 WHERE request_id='viewer'",
            [],
        )
        .expect("expire interest");
        assert_eq!(claim(), 0, "maintenance still waits after viewer expiry");
        db.execute(
            "UPDATE background_job_waiters SET deadline_ms=2000 WHERE request_id='viewer'",
            [],
        )
        .expect("renew interest");
        assert_eq!(claim(), 1, "live title gets the protected turn");
        let held: i64 = db
            .query_row(
                "SELECT count(*) FROM dispatch_turn_protected_jobs WHERE job_id='title'",
                [],
                |r| r.get(0),
            )
            .expect("held classification");
        assert_eq!(held, 1);
    }
}
