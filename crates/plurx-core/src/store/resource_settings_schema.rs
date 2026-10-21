//! Independently installed settings provenance guards. AUTH/Source ordinals are
//! deliberately untouched. Partial/future installations never authorize a typed
//! writer; ordinary numeric settings and legacy playback remain available.
use crate::error::StoreError;
use rusqlite::{Connection, OptionalExtension};

pub(super) const SHAPE_SQL: &str =
    "SELECT name,sql FROM sqlite_master WHERE name GLOB 'resource_settings_*' ORDER BY name";
pub(super) const MARKER_SQL: &str =
    "SELECT version FROM resource_settings_schema WHERE singleton=1";
pub(super) const INSERT_MARKER: &str =
    "INSERT INTO resource_settings_schema(singleton,version,revision) VALUES(1,1,0)";
pub(super) const CLEAR_UNVERSIONED: &str = "DELETE FROM settings WHERE key IN ('transcode.max_hw_sessions.provenance','transcode.software_pool_threads.provenance')";
pub(super) const SNAPSHOT_SQL: &str = "SELECT r.revision,s.key,s.value FROM resource_settings_schema r LEFT JOIN settings s ON s.key IN ('transcode.max_hw_sessions','transcode.software_pool_threads','transcode.max_hw_sessions.provenance','transcode.software_pool_threads.provenance') WHERE r.singleton=1 AND r.version=1 ORDER BY s.key";
pub(super) const CLAIM_SQL: &str = "UPDATE resource_settings_schema SET write_claim=$1 WHERE singleton=1 AND version=1 AND revision=$2 AND write_claim IS NULL";
pub(super) const WRITE_SQL: &str = "INSERT INTO settings(key,value,updated_at) SELECT $1,$2,$3 WHERE EXISTS(SELECT 1 FROM resource_settings_schema WHERE singleton=1 AND version=1 AND write_claim=$4) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at";
pub(super) const CLEAR_CLAIM_SQL: &str =
    "UPDATE resource_settings_schema SET write_claim=NULL WHERE singleton=1 AND write_claim=$1";

// These canonical SQL bytes are also the installed shape contract. SQLite
// preserves their spelling in sqlite_master. Refusing even an equivalent but
// unrecognized spelling is conservative; never replace unknown trigger bodies.
pub(super) const OBJECTS: &[(&str, &str)] = &[
    ("resource_settings_schema", "CREATE TABLE resource_settings_schema (singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=1), revision INTEGER NOT NULL CHECK(revision>=0), write_claim TEXT) STRICT"),
    ("resource_settings_numeric_ai", "CREATE TRIGGER resource_settings_numeric_ai AFTER INSERT ON settings WHEN NEW.key IN ('transcode.max_hw_sessions','transcode.software_pool_threads') BEGIN DELETE FROM settings WHERE key=NEW.key||'.provenance'; END"),
    ("resource_settings_numeric_au", "CREATE TRIGGER resource_settings_numeric_au AFTER UPDATE OF key,value ON settings WHEN OLD.key IN ('transcode.max_hw_sessions','transcode.software_pool_threads') OR NEW.key IN ('transcode.max_hw_sessions','transcode.software_pool_threads') BEGIN DELETE FROM settings WHERE key IN (OLD.key||'.provenance',NEW.key||'.provenance'); END"),
    ("resource_settings_numeric_ad", "CREATE TRIGGER resource_settings_numeric_ad AFTER DELETE ON settings WHEN OLD.key IN ('transcode.max_hw_sessions','transcode.software_pool_threads') BEGIN DELETE FROM settings WHERE key=OLD.key||'.provenance'; END"),
    ("resource_settings_revision_ai", "CREATE TRIGGER resource_settings_revision_ai AFTER INSERT ON settings WHEN NEW.key IN ('transcode.max_hw_sessions','transcode.software_pool_threads','transcode.max_hw_sessions.provenance','transcode.software_pool_threads.provenance') BEGIN UPDATE resource_settings_schema SET revision=revision+1 WHERE singleton=1; END"),
    ("resource_settings_revision_au", "CREATE TRIGGER resource_settings_revision_au AFTER UPDATE OF key,value ON settings WHEN OLD.key IN ('transcode.max_hw_sessions','transcode.software_pool_threads','transcode.max_hw_sessions.provenance','transcode.software_pool_threads.provenance') OR NEW.key IN ('transcode.max_hw_sessions','transcode.software_pool_threads','transcode.max_hw_sessions.provenance','transcode.software_pool_threads.provenance') BEGIN UPDATE resource_settings_schema SET revision=revision+1 WHERE singleton=1; END"),
    ("resource_settings_revision_ad", "CREATE TRIGGER resource_settings_revision_ad AFTER DELETE ON settings WHEN OLD.key IN ('transcode.max_hw_sessions','transcode.software_pool_threads','transcode.max_hw_sessions.provenance','transcode.software_pool_threads.provenance') BEGIN UPDATE resource_settings_schema SET revision=revision+1 WHERE singleton=1; END"),
];

