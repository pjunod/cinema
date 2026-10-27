//! Independently recognized Shared caption read extension.
use crate::error::StoreError;
use std::sync::LazyLock;

pub(crate) const SCHEMA: &str = include_str!("shared_caption_read.sql");
pub(crate) const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'shared_caption_*' AND sql IS NOT NULL ORDER BY name";
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
    conn.execute_batch("CREATE TABLE subtitle_translation_assets(id TEXT PRIMARY KEY);")
        .expect("translation schema dependencies");
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
            "Shared caption read schema is partial or incompatible".into(),
        ))
    }
}
pub(crate) fn verify_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    if version(&rows(conn)?)? != Some(1) {
        return Err(StoreError::Migration(
            "Shared caption read schema missing".into(),
        ));
    }
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM shared_caption_schema WHERE singleton=1 AND version=1",
        [],
        |row| row.get(0),
    )?;
    if count != 1 {
        return Err(StoreError::Migration(
            "Shared caption read marker missing".into(),
        ));
    }
    Ok(())
}
