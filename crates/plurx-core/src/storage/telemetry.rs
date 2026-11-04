//! Preserve this node's local observations and normative rendition plans
//! across storage-mode changes. No row is submitted to consensus.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::store::{SqliteStore, SQLITE_SCHEMA_VERSION};

pub const LOCAL_TABLES: [&str; 6] = [
    "playback_events",
    "network_priors",
    "candidate_link_priors",
    "fragment_indexes",
    "fragment_index_outcomes",
    "rendition_plans",
];
const SIDECAR_SCHEMA_VERSION: i64 = 12;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalTelemetryTableTransfer {
    pub table: String,
    pub columns: Vec<String>,
    pub rows: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalTelemetryTransferReceipt {
    pub format_version: u32,
    pub source_schema_version: i64,
    pub target_schema_version: i64,
    pub tables: Vec<LocalTelemetryTableTransfer>,
    pub local_sequences: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Column {
    name: String,
    kind: String,
    not_null: bool,
    default: Option<String>,
    primary_key: i64,
    hidden: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Shape {
    columns: Vec<Column>,
    strict: bool,
    auto_increment: bool,
}

fn refused(reason: impl std::fmt::Display) -> StoreError {
    StoreError::Migration(format!("local storage continuity refused: {reason}"))
}

fn regular(path: &Path) -> Result<(), StoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| refused(format!("reading {}: {error}", path.display())))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(refused("transfer requires existing regular SQLite files"));
    }
    Ok(())
}