pub(super) fn verify_shape(rows: &[(String, String)]) -> Result<bool, StoreError> {
    if rows.is_empty() {
        return Ok(false);
    }
    if rows.len() == OBJECTS.len()
        && OBJECTS.iter().all(|(name, sql)| {
            rows.iter()
                .any(|(n, s)| n == name && s.trim_end_matches(';') == *sql)
        })
    {
        Ok(true)
    } else {
        Err(StoreError::Migration(
            "partial or unknown resource settings schema; adaptive settings unavailable".into(),
        ))
    }
}

pub(super) fn sqlite_shape(conn: &Connection) -> Result<bool, StoreError> {
    let rows = conn
        .prepare(SHAPE_SQL)?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<(String, String)>, _>>()?;
    verify_shape(&rows)
}

pub(super) fn sqlite_ready(conn: &Connection) -> Result<bool, StoreError> {
    if !matches!(sqlite_shape(conn), Ok(true)) {
        return Ok(false);
    }
    let version = conn
        .query_row(MARKER_SQL, [], |row| row.get::<_, i64>(0))
        .optional()?;
    Ok(version == Some(1))
}

pub(super) fn install_sqlite(conn: &Connection) -> Result<(), StoreError> {
    match sqlite_shape(conn) {
        Ok(true) => return Ok(()),
        Err(error) => {
            tracing::warn!(%error, "preserving unrecognized resource settings installation; legacy playback remains available");
            return Ok(());
        }
        Ok(false) => {}
    }
    let tx = conn.unchecked_transaction()?;
    for (_, sql) in OBJECTS {
        tx.execute(sql, [])?;
    }
    // A schema-less restored companion is not proof of operator intent. An
    // intact versioned full-image restore takes the verified-shape path above.
    tx.execute(CLEAR_UNVERSIONED, [])?;
    tx.execute(INSERT_MARKER, [])?;
    if !sqlite_ready(&tx)? {
        return Err(StoreError::Migration(
            "resource settings installation did not verify".into(),
        ));
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn sqlite_snapshot(
    conn: &Connection,
) -> Result<Option<crate::resource_settings::CapacitySnapshot>, StoreError> {
    if !sqlite_ready(conn)? {
        return Ok(None);
    }
    let mut statement = conn.prepare(SNAPSHOT_SQL)?;
    let mut rows = statement.query([])?;
    let mut snapshot = None;
    while let Some(row) = rows.next()? {
        let revision = row.get(0)?;
        let current = snapshot.get_or_insert_with(|| crate::resource_settings::CapacitySnapshot {
            revision,
            values: Default::default(),
        });
        if let (Some(key), Some(value)) = (
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ) {
            current.values.insert(key, value);
        }
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        resource_settings::{ReadState, ResourceSettings},
        store::keys,
    };
    use std::collections::BTreeMap;

    fn settings() -> Connection {
        let conn = Connection::open_in_memory().expect("settings database");
        conn.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL,updated_at INTEGER NOT NULL DEFAULT 0) STRICT").expect("legacy settings");
        conn
    }
    fn automatic(conn: &Connection) {
        conn.execute(
            "INSERT OR REPLACE INTO settings(key,value) VALUES(?1,'2')",
            [keys::MAX_HW_SESSIONS],
        )
        .expect("numeric cache");
        conn.execute(
            "INSERT OR REPLACE INTO settings(key,value) VALUES(?1,?2)",
            [
                keys::MAX_HW_SESSIONS_PROVENANCE,
                r#"{"schema":1,"mode":"automatic","bound_value":"2"}"#,
            ],
        )
        .expect("typed record last");
    }
    fn report(conn: &Connection) -> ResourceSettings {
        let values = conn
            .prepare("SELECT key,value FROM settings")
            .expect("snapshot")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("rows")
            .collect::<Result<BTreeMap<String, String>, _>>()
            .expect("settings");
        ResourceSettings::from_snapshot(&values)
    }

    #[test]
    fn legacy_same_value_upsert_and_replace_invalidate_automatic_with_recursive_triggers_off_and_on(
    ) {
        for recursive in [false, true] {
            let conn = settings();
            install_sqlite(&conn).expect("installation");
            conn.pragma_update(None, "recursive_triggers", recursive)
                .expect("old connection pragma");
            for sql in ["UPDATE settings SET value='2' WHERE key=?1", "INSERT INTO settings(key,value) VALUES(?1,'2') ON CONFLICT(key) DO UPDATE SET value=excluded.value", "INSERT OR REPLACE INTO settings(key,value) VALUES(?1,'2')"] {
                automatic(&conn);
                assert_eq!(report(&conn).hardware_sessions.state, ReadState::Automatic);
                let before: i64 = conn.query_row("SELECT revision FROM resource_settings_schema",[],|r| r.get(0)).expect("revision");
                conn.execute(sql,[keys::MAX_HW_SESSIONS]).expect("actual legacy SQL");
                assert_eq!(report(&conn).hardware_sessions.state, ReadState::LegacyAmbiguous);
                let after: i64 = conn.query_row("SELECT revision FROM resource_settings_schema",[],|r| r.get(0)).expect("revision");
                assert!(after > before);
            }
            automatic(&conn);
            conn.execute("DELETE FROM settings WHERE key=?1", [keys::MAX_HW_SESSIONS])
                .expect("legacy delete");
            assert_eq!(report(&conn).hardware_sessions.state, ReadState::Absent);
        }
    }

    #[test]
    fn partial_or_future_installation_never_stamps_success_or_breaks_legacy_writes() {
        let conn = settings();
        conn.execute(OBJECTS[0].1, [])
            .expect("partial marker shape");
        install_sqlite(&conn).expect("ordinary playback may still open");
        assert!(!sqlite_ready(&conn).expect("partial refused"));
        assert_eq!(
            conn.query_row("SELECT count(*) FROM resource_settings_schema", [], |r| r
                .get::<_, i64>(
                0
            ))
            .expect("marker count"),
            0
        );
        conn.execute(
            "INSERT INTO settings(key,value) VALUES('transcode.max_hw_sessions','0')",
            [],
        )
        .expect("ordinary zero write");
        assert_eq!(report(&conn).hardware_sessions.stored_value, Some(0));
    }

    #[test]
    fn schema_marker_and_guards_install_atomically_and_idempotently() {
        let conn = settings();
        {
            let tx = conn
                .unchecked_transaction()
                .expect("interrupted transaction");
            for (_, sql) in OBJECTS {
                tx.execute(sql, []).expect("DDL");
            }
            // Simulated interrupted install: no success marker, transaction rollback.
        }
        assert!(!sqlite_shape(&conn).expect("rolled back"));
        install_sqlite(&conn).expect("fresh install");
        install_sqlite(&conn).expect("restart");
        assert!(sqlite_ready(&conn).expect("verified complete installation"));
        assert_eq!(report(&conn).hardware_sessions.state, ReadState::Absent);
    }
}
