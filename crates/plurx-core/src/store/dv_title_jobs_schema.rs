//! Additive title-job extension; both stores install and verify the same SQL.
use crate::error::StoreError;
use std::sync::LazyLock;
pub(crate) const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'dv_title_jobs_*' AND sql IS NOT NULL ORDER BY name";
pub(crate) const MARKER_SQL: &str =
    "SELECT count(*) AS count FROM dv_title_jobs_schema WHERE singleton=1 AND version=1";
const DEPENDENCIES: &str = "CREATE TABLE files(id INTEGER PRIMARY KEY,path TEXT,size INTEGER,mtime INTEGER,video_codec TEXT,dv_profile INTEGER,dv_rpu_present INTEGER,probe_json TEXT); CREATE TABLE background_jobs(id TEXT,kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT,owner_node_id TEXT,owner_boot_id TEXT,claim_id TEXT,lease_expires_ms INTEGER,result_ref TEXT,updated_at_ms INTEGER); CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT); CREATE TABLE background_job_waiters(job_id TEXT,state TEXT,result_ref TEXT,updated_at_ms INTEGER);";
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
}