/// Call on a blocking thread while the directory owner has stopped application
/// producers. The source is read through an explicit SQLite snapshot, including
/// committed WAL pages. The destination must be newly built, or contain exactly
/// the same completed transfer. Unknown current-sidecar data is never ignored.
pub fn transfer_local_telemetry(
    source: &Path,
    target: &Path,
) -> Result<LocalTelemetryTransferReceipt, StoreError> {
    regular(source)?;
    regular(target)?;
    let source = fs::canonicalize(source).map_err(|error| refused(error.to_string()))?;
    let target = fs::canonicalize(target).map_err(|error| refused(error.to_string()))?;
    if source == target {
        return Err(refused("source and destination must differ"));
    }
    // The current native migration chain is the canonical common shape. This
    // schema-only reference contains no identity, credentials or user data.
    let reference = tempfile::tempdir().map_err(|error| refused(error.to_string()))?;
    let reference_path = reference.path().join("schema.sqlite");
    drop(SqliteStore::open(&reference_path)?);
    let reference = Connection::open_with_flags(&reference_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut canonical = BTreeMap::new();
    for table in LOCAL_TABLES {
        canonical.insert(table, shape(&reference, "main", table)?);
    }

    let connection = Connection::open_with_flags(
        &target,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    #[cfg(target_os = "macos")]
    connection.pragma_update(None, "fullfsync", true)?;
    connection.execute(
        "ATTACH DATABASE ?1 AS continuity_source",
        [read_only_uri(&source)],
    )?;
    connection.execute_batch("BEGIN IMMEDIATE;")?;
    let result = (|| {
        let source_version = validate_layout(&connection, "continuity_source")?;
        let target_version = validate_layout(&connection, "main")?;
        for table in LOCAL_TABLES {
            if shape(&connection, "continuity_source", table)? != canonical[table]
                || shape(&connection, "main", table)? != canonical[table]
            {
                return Err(refused(format!("unknown local table shape: {table}")));
            }
        }
        let local_sequences = sequences(&connection, "continuity_source")?;
        let target_sequences = sequences(&connection, "main")?;
        let mut tables = Vec::new();
        let mut target_empty = true;
        let mut already_complete = local_sequences == target_sequences;
        for table in LOCAL_TABLES {
            let source = digest(&connection, "continuity_source", table, &canonical[table])?;
            let target = digest(&connection, "main", table, &canonical[table])?;
            target_empty &= target.0 == 0;
            already_complete &= source == target;
            tables.push(LocalTelemetryTableTransfer {
                table: table.to_owned(),
                columns: canonical[table]
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect(),
                rows: source.0,
                sha256: source.1,
            });
        }
        if !already_complete {
            if !target_empty || !target_sequences.is_empty() {
                return Err(refused(
                    "destination already has different accepted local observations",
                ));
            }
            for table in LOCAL_TABLES {
                let columns = projection(&canonical[table]);
                connection.execute_batch(&format!(
                    "INSERT INTO main.{name} ({columns}) SELECT {columns} FROM continuity_source.{name} ORDER BY _rowid_;",
                    name = quoted(table),
                ))?;
            }
            if !local_sequences.is_empty() {
                if !has_table(&connection, "main", "sqlite_sequence")? {
                    return Err(refused(
                        "local sequence high-water marks have no destination",
                    ));
                }
                for (table, sequence) in &local_sequences {
                    connection
                        .execute("DELETE FROM main.sqlite_sequence WHERE name=?1", [table])?;
                    connection.execute(
                        "INSERT INTO main.sqlite_sequence(name,seq) VALUES(?1,?2)",
                        rusqlite::params![table, sequence],
                    )?;
                }
            }
            for table in &tables {
                let actual = digest(
                    &connection,
                    "main",
                    &table.table,
                    &canonical[table.table.as_str()],
                )?;
                if actual != (table.rows, table.sha256.clone()) {
                    return Err(refused(format!(
                        "typed local parity failed: {}",
                        table.table
                    )));
                }
            }
            if sequences(&connection, "main")? != local_sequences {
                return Err(refused("local sequence high-water parity failed"));
            }
        }
        Ok(LocalTelemetryTransferReceipt {
            format_version: 1,
            source_schema_version: source_version,
            target_schema_version: target_version,
            tables,
            local_sequences,
        })
    })();
    match result {
        Ok(receipt) => {
            connection.execute_batch("COMMIT;")?;
            connection.execute_batch("DETACH DATABASE continuity_source;")?;
            Ok(receipt)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

fn shape(connection: &Connection, schema: &str, table: &str) -> Result<Shape, StoreError> {
    let definition: (String, i64, i64) = connection.query_row(
        "SELECT type,wr,strict FROM pragma_table_list WHERE schema=?1 AND name=?2",
        rusqlite::params![schema, table],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if definition.0 != "table" || definition.1 != 0 {
        return Err(refused("local data is not a canonical rowid table"));
    }
    let columns = connection
        .prepare(&format!(
            "PRAGMA {}.table_xinfo({})",
            quoted(schema),
            literal(table)
        ))?
        .query_map([], |row| {
            Ok(Column {
                name: row.get(1)?,
                kind: row.get::<_, String>(2)?.to_ascii_uppercase(),
                not_null: row.get(3)?,
                default: row.get(4)?,
                primary_key: row.get(5)?,
                hidden: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let sql: String = connection.query_row(
        &format!(
            "SELECT sql FROM {}.sqlite_schema WHERE name=?1 AND type='table'",
            quoted(schema)
        ),
        [table],
        |row| row.get(0),
    )?;
    let triggers: i64 = connection.query_row(
        &format!(
            "SELECT count(*) FROM {}.sqlite_schema WHERE type='trigger' AND tbl_name=?1",
            quoted(schema)
        ),
        [table],
        |row| row.get(0),
    )?;
    let foreign_keys: i64 = connection.query_row(
        &format!(
            "SELECT count(*) FROM pragma_foreign_key_list({}, {})",
            literal(table),
            literal(schema)
        ),
        [],
        |row| row.get(0),
    )?;
    if triggers != 0 || foreign_keys != 0 {
        return Err(refused(
            "unknown trigger or foreign-key behavior on node-local data",
        ));
    }
    Ok(Shape {
        columns,
        strict: definition.2 != 0,
        auto_increment: sql.to_ascii_uppercase().contains("AUTOINCREMENT"),
    })
}

fn validate_layout(connection: &Connection, schema: &str) -> Result<i64, StoreError> {
    let version: i64 = connection.query_row(
        &format!("PRAGMA {}.user_version", quoted(schema)),
        [],
        |row| row.get(0),
    )?;
    if version == SQLITE_SCHEMA_VERSION && has_table(connection, schema, "settings")? {
        return Ok(version);
    }
    if version != SIDECAR_SCHEMA_VERSION || has_table(connection, schema, "settings")? {
        return Err(refused("unknown native or local-sidecar schema version"));
    }
    let names = connection
        .prepare(&format!(
            "SELECT name FROM {}.sqlite_schema WHERE type='table'",
            quoted(schema)
        ))?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for table in names {
        if !LOCAL_TABLES.contains(&table.as_str())
            && !["sqlite_sequence", "sqlite_stat1", "sqlite_stat4"].contains(&table.as_str())
        {
            return Err(refused(format!("unclassified local-sidecar table {table}")));
        }
    }
    Ok(version)
}

fn has_table(connection: &Connection, schema: &str, table: &str) -> Result<bool, StoreError> {
    Ok(connection
        .query_row(
            &format!(
                "SELECT 1 FROM {}.sqlite_schema WHERE type='table' AND name=?1",
                quoted(schema)
            ),
            [table],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false))
}

fn sequences(connection: &Connection, schema: &str) -> Result<BTreeMap<String, i64>, StoreError> {
    if !has_table(connection, schema, "sqlite_sequence")? {
        return Ok(BTreeMap::new());
    }
    let mut result = BTreeMap::new();
    let mut statement = connection.prepare(&format!(
        "SELECT name,seq FROM {}.sqlite_sequence",
        quoted(schema)
    ))?;
    for row in statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })? {
        let (name, sequence) = row?;
        if LOCAL_TABLES.contains(&name.as_str())
            && (sequence < 0 || result.insert(name, sequence).is_some())
        {
            return Err(refused("invalid local sequence high-water mark"));
        }
    }
    Ok(result)
}

fn projection(shape: &Shape) -> String {
    std::iter::once("_rowid_".to_owned())
        .chain(shape.columns.iter().map(|column| column.name.clone()))
        .map(|name| quoted(&name))
        .collect::<Vec<_>>()
        .join(",")
}

fn digest(
    connection: &Connection,
    schema: &str,
    table: &str,
    shape: &Shape,
) -> Result<(u64, String), StoreError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {} FROM {}.{} ORDER BY _rowid_",
        projection(shape),
        quoted(schema),
        quoted(table)
    ))?;
    let columns = statement.column_count();
    let mut rows = statement.query([])?;
    let mut hash = Sha256::new();
    let mut count = 0u64;
    while let Some(row) = rows.next()? {
        hash.update([0xff]);
        for column in 0..columns {
            match row.get_ref(column)? {
                rusqlite::types::ValueRef::Null => hash.update([0]),
                rusqlite::types::ValueRef::Integer(value) => {
                    hash.update([1]);
                    hash.update(value.to_be_bytes());
                }
                rusqlite::types::ValueRef::Real(value) => {
                    hash.update([2]);
                    hash.update(value.to_bits().to_be_bytes());
                }
                rusqlite::types::ValueRef::Text(value) => {
                    hash.update([3]);
                    hash.update((value.len() as u64).to_be_bytes());
                    hash.update(value);
                }
                rusqlite::types::ValueRef::Blob(value) => {
                    hash.update([4]);
                    hash.update((value.len() as u64).to_be_bytes());
                    hash.update(value);
                }
            }
        }
        count = count
            .checked_add(1)
            .ok_or_else(|| refused("local row count overflow"))?;
    }
    Ok((count, format!("{:x}", hash.finalize())))
}

fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
fn read_only_uri(path: &Path) -> String {
    let mut encoded = "file:".to_owned();
    for byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(byte) {
            encoded.push(char::from(*byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded.push_str("?mode=ro");
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native(path: &Path) {
        drop(SqliteStore::open(path).expect("native schema"));
    }

    #[test]
    fn local_transfer_preserves_wal_text_blobs_ids_and_all_six_table_hashes() {
        let directory = tempfile::tempdir().expect("root");
        let source = directory.path().join("source.db");
        let target = directory.path().join("target.db");
        native(&source);
        native(&target);
        let writer = Connection::open(&source).expect("source");
        writer
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .expect("WAL");
        writer.execute("INSERT INTO playback_events(id,at_unix_ms,event,detail,speed_recent) VALUES(77,123,'test',?1,1.23456789012345)",["unicode 🙂\0after-nul"]).expect("event");
        writer.execute("INSERT INTO rendition_plans VALUES('retained-plan',4,1024,55,'argv',1,90000,6,1,?1,123)",[&[0u8,255,16,0][..]]).expect("normative packed plan");
        let receipt =
            transfer_local_telemetry(&source, &target).expect("transfer committed WAL snapshot");
        assert_eq!(receipt.tables.len(), 6);
        assert!(receipt
            .tables
            .iter()
            .any(|table| table.table == "playback_events" && table.rows == 1));
        assert!(receipt
            .tables
            .iter()
            .any(|table| table.table == "rendition_plans" && table.rows == 1));
        assert_eq!(
            transfer_local_telemetry(&source, &target).expect("completed retry"),
            receipt
        );
        let copied = Connection::open(&target).expect("target");
        assert_eq!(
            copied
                .query_row(
                    "SELECT detail FROM playback_events WHERE id=77",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .expect("text"),
            "unicode 🙂\0after-nul"
        );
        assert_eq!(
            copied
                .query_row("SELECT entries_packed FROM rendition_plans", [], |row| row
                    .get::<_, Vec<
                    u8,
                >>(
                    0
                ))
                .expect("blob"),
            vec![0, 255, 16, 0]
        );
    }

    #[test]
    fn local_transfer_refuses_accepted_destination_bytes_and_unknown_local_shapes() {
        let directory = tempfile::tempdir().expect("root");
        let source = directory.path().join("source.db");
        let target = directory.path().join("target.db");
        native(&source);
        native(&target);
        let accepted = Connection::open(&target).expect("target");
        accepted
            .execute(
                "INSERT INTO playback_events(at_unix_ms,event) VALUES(1,'accepted')",
                [],
            )
            .expect("existing data");
        assert!(transfer_local_telemetry(&source, &target).is_err());
        assert_eq!(
            accepted
                .query_row("SELECT event FROM playback_events", [], |row| row
                    .get::<_, String>(0))
                .expect("preserved"),
            "accepted"
        );
        accepted
            .execute("DELETE FROM playback_events", [])
            .expect("fresh target fixture");
        let changed = Connection::open(&source).expect("source");
        changed
            .execute(
                "ALTER TABLE rendition_plans ADD COLUMN unknown_durable_value TEXT",
                [],
            )
            .expect("unknown shape");
        assert!(transfer_local_telemetry(&source, &target).is_err());
    }

    #[test]
    fn local_transfer_round_trips_sidecar_and_refuses_unclassified_sidecar_tables() {
        let directory = tempfile::tempdir().expect("root");
        let source = directory.path().join("source.db");
        let sidecar = directory.path().join("telemetry.db");
        let restored = directory.path().join("restored.db");
        native(&source);
        native(&restored);
        let source_connection = Connection::open(&source).expect("native");
        source_connection.execute("INSERT INTO playback_events(id,at_unix_ms,event) VALUES(9007199254740993,1,'large-integer-identity')",[]).expect("exact 64-bit row identity");
        drop(
            crate::store::telemetry::NodeLocalTelemetry::open(&sidecar)
                .expect("actual current sidecar constructor"),
        );
        let sidecar_connection = Connection::open(&sidecar).expect("actual sidecar");
        let promoted = transfer_local_telemetry(&source, &sidecar).expect("native to sidecar");
        let converted = transfer_local_telemetry(&sidecar, &restored).expect("sidecar to native");
        assert_eq!(promoted.tables, converted.tables);
        assert_eq!(promoted.source_schema_version, SQLITE_SCHEMA_VERSION);
        assert_eq!(promoted.target_schema_version, SIDECAR_SCHEMA_VERSION);
        let restored_connection = Connection::open(&restored).expect("restored");
        assert_eq!(
            restored_connection
                .query_row("SELECT id FROM playback_events", [], |row| row
                    .get::<_, i64>(0))
                .expect("exact identity"),
            9_007_199_254_740_993
        );
        sidecar_connection
            .execute(
                "CREATE TABLE unclassified_future_observations(value TEXT)",
                [],
            )
            .expect("unknown local table");
        assert!(transfer_local_telemetry(&sidecar, &restored).is_err());
    }
}
