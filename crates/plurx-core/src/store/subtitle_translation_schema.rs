//! Independently recognized progressive translation extension.
use crate::error::StoreError;
use std::sync::LazyLock;

pub(crate) const SCHEMA: &str = include_str!("subtitle_translation.sql");
pub(crate) const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'subtitle_translation_*' AND sql IS NOT NULL ORDER BY name";
#[cfg(feature = "hiqlite-store")]
pub(crate) fn statements() -> Vec<String> {
    SCHEMA
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
    let conn = rusqlite::Connection::open_in_memory().expect("translation schema fixture");
    conn.execute_batch("CREATE TABLE files(id INTEGER,size INTEGER,mtime INTEGER); CREATE TABLE background_jobs(kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT);").expect("translation schema dependencies");
    conn.execute_batch(SCHEMA).expect("translation schema");
    rows(&conn).expect("translation schema shape")
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
            "progressive translation schema is partial or incompatible".into(),
        ))
    }
}
pub(crate) fn verify_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    if version(&rows(conn)?)? != Some(1) {
        return Err(StoreError::Migration(
            "progressive translation schema missing".into(),
        ));
    }
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM subtitle_translation_schema WHERE singleton=1 AND version=1",
        [],
        |row| row.get(0),
    )?;
    if count != 1 {
        return Err(StoreError::Migration(
            "progressive translation marker missing".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn translation_schema_install_is_atomic_and_refuses_partial_shape() {
        let conn = rusqlite::Connection::open_in_memory().expect("fixture");
        conn.execute_batch("CREATE TABLE files(id INTEGER,size INTEGER,mtime INTEGER); CREATE TABLE background_jobs(kind TEXT,state TEXT,payload_json TEXT,revision INTEGER,last_error_code TEXT);").expect("dependencies");
        assert_eq!(version(&rows(&conn).expect("shape")).expect("absent"), None);
        conn.execute_batch("BEGIN").expect("transaction");
        conn.execute_batch(SCHEMA).expect("install");
        verify_sqlite(&conn).expect("complete");
        conn.execute_batch("ROLLBACK").expect("rollback");
        assert_eq!(version(&rows(&conn).expect("shape")).expect("absent"), None);
        conn.execute_batch(SCHEMA).expect("replay");
        verify_sqlite(&conn).expect("replayed");
        conn.execute_batch("DROP TABLE subtitle_translation_read_leases")
            .expect("corrupt fixture");
        assert!(verify_sqlite(&conn).is_err());
    }
}
