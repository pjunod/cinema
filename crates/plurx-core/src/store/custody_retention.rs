//! Retention keeps a job that still owns charged source custody.
//!
//! A charged `background_source_custody` row names its owner job, and that
//! job's payload is the custody's only exact source binding. If the settled
//! job is deleted, the host source census can no longer tie what it observed
//! to the file the dead owner read. The enqueue and upkeep triggers are
//! replaced under their own names with one added clause. SQLite installs them
//! as one migration step; hiqlite installs them as an idempotent,
//! shape-verified extension whose marker an older binary never reads.
use std::sync::LazyLock;

use crate::error::StoreError;

pub(crate) const SCHEMA: &str = include_str!("background_jobs_custody_retention.sql");
pub const SQLITE_INTRODUCED_SCHEMA: i64 = 129;
pub(crate) const SHAPE_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name IN \
    ('background_custody_retention_schema','background_job_enqueue_command',\
    'background_job_maintenance_command') AND sql IS NOT NULL ORDER BY name";
pub(crate) const MARKER_SQL: &str =
    "SELECT count(*) AS count FROM background_custody_retention_schema WHERE singleton=1 AND version=1";
const MARKER_TABLE: &str = "background_custody_retention_schema";

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

/// The installed shape, from a fresh SQLite catalogue migrated to the current
/// schema: the triggers reference most of the queue, so the whole catalogue is
/// their dependency.
static SHAPE: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let conn = rusqlite::Connection::open_in_memory().expect("custody retention fixture");
    super::sqlite::SqliteStore::migrate(&conn).expect("custody retention catalogue");
    rows(&conn).expect("custody retention shape")
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

/// `None` before the extension (no marker table: the older triggers are in
/// place), `Some(1)` for its exact shape; anything in between is refused.
pub(crate) fn version(shape: &[(String, String)]) -> Result<Option<i64>, StoreError> {
    if !shape.iter().any(|(name, _)| name == MARKER_TABLE) {
        Ok(None)
    } else if canonical(shape) == canonical(&SHAPE) {
        Ok(Some(1))
    } else {
        Err(StoreError::Migration(
            "custody retention schema is partial or incompatible".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_custody_clause_differs_from_the_v85_and_v86_triggers() {
        // The replacement triggers are the v85 enqueue and v86 upkeep triggers
        // with one clause added to each settled-job eviction; nothing else.
        let clause = "          AND NOT EXISTS (SELECT 1 FROM background_source_custody custody\n            WHERE custody.owner_kind = 'job' AND custody.work_id = job.id)\n";
        let trigger = |sql: &str, name: &str| {
            let start = sql
                .find(&format!("CREATE TRIGGER IF NOT EXISTS {name}"))
                .expect("trigger");
            let end = sql[start..].find("\nEND;").expect("trigger end") + start + 5;
            sql[start..end].to_owned()
        };
        for (name, before) in [
            (
                "background_job_enqueue_command",
                super::super::background_jobs::RETENTION_SCHEMA,
            ),
            (
                "background_job_maintenance_command",
                super::super::background_jobs::RECEIPT_PRESSURE_SCHEMA,
            ),
        ] {
            let after = trigger(SCHEMA, name);
            assert_eq!(
                after.matches(clause).count(),
                1,
                "{name}: one eviction guarded"
            );
            assert_eq!(
                after.replacen(clause, "", 1),
                trigger(before, name),
                "{name}"
            );
        }
    }

    #[test]
    fn a_fresh_catalogue_is_the_exact_extension() {
        assert_eq!(version(&SHAPE).expect("shape"), Some(1));
        let conn = rusqlite::Connection::open_in_memory().expect("catalogue");
        super::super::sqlite::SqliteStore::migrate(&conn).expect("migrate");
        let count: i64 = conn
            .query_row(MARKER_SQL, [], |row| row.get(0))
            .expect("marker");
        assert_eq!(count, 1);
        // A marker beside the older triggers is refused, not repaired.
        let mut partial = rows(&conn).expect("rows");
        partial.retain(|(name, _)| name == MARKER_TABLE);
        assert!(version(&partial).is_err());
        assert_eq!(version(&[]).expect("absent"), None);
    }
}
