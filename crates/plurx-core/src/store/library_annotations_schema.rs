//! Item ratings and per-user item lists — an independently recognized
//! extension schema, installed the same way on SQLite (one migration step) and
//! hiqlite (an idempotent install from the current-schema arm).
use crate::error::StoreError;
use std::sync::LazyLock;

pub(crate) const SCHEMA: &str = include_str!("library_annotations.sql");
pub(crate) const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'library_annotations_*' AND sql IS NOT NULL ORDER BY name";
pub(crate) const MARKER_SQL: &str =
    "SELECT count(*) AS count FROM library_annotations_schema WHERE singleton=1 AND version=1";
/// The tables this extension owns, children before parents' rows go.
const DEPENDENCIES: &str =
    "CREATE TABLE items(id INTEGER PRIMARY KEY); CREATE TABLE users(id INTEGER PRIMARY KEY);";

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
    let conn = rusqlite::Connection::open_in_memory().expect("annotations schema fixture");
    conn.execute_batch(DEPENDENCIES)
        .expect("annotations schema dependencies");
    conn.execute_batch(SCHEMA).expect("annotations schema");
    rows(&conn).expect("annotations schema shape")
});

/// `None` when nothing of the extension exists, `Some(1)` when its exact shape
/// does; anything in between is refused rather than repaired.
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
            "library annotations schema is partial or incompatible".into(),
        ))
    }
}

pub(crate) fn verify_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    if version(&rows(conn)?)? != Some(1) {
        return Err(StoreError::Migration(
            "library annotations schema missing".into(),
        ));
    }
    let count: i64 = conn.query_row(MARKER_SQL, [], |row| row.get(0))?;
    if count != 1 {
        return Err(StoreError::Migration(
            "library annotations marker missing".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotations_schema_installs_whole_and_refuses_a_partial_shape() {
        let conn = rusqlite::Connection::open_in_memory().expect("fixture");
        conn.execute_batch(DEPENDENCIES).expect("dependencies");
        assert_eq!(version(&rows(&conn).expect("shape")).expect("absent"), None);
        conn.execute_batch("BEGIN").expect("transaction");
        conn.execute_batch(SCHEMA).expect("install");
        verify_sqlite(&conn).expect("complete");
        conn.execute_batch("ROLLBACK").expect("rollback");
        assert_eq!(version(&rows(&conn).expect("shape")).expect("absent"), None);
        conn.execute_batch(SCHEMA).expect("replay");
        verify_sqlite(&conn).expect("replayed");
        conn.execute_batch("DROP TABLE library_annotations_rating_fetches")
            .expect("corrupt fixture");
        assert!(verify_sqlite(&conn).is_err());
    }

    #[test]
    fn hiqlite_statements_are_the_sqlite_schema_split() {
        #[cfg(feature = "hiqlite-store")]
        {
            let conn = rusqlite::Connection::open_in_memory().expect("fixture");
            conn.execute_batch(DEPENDENCIES).expect("dependencies");
            for statement in statements() {
                conn.execute_batch(&statement).expect("statement");
            }
            verify_sqlite(&conn).expect("same shape as the SQLite step");
        }
    }
}
