//! Additive title-job extension; both stores install and verify the same SQL.
use crate::error::StoreError;
use std::sync::LazyLock;
pub(crate) const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'dv_title_jobs_*' AND sql IS NOT NULL ORDER BY name";
pub(crate) const MARKER_SQL: &str =
    "SELECT count(*) AS count FROM dv_title_jobs_schema WHERE singleton=1 AND version=1";
const DEPENDENCIES: &str = "CREATE TABLE files(id INTEGER PRIMARY KEY,path TEXT,size INTEGER,mtime INTEGER,video_codec TEXT,dv_profile INTEGER,dv_rpu_present INTEGER,probe_json TEXT,scanned_at INTEGER); CREATE TABLE background_jobs(id TEXT,kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT,owner_node_id TEXT,owner_boot_id TEXT,claim_id TEXT,lease_expires_ms INTEGER,result_ref TEXT,updated_at_ms INTEGER); CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT); CREATE TABLE background_job_waiters(job_id TEXT,state TEXT,result_ref TEXT,updated_at_ms INTEGER);";
#[cfg(feature = "hiqlite-store")]
pub(crate) fn statements() -> Vec<String> {
    super::dv_title_jobs::SCHEMA
        .split("-- next statement")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
fn rows(conn: &rusqlite::Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut query = conn.prepare(SHAPE_SQL)?;
    let rows = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}
static SHAPE: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let conn = rusqlite::Connection::open_in_memory().expect("title schema fixture");
    conn.execute_batch(DEPENDENCIES)
        .expect("title schema dependencies");
    conn.execute_batch(super::dv_title_jobs::SCHEMA)
        .expect("title schema");
    rows(&conn).expect("title schema shape")
});
pub(crate) fn version(shape: &[(String, String)]) -> Result<Option<i64>, StoreError> {
    let canonical = |rows: &[(String, String)]| {
        rows.iter()
            .map(|(name, sql)| {
                (
                    name.clone(),
                    sql.split_whitespace().collect::<Vec<_>>().join(" "),
                )
            })
            .collect::<Vec<_>>()
    };
    if shape.is_empty() {
        Ok(None)
    } else if canonical(shape) == canonical(&SHAPE) {
        Ok(Some(1))
    } else {
        Err(StoreError::Migration(
            "title jobs schema partial or incompatible".into(),
        ))
    }
}
pub(crate) fn verify_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    if version(&rows(conn)?)? != Some(1)
        || conn.query_row(MARKER_SQL, [], |row| row.get::<_, i64>(0))? != 1
    {
        return Err(StoreError::Migration(
            "title jobs schema/marker missing".into(),
        ));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn title_job_schema_refuses_partial_shape_and_has_backend_parity() {
        let conn = rusqlite::Connection::open_in_memory().expect("fixture");
        conn.execute_batch(DEPENDENCIES).expect("dependencies");
        conn.execute_batch(super::super::dv_title_jobs::SCHEMA)
            .expect("install");
        verify_sqlite(&conn).expect("complete");
        #[cfg(feature = "hiqlite-store")]
        for sql in statements() {
            conn.execute_batch(&sql)
                .expect("idempotent replica statement");
        }
        verify_sqlite(&conn).expect("same schema");
        conn.execute_batch("DROP TRIGGER dv_title_jobs_file_update")
            .expect("partial fixture");
        assert!(verify_sqlite(&conn).is_err());
    }

    #[test]
    fn title_source_only_update_and_schema_replay_cannot_reseed_stale_facts() {
        for recursive in [false, true] {
            let conn = rusqlite::Connection::open_in_memory().expect("fixture");
            conn.pragma_update(None, "recursive_triggers", recursive)
                .expect("trigger mode");
            conn.execute_batch(DEPENDENCIES).expect("dependencies");
            conn.execute_batch(
                "INSERT INTO files VALUES(1,'/media/movie.mkv',100,1,'hevc',7,1,'{}',1);",
            )
            .expect("pre-migration probed source");
            conn.execute_batch(super::super::dv_title_jobs::SCHEMA)
                .expect("initial seed");
            let revision: i64 = conn
                .query_row("SELECT revision FROM dv_title_jobs_demand", [], |r| {
                    r.get(0)
                })
                .expect("one initial demand");
            conn.execute_batch("UPDATE files SET size=101 WHERE id=1")
                .expect("source changed without fresh probe");
            conn.execute_batch(super::super::dv_title_jobs::SCHEMA)
                .expect("idempotent migration replay");
            let count: i64 = conn
                .query_row("SELECT count(*) FROM dv_title_jobs_demand", [], |r| {
                    r.get(0)
                })
                .expect("no stale demand");
            assert_eq!(count, 0, "recursive_triggers={recursive}");
            conn.execute_batch("UPDATE files SET probe_json='{}',scanned_at=scanned_at WHERE id=1")
                .expect("fresh ordinary probe, even with identical facts");
            let fresh: i64 = conn
                .query_row("SELECT revision FROM dv_title_jobs_demand", [], |r| {
                    r.get(0)
                })
                .expect("fresh demand");
            assert!(fresh > revision);
        }
    }

    #[test]
    fn title_probe_intent_is_order_independent_in_both_recursive_trigger_modes() {
        for recursive in [false, true] {
            for reverse_after_order in [false, true] {
                let conn = rusqlite::Connection::open_in_memory().expect("fixture");
                conn.pragma_update(None, "recursive_triggers", recursive)
                    .expect("trigger mode");
                conn.execute_batch(DEPENDENCIES).expect("dependencies");
                conn.execute_batch(super::super::dv_title_jobs::SCHEMA)
                    .expect("schema");
                if reverse_after_order {
                    let source_cleanup: String = conn
                        .query_row(
                            "SELECT sql FROM sqlite_master WHERE name='dv_title_jobs_file_update'",
                            [],
                            |r| r.get(0),
                        )
                        .expect("source trigger definition");
                    conn.execute_batch("DROP TRIGGER dv_title_jobs_file_update")
                        .expect("change creation order");
                    conn.execute_batch(&source_cleanup)
                        .expect("recreate after probe trigger");
                }
                conn.execute_batch(
                    "INSERT INTO files VALUES(1,'/media/movie.mkv',100,1,'hevc',7,1,'{}',1)",
                )
                .expect("probed file");
                let revision = || {
                    conn.query_row::<i64, _, _>(
                        "SELECT revision FROM dv_title_jobs_demand WHERE file_id=1",
                        [],
                        |r| r.get(0),
                    )
                    .expect("one demand")
                };
                let mut previous = revision();
                for source_update in ["size=size", "size=size+1,mtime=mtime+1,path=path||'x'"] {
                    conn.execute_batch("UPDATE files SET probe_json=json_set(probe_json,'$.plurx_dv_title_v1',json('{\"schema\":1}')); UPDATE dv_title_jobs_demand SET scanner_digest='scanner',terminal=1")
                        .expect("aggregate and terminal fixture");
                    // Explicit publication with byte-identical facts and timestamp.
                    conn.execute_batch(&format!("UPDATE files SET {source_update},probe_json=probe_json,scanned_at=scanned_at WHERE id=1"))
                        .expect("fresh ordinary publication");
                    let fresh = revision();
                    assert!(
                        fresh > previous,
                        "recursive={recursive}, reverse={reverse_after_order}"
                    );
                    assert_eq!(
                        conn.query_row::<i64, _, _>(
                            "SELECT terminal FROM dv_title_jobs_demand WHERE file_id=1",
                            [],
                            |r| r.get(0)
                        )
                        .expect("terminal"),
                        0
                    );
                    let aggregate: Option<String> = conn
                        .query_row(
                            "SELECT json_extract(probe_json,'$.plurx_dv_title_v1') FROM files",
                            [],
                            |r| r.get(0),
                        )
                        .expect("aggregate");
                    assert!(aggregate.is_none());
                    previous = fresh;
                }
                conn.execute_batch("UPDATE files SET size=size+1 WHERE id=1")
                    .expect("source-only invalidation");
                for field in [
                    "chapters",
                    "plurx_hevc_parameter_sets",
                    "plurx_content_encoding",
                ] {
                    for _ in 0..2 {
                        conn.execute_batch(&format!("UPDATE files SET probe_json=json_set(probe_json,'$.{field}',json('{{}}')) WHERE id=1"))
                            .expect("changed then identical decoration");
                        assert_eq!(
                            conn.query_row::<i64, _, _>(
                                "SELECT count(*) FROM dv_title_jobs_demand",
                                [],
                                |r| r.get(0)
                            )
                            .expect("no stale demand"),
                            0
                        );
                    }
                }
                conn.execute_batch("UPDATE files SET scanned_at=scanned_at WHERE id=1")
                    .expect("ordinary facts for changed source");
                assert!(revision() > previous);
            }
        }
    }

    #[test]
    fn title_command_marker_prevents_nested_attach_invalidation_in_both_trigger_modes() {
        for recursive in [false, true] {
            let conn = rusqlite::Connection::open_in_memory().expect("fixture");
            conn.pragma_update(None, "recursive_triggers", recursive)
                .expect("trigger mode");
            conn.execute_batch(DEPENDENCIES).expect("dependencies");
            conn.execute_batch(super::super::dv_title_jobs::SCHEMA)
                .expect("schema");
            conn.execute_batch(
                "INSERT INTO files VALUES(1,'/media/movie.mkv',100,1,'hevc',7,1,'{}',1);",
            )
            .expect("probed source");
            let revision: i64 = conn
                .query_row("SELECT revision FROM dv_title_jobs_demand", [], |r| {
                    r.get(0)
                })
                .expect("demand");
            conn.execute_batch(
                r#"INSERT INTO background_job_commands VALUES('attach','publish_dv_title',
                '{"output":{"file_id":1,"stats":{"schema":1}},"now_ms":1}',
                '{"outcome":"published","file_id":1}');"#,
            )
            .expect("nested attach");
            let after: i64 = conn
                .query_row("SELECT revision FROM dv_title_jobs_demand", [], |r| {
                    r.get(0)
                })
                .expect("same demand");
            assert_eq!(after, revision, "recursive_triggers={recursive}");
            let attached: i64 = conn
                .query_row(
                    "SELECT json_extract(probe_json,'$.plurx_dv_title_v1.schema') FROM files",
                    [],
                    |r| r.get(0),
                )
                .expect("aggregate not removed by nested invalidation");
            assert_eq!(attached, 1);
            let commands: i64 = conn
                .query_row("SELECT count(*) FROM background_job_commands", [], |r| {
                    r.get(0)
                })
                .expect("command consumed");
            assert_eq!(commands, 0);
            conn.execute_batch("UPDATE files SET mtime=2 WHERE id=1")
                .expect("source-only supersession");
            let aggregate: Option<String> = conn
                .query_row(
                    "SELECT json_extract(probe_json,'$.plurx_dv_title_v1') FROM files",
                    [],
                    |r| r.get(0),
                )
                .expect("aggregate query");
            assert!(aggregate.is_none(), "recursive_triggers={recursive}");
        }
    }
}
