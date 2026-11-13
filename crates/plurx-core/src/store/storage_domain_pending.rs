//! Desired storage identities settle atomically once their stored holders are safe.
use std::sync::LazyLock;

use crate::error::StoreError;

pub(crate) const SQLITE_INTRODUCED_SCHEMA: i64 = 130;
pub(crate) const SCHEMA: &str = include_str!("storage_domain_pending.sql");
pub(crate) const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name IN \
    ('background_storage_domain_pending','background_storage_domains_live',\
    'background_storage_domain_changed_roots','background_storage_domain_disturbed',\
    'background_storage_domain_holders','background_storage_domain_affected_jobs',\
    'background_storage_domains_apply','background_storage_domain_pending_schema') \
    AND sql IS NOT NULL ORDER BY name";
pub(crate) const MARKER_SQL: &str = "SELECT count(*) AS count FROM background_storage_domain_pending_schema WHERE singleton=1 AND version=1";

pub(crate) fn statements() -> Vec<String> {
    SCHEMA
        .split("-- next statement")
        .map(str::trim)
        .filter(|sql| !sql.is_empty())
        .map(str::to_owned)
        .collect()
}

fn rows(conn: &rusqlite::Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut query = conn.prepare(SHAPE_SQL)?;
    let rows = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

/// Views depend on the full queue catalogue, so derive the exact shape from
/// a freshly migrated SQLite catalogue rather than a partial fixture.
static SHAPE: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let conn = rusqlite::Connection::open_in_memory().expect("storage domain pending fixture");
    super::sqlite::SqliteStore::migrate(&conn).expect("storage domain pending catalogue");
    rows(&conn).expect("storage domain pending shape")
});

fn canonical(rows: &[(String, String)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(name, sql)| {
            (
                name.clone(),
                sql.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect()
}

/// An entirely absent extension can be installed; every nonempty partial or
/// incompatible shape is refused, including objects left without a marker.
pub(crate) fn version(shape: &[(String, String)]) -> Result<Option<i64>, StoreError> {
    if shape.is_empty() {
        Ok(None)
    } else if canonical(shape) == canonical(&SHAPE) {
        Ok(Some(1))
    } else {
        Err(StoreError::Migration(
            "storage domain pending schema is partial or incompatible".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_catalogue_is_the_exact_extension() {
        assert_eq!(version(&SHAPE).expect("shape"), Some(1));
        let conn = rusqlite::Connection::open_in_memory().expect("catalogue");
        super::super::sqlite::SqliteStore::migrate(&conn).expect("migrate");
        let count: i64 = conn
            .query_row(MARKER_SQL, [], |row| row.get(0))
            .expect("marker");
        assert_eq!(count, 1);
        for statement in statements() {
            conn.execute_batch(&statement).expect("idempotent install");
        }
        assert_eq!(
            version(&rows(&conn).expect("rows")).expect("shape"),
            Some(1)
        );
        assert_eq!(version(&[]).expect("absent"), None);
    }

    #[test]
    fn every_partial_shape_is_refused_even_without_its_marker() {
        assert_eq!(SHAPE.len(), 8);
        for missing in 0..SHAPE.len() {
            let mut partial = SHAPE.clone();
            partial.remove(missing);
            assert!(version(&partial).is_err(), "missing {}", SHAPE[missing].0);
        }
        let mut incompatible = SHAPE.clone();
        incompatible[0].1.push_str(" incompatible");
        assert!(version(&incompatible).is_err());
    }

    #[test]
    fn pending_rows_enforce_bounded_revision_and_json_state() {
        let conn = rusqlite::Connection::open_in_memory().expect("catalogue");
        super::super::sqlite::SqliteStore::migrate(&conn).expect("migrate");
        let insert = "INSERT INTO background_storage_domain_pending \
            (singleton,revision,mappings_json,base_json,saved_at_ms,state,settled_reason) \
            VALUES (1,?1,?2,?3,?4,?5,?6)";
        for (revision, mappings, base, saved_at, state, reason) in [
            (0, "[]", "[]", 0, "pending", "".to_owned()),
            (1, "{}", "[]", 0, "pending", "".to_owned()),
            (1, "[]", "null", 0, "pending", "".to_owned()),
            (1, "[]", "[]", -1, "pending", "".to_owned()),
            (1, "[]", "[]", 0, "unknown", "".to_owned()),
            (1, "[]", "[]", 0, "pending", "x".repeat(65)),
        ] {
            assert!(conn
                .execute(
                    insert,
                    rusqlite::params![revision, mappings, base, saved_at, state, reason]
                )
                .is_err());
        }
        conn.execute(insert, rusqlite::params![1, "[]", "[]", 0, "pending", ""])
            .expect("valid pending row");
        assert!(
            conn.execute(insert, rusqlite::params![2, "[]", "[]", 0, "pending", ""])
                .is_err(),
            "singleton cannot accumulate revisions"
        );
    }
}
