use super::*;
use crate::resource_settings::{ReadState, ResourceSettings};
use crate::store::resource_settings_schema as schema;
use std::collections::BTreeMap;

fn database() -> Connection {
    let db = Connection::open_in_memory().expect("import database");
    db.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL,updated_at INTEGER NOT NULL DEFAULT 0) STRICT").expect("settings");
    db
}

fn prepared_source(versioned: bool) -> Connection {
    let source = database();
    if versioned {
        schema::install_sqlite(&source).expect("versioned source");
    }
    for (key, value) in [(keys::MAX_HW_SESSIONS, "2"), (keys::SW_POOL_THREADS, "0")] {
        source
            .execute(
                "INSERT INTO settings(key,value) VALUES(?1,?2)",
                [key, value],
            )
            .expect("numeric");
        let record =
            serde_json::json!({"schema":1,"mode":"automatic","bound_value":value}).to_string();
        source
            .execute(
                "INSERT INTO settings(key,value) VALUES(?1,?2)",
                [&format!("{key}.provenance"), &record],
            )
            .expect("companion");
    }
    source
}

fn plan(source: &Connection) -> TablePlan {
    let (hardware_provenance, software_provenance) =
        source_capacity_provenance(source).expect("actual schema/binding recognition");
    TABLES
        .iter()
        .find(|table| table.name == "settings")
        .copied()
        .expect("production settings plan")
        .capacity_provenance(&SourceMetadata {
            backup_sha256: String::new(),
            schema_version: SQLITE_SCHEMA_VERSION,
            instance_id: String::new(),
            instance_updated_at: 0,
            hardware_provenance,
            software_provenance,
        })
}

fn import_settings(source: &Connection, table: TablePlan) -> Connection {
    let target = database();
    schema::install_sqlite(&target).expect("target guards");
    let sql = import_select_sql(table, SQLITE_SCHEMA_VERSION);
    let rows = source
        .prepare(&sql)
        .expect("actual import SELECT")
        .query_map(rusqlite::params![i64::MAX, 0], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .expect("import rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("imported values");
    for (key, value, updated_at) in rows {
        target
            .execute(
                "INSERT INTO settings(key,value,updated_at) VALUES(?1,?2,?3)",
                rusqlite::params![key, value, updated_at],
            )
            .expect("actual guarded target insert");
    }
    target
}

fn report(db: &Connection) -> ResourceSettings {
    let values = db
        .prepare("SELECT key,value FROM settings")
        .expect("snapshot")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("rows")
        .collect::<Result<BTreeMap<String, String>, _>>()
        .expect("snapshot values");
    ResourceSettings::from_snapshot(&values)
}

#[test]
fn unversioned_import_cannot_restore_stale_automatic() {
    let source = prepared_source(false);
    let table = plan(&source);
    let target = import_settings(&source, table);
    let report = report(&target);
    assert_eq!(report.hardware_sessions.state, ReadState::LegacyAmbiguous);
    assert_eq!(
        report.software_pool_threads.state,
        ReadState::LegacyAmbiguous
    );
    assert_eq!(report.hardware_sessions.stored_value, Some(2));
    assert_eq!(report.software_pool_threads.stored_value, Some(0));
    assert_eq!(
        source_digest(&source, SQLITE_SCHEMA_VERSION, table).expect("normalized source digest"),
        source_digest(&target, SQLITE_SCHEMA_VERSION, table).expect("target digest")
    );
    let (query, _) = target_parity_page(table, &[]).expect("production parity SELECT");
    assert!(query.contains("key NOT IN"));
    assert_eq!(parity_key_columns(table), vec!["key"]);
}

#[test]
fn recognized_import_installs_exact_bound_provenance_last_and_keeps_canonical_parity() {
    let source = prepared_source(true);
    let table = plan(&source);
    let target = import_settings(&source, table);
    assert_eq!(
        report(&target).hardware_sessions.state,
        ReadState::Automatic
    );
    assert_eq!(
        report(&target).software_pool_threads.state,
        ReadState::Automatic
    );
    assert_eq!(
        source_digest(&source, SQLITE_SCHEMA_VERSION, table).expect("source digest"),
        source_digest(&target, SQLITE_SCHEMA_VERSION, table).expect("target digest")
    );
    // Corrupt/binding-mismatched companion in an otherwise recognized image.
    source
        .execute(
            "UPDATE settings SET value=?1 WHERE key=?2",
            rusqlite::params![
                r#"{"schema":1,"mode":"automatic","bound_value":"99"}"#,
                keys::MAX_HW_SESSIONS_PROVENANCE
            ],
        )
        .expect("corrupt companion");
    let table = plan(&source);
    let target = import_settings(&source, table);
    assert_eq!(
        report(&target).hardware_sessions.state,
        ReadState::LegacyAmbiguous
    );
    assert_eq!(
        report(&target).software_pool_threads.state,
        ReadState::Automatic
    );
}

#[test]
fn unversioned_image_upgrade_discards_provenance_but_intact_versioned_restore_retains_it() {
    let old = prepared_source(false);
    schema::install_sqlite(&old).expect("legacy full-image migration");
    assert_eq!(
        report(&old).hardware_sessions.state,
        ReadState::LegacyAmbiguous
    );
    let current = prepared_source(true);
    schema::install_sqlite(&current).expect("recognized full-image restart");
    assert_eq!(
        report(&current).hardware_sessions.state,
        ReadState::Automatic
    );
}
