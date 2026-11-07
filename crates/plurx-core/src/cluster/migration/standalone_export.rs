//! One-way adapter from a quiescent private Hiqlite singleton to native SQLite.
//!
//! This module alone owns the old cluster runtime dependency. The caller owns
//! the data-directory lock, stops all application writers before calling, and
//! publishes the resulting native generation only after this adapter returns.
//! It never reopens the retained pre-activation root database. A failure leaves
//! the old backend authoritative and keeps its immutable image for recovery.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use hiqlite::macros::params;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::StoreError;
#[cfg(test)]
use crate::store::AUTH_SCHEMA_VERSION;
use crate::store::SQLITE_SCHEMA_VERSION;

const EXPORT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandaloneTableTransfer {
    pub table: String,
    pub disposition: String,
    pub source_columns: Vec<String>,
    pub source_rows: u64,
    pub source_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandaloneExportReceipt {
    pub format_version: u32,
    pub cluster_id: String,
    pub node_id: String,
    pub former_raft_id: u64,
    pub applied_term: u64,
    pub applied_index: u64,
    pub source_schema_version: i64,
    pub native_schema_version: i64,
    pub source_image: PathBuf,
    pub source_sha256: String,
    pub native_sha256: String,
    pub tables: Vec<StandaloneTableTransfer>,
}

/// Convert only a private, never-peered singleton. `native_target` must be an
/// unused path; the verified snapshot selects its exact native schema. No listener,
/// application worker or second writer may be started until the caller has
/// finished the durable mode handoff. This function does not publish authority.
pub async fn export_private_singleton(
    client: &hiqlite::Client,
    source_root: &Path,
    native_target: &Path,
    expected_cluster_id: &str,
    expected_node_id: &str,
) -> Result<StandaloneExportReceipt, StoreError> {
    let marker = super::read_activation_marker(&source_root.join("hiqlite"))?;
    if marker.cluster_id != expected_cluster_id || marker.admitted_role.is_some() {
        return Err(refusal("joined/restored or identity-mismatched directories require explicit cluster decommission/export"));
    }
    let deadline = tokio::time::Instant::now() + EXPORT_TIMEOUT;
    let local = client.local_db_raft_metrics().map_err(cluster_error)?;
    let membership = local.membership_snapshot();
    let observed = local.snapshot();
    if !membership.running
        || !membership.committed
        || membership.members != BTreeSet::from([membership.node_id])
        || membership.voters != BTreeSet::from([membership.node_id])
        || observed.current_leader != Some(membership.node_id)
    {
        return Err(refusal(
            "conversion requires an applied, running sole-member leader",
        ));
    }
    let anchor = tokio::time::timeout_at(
        deadline,
        client.execute_acked(
            "UPDATE settings SET value=value WHERE key='instance.id'",
            params!(),
        ),
    )
    .await
    .map_err(|_| refusal("conversion anchor timed out"))?
    .map_err(cluster_error)?;
    if anchor.result != 1 {
        return Err(refusal(
            "conversion anchor did not identify exactly one server",
        ));
    }
    let anchor = anchor
        .log_index
        .ok_or_else(|| refusal("conversion anchor has no acknowledged Raft position"))?;
    loop {
        let observation = local.snapshot();
        if !observation.running || observation.current_leader != Some(membership.node_id) {
            return Err(refusal(
                "local leadership disappeared while cutting conversion image",
            ));
        }
        if observation
            .last_applied_index
            .is_some_and(|index| index >= anchor)
        {
            break;
        }
        pause_before(deadline).await?;
    }
    tokio::time::timeout_at(deadline, client.trigger_local_db_snapshot())
        .await
        .map_err(|_| refusal("conversion snapshot trigger timed out"))?
        .map_err(cluster_error)?;
    loop {
        let metrics = tokio::time::timeout_at(deadline, client.metrics_db())
            .await
            .map_err(|_| refusal("conversion snapshot publication timed out"))?
            .map_err(cluster_error)?;
        if metrics
            .snapshot
            .as_ref()
            .is_some_and(|log| log.index >= anchor)
        {
            break;
        }
        pause_before(deadline).await?;
    }
    let snapshots = source_root.join("hiqlite/state_machine/snapshots");
    let mut pointer = Vec::new();
    File::open(snapshots.join("current"))
        .and_then(|file| file.take(4097).read_to_end(&mut pointer))
        .map_err(|error| refusal(format!("reading conversion snapshot pointer: {error}")))?;
    if pointer.len() > 4096 {
        return Err(refusal("conversion snapshot pointer is oversized"));
    }
    let pointer = std::str::from_utf8(&pointer)
        .map_err(|_| refusal("conversion snapshot pointer is not UTF-8"))?
        .trim();
    if !matches!(
        Path::new(pointer)
            .components()
            .collect::<Vec<_>>()
            .as_slice(),
        [Component::Normal(_)]
    ) {
        return Err(refusal(
            "conversion snapshot pointer is not one generation name",
        ));
    }
    // Pin this exact immutable generation before snapshot cleanup can unlink it.
    let snapshot = File::open(snapshots.join(pointer))
        .map_err(|error| refusal(format!("opening conversion snapshot: {error}")))?;
    let target = native_target.to_path_buf();
    let cluster_id = expected_cluster_id.to_owned();
    let node_id = expected_node_id.to_owned();
    tokio::task::spawn_blocking(move || {
        export_snapshot(
            snapshot,
            &target,
            &cluster_id,
            &node_id,
            membership.node_id,
            anchor,
        )
    })
    .await
    .map_err(|error| StoreError::Task(format!("joining singleton conversion: {error}")))?
}

async fn pause_before(deadline: tokio::time::Instant) -> Result<(), StoreError> {
    if tokio::time::Instant::now() >= deadline {
        return Err(refusal("conversion cut did not settle before its deadline"));
    }
    tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + Duration::from_millis(25)))
        .await;
    Ok(())
}

fn export_snapshot(
    mut snapshot: File,
    target: &Path,
    cluster_id: &str,
    node_id: &str,
    raft_id: u64,
    minimum_applied: u64,
) -> Result<StandaloneExportReceipt, StoreError> {
    let parent = target
        .parent()
        .ok_or_else(|| refusal("native target has no parent"))?;
    let image = parent.join(format!("standalone-source-{}.sqlite", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options
        .open(&image)
        .map_err(|error| refusal(format!("creating immutable conversion image: {error}")))?;
    std::io::copy(&mut snapshot, &mut output)
        .and_then(|_| output.sync_all())
        .map_err(|error| refusal(format!("copying immutable conversion image: {error}")))?;
    drop(output);
    crate::storage::durability::directory_barrier(parent)
        .map_err(|error| refusal(format!("syncing conversion image directory: {error}")))?;
    let source = Connection::open_with_flags(
        read_only_uri(&image),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    let encoded: Vec<u8> =
        source.query_row("SELECT data FROM _metadata WHERE key='meta'", [], |row| {
            row.get(0)
        })?;
    let (metadata, _): (hiqlite::StateMachineData, usize) =
        bincode::serde::decode_from_slice(&encoded, bincode::config::legacy())
            .map_err(|error| refusal(format!("decoding conversion cut: {error}")))?;
    let applied = metadata
        .last_applied_log_id
        .ok_or_else(|| refusal("conversion snapshot has no applied position"))?;
    if applied.index < minimum_applied {
        return Err(refusal("conversion snapshot predates its acknowledged cut"));
    }
    let membership = metadata.last_membership.membership();
    let configs = membership.get_joint_config();
    if configs.len() != 1
        || configs[0] != BTreeSet::from([raft_id])
        || membership.nodes().count() != 1
        || membership.nodes().any(|(id, node)| {
            *id != raft_id || !loopback(&node.addr_raft) || !loopback(&node.addr_api)
        })
    {
        return Err(refusal(
            "conversion snapshot is not a private uniform singleton",
        ));
    }
    let version = validate_source(&source, cluster_id, node_id, raft_id)?;
    let source_sha256 = super::sha256_file(&image)?;
    drop(source);
    if version == crate::store::AUTH_SCHEMA_BASELINE_VERSION {
        crate::store::SqliteStore::create_native_pre_source_import_target(target)?;
    } else {
        crate::store::SqliteStore::create_native_import_target(target)?;
    }
    let tables = copy_image(&image, target)?;
    let receipt = StandaloneExportReceipt {
        format_version: 1,
        cluster_id: cluster_id.to_owned(),
        node_id: node_id.to_owned(),
        former_raft_id: raft_id,
        applied_term: applied.leader_id.term,
        applied_index: applied.index,
        source_schema_version: version,
        native_schema_version: SQLITE_SCHEMA_VERSION,
        source_image: image,
        source_sha256,
        native_sha256: super::sha256_file(target)?,
        tables,
    };
    let receipt_path = parent.join("standalone-export-receipt.json");
    let bytes = serde_json::to_vec_pretty(&receipt)
        .map_err(|error| refusal(format!("encoding conversion receipt: {error}")))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&receipt_path)
        .map_err(|error| refusal(format!("creating conversion receipt: {error}")))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| refusal(format!("syncing conversion receipt: {error}")))?;
    crate::storage::durability::directory_barrier(parent)
        .map_err(|error| refusal(format!("syncing conversion receipt directory: {error}")))?;
    Ok(receipt)
}

fn loopback(address: &str) -> bool {
    address
        .parse::<SocketAddr>()
        .is_ok_and(|address| address.ip().is_loopback())
}

/// Accept only the two closed Source cohorts, never an inferred version
/// range. This inspects the immutable cut and does not enable Sharing or run
/// a replicated schema factory on the former authority.
fn validate_schema_cohort(source: &Connection) -> Result<i64, StoreError> {
    let version: i64 = source.query_row(
        "SELECT schema_version FROM cluster_meta WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    let guard = crate::store::sharing_source_schema::migration_source_layout_guard(version)
        .ok_or_else(|| refusal("singleton conversion supports only the exact closed predecessor and current Source schema cohorts"))?;
    let ready: bool = source.query_row(&format!("SELECT ({guard})"), [], |row| row.get(0))?;
    if !ready {
        return Err(refusal(
            "singleton Source schema marker does not match its exact closed layout",
        ));
    }
    Ok(version)
}

fn validate_source(
    source: &Connection,
    cluster_id: &str,
    node_id: &str,
    raft_id: u64,
) -> Result<i64, StoreError> {
    crate::storage::ingress_quiescence::require_quiescent_backend_transition(source)?;
    integrity(source)?;
    let version = validate_schema_cohort(source)?;
    let actual: String = source.query_row(
        "SELECT value FROM settings WHERE key='instance.id'",
        [],
        |row| row.get(0),
    )?;
    if actual != cluster_id {
        return Err(refusal("conversion source logical identity does not match"));
    }
    if !table_exists(source, "main", "cluster_nodes")? {
        return Err(refusal(
            "conversion source lacks the admission-history ledger; history is ambiguous",
        ));
    }
    // Include removal tombstones: a formerly clustered member is not a
    // never-peered private install merely because peers are unavailable.
    let peers: i64 = source.query_row(
        "SELECT COUNT(*) FROM cluster_nodes WHERE raft_id IS NULL OR node_id IS NULL OR raft_id<>?1 OR node_id<>?2",
        rusqlite::params![raft_id as i64, node_id],
        |row| row.get(0),
    )?;
    if peers != 0 {
        return Err(refusal(
            "conversion source contains admitted or removed peer history",
        ));
    }
    let restored: bool = source.query_row(
        "SELECT EXISTS(SELECT 1 FROM settings WHERE key=?1)",
        [crate::store::keys::CLUSTER_RESTORE_GENERATION],
        |row| row.get(0),
    )?;
    if restored {
        return Err(refusal(
            "restored cluster history requires explicit decommission/export",
        ));
    }
    for table in [
        "sharing_source_boot_intents",
        "sharing_purpose_census_intents",
    ] {
        if table_exists(source, "main", table)? {
            let foreign: bool = source.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {} WHERE node_id IS NULL OR raft_id IS NULL OR node_id<>?1 OR raft_id<>?2)",
                    quoted(table)
                ),
                rusqlite::params![node_id, raft_id as i64],
                |row| row.get(0),
            )?;
            if foreign {
                return Err(refusal(format!(
                    "conversion has another node's process authority in {table}"
                )));
            }
        }
    }
    for (table, predicate) in [
        ("cluster_join_tokens", "state IN ('redeeming','redeemed')"),
        ("cluster_node_join_staging", "1"),
        ("cluster_node_removals", "1"),
        ("cluster_node_removal_attempts", "1"),
    ] {
        if table_exists(source, "main", table)? {
            let pending: bool = source.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {} WHERE {predicate})",
                    quoted(table)
                ),
                [],
                |row| row.get(0),
            )?;
            if pending {
                return Err(refusal(format!(
                    "conversion source has admission/removal history or intent in {table}"
                )));
            }
        }
    }
    Ok(version)
}

#[derive(Clone, Debug)]
struct Column {
    name: String,
    kind: String,
    hidden: i64,
}

#[derive(Clone, Debug)]
struct Table {
    kind: String,
    without_rowid: bool,
    columns: Vec<Column>,
}

fn inventory(connection: &Connection, schema: &str) -> Result<BTreeMap<String, Table>, StoreError> {
    let mut result = BTreeMap::new();
    let sql = format!(
        "SELECT name,type,wr FROM pragma_table_list WHERE schema={} ORDER BY name",
        literal(schema)
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (name, kind, without_rowid) = row?;
        if name == "sqlite_schema" || name == "sqlite_temp_schema" {
            continue;
        }
        let sql = format!("PRAGMA {}.table_xinfo({})", quoted(schema), literal(&name));
        let mut columns = connection.prepare(&sql)?;
        let columns = columns
            .query_map([], |row| {
                Ok(Column {
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    hidden: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        result.insert(
            name,
            Table {
                kind,
                without_rowid: without_rowid != 0,
                columns,
            },
        );
    }
    Ok(result)
}

const SHARING_PROTOCOL_TABLES: [&str; 5] = [
    "cluster_sharing_join_intents",
    "cluster_sharing_join_declarations",
    "cluster_sharing_membership_intents",
    "cluster_sharing_membership_generation",
    "cluster_sharing_purpose_bootstrap_guard",
];

/// Retiring the replication adapter retires its admission observations, not
/// Source grants, sealed keys or application generations. Pending transitions
/// must be settled by their original owners before the mode can change.
fn validate_sharing_protocol_retirement(source: &Connection) -> Result<(), StoreError> {
    let mut present = false;
    for name in SHARING_PROTOCOL_TABLES {
        let object: bool = source.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
            [name],
            |row| row.get(0),
        )?;
        if object {
            present = true;
        }
    }
    if !present {
        return Ok(());
    }
    let mut expected = BTreeMap::<String, (String, String)>::new();
    for sql in crate::cluster::membership::sharing_member_admission_guard_schema() {
        if !sql.starts_with("CREATE ") {
            continue;
        }
        let sql = sql.replace(" IF NOT EXISTS", "");
        let words = sql.split_whitespace().collect::<Vec<_>>();
        let name = words[2]
            .split('(')
            .next()
            .ok_or_else(|| refusal("invalid compiled sharing control declaration"))?
            .to_owned();
        expected.insert(name, (words[1].to_ascii_lowercase(), sql));
    }
    let bootstrap_present:bool=source.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='cluster_sharing_purpose_bootstrap_guard')",[],|row|row.get(0))?;
    if bootstrap_present {
        expected.insert(
            "cluster_sharing_purpose_bootstrap_guard".into(),
            (
                "table".into(),
                crate::cluster::membership::PURPOSE_BOOTSTRAP_GUARD_SCHEMA.into(),
            ),
        );
    }
    for (name, (kind, sql)) in &expected {
        let exact: bool = source.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1 AND type=?2 AND sql=?3)",
            rusqlite::params![name, kind, sql],
            |row| row.get(0),
        )?;
        if !exact {
            return Err(refusal(format!(
                "sharing control object {name} does not match its canonical closed declaration"
            )));
        }
    }
    for table in SHARING_PROTOCOL_TABLES {
        if !table_exists(source, "main", table)? {
            continue;
        }
        let mut statement = source
            .prepare("SELECT name FROM sqlite_master WHERE tbl_name=?1 AND sql IS NOT NULL")?;
        let names = statement.query_map([table], |row| row.get::<_, String>(0))?;
        for name in names {
            let name = name?;
            if !expected.contains_key(&name) {
                return Err(refusal(format!(
                    "unrecognized durable sharing control object {name}"
                )));
            }
        }
    }
    for table in [
        "cluster_sharing_join_intents",
        "cluster_sharing_join_declarations",
        "cluster_sharing_membership_intents",
    ] {
        let pending: bool = source.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {})", quoted(table)),
            [],
            |row| row.get(0),
        )?;
        if pending {
            return Err(refusal(format!(
                "pending cluster Source admission requires settlement before conversion: {table}"
            )));
        }
    }
    let generation:bool=source.query_row("SELECT count(*)=1 AND coalesce(min(singleton=1 AND generation>=0),0) FROM cluster_sharing_membership_generation",[],|row|row.get(0))?;
    if !generation {
        return Err(refusal(
            "sharing membership generation is not an exact nonnegative singleton",
        ));
    }
    if bootstrap_present {
        let bootstrap:bool=source.query_row("SELECT count(*)=1 AND coalesce(min(singleton=1 AND passed=1),0) FROM cluster_sharing_purpose_bootstrap_guard",[],|row|row.get(0))?;
        if !bootstrap {
            return Err(refusal(
                "sharing purpose bootstrap guard is not its completed singleton",
            ));
        }
    }
    Ok(())
}

fn copy_image(
    source_path: &Path,
    target_path: &Path,
) -> Result<Vec<StandaloneTableTransfer>, StoreError> {
    // Immutable attach deliberately ignores WAL. Accept only the closed cut
    // produced above, never a live caller's database with unapplied sidecars.
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = source_path.as_os_str().to_os_string();
        sidecar.push(suffix);
        match std::fs::symlink_metadata(PathBuf::from(sidecar)) {
            Ok(_) => {
                return Err(refusal(
                    "conversion source must be a closed immutable image without sidecars",
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(refusal(format!(
                    "inspecting conversion source sidecar: {error}"
                )))
            }
        }
    }
    let source_validation = Connection::open_with_flags(
        read_only_uri(source_path),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    validate_sharing_protocol_retirement(&source_validation)?;
    drop(source_validation);
    let connection = Connection::open_with_flags(
        target_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
    )?;
    let native_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if native_version != SQLITE_SCHEMA_VERSION {
        return Err(refusal(
            "conversion target must have the current native schema",
        ));
    }
    if table_exists(&connection, "main", "cluster_meta")? {
        return Err(refusal(
            "native conversion target contains a replicated authority marker",
        ));
    }
    if table_exists(&connection, "main", "settings")? {
        let identity: Option<String> = connection
            .query_row(
                "SELECT value FROM settings WHERE key='instance.id'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if identity.is_some() {
            return Err(refusal(
                "conversion target is already an initialized server",
            ));
        }
    }
    connection.execute(
        "ATTACH DATABASE ?1 AS export_source",
        [read_only_uri(source_path)],
    )?;
    let source = inventory(&connection, "export_source")?;
    let target = inventory(&connection, "main")?;
    let triggers = {
        let mut statement = connection.prepare("SELECT name,sql FROM main.sqlite_schema WHERE type='trigger' AND sql IS NOT NULL ORDER BY name")?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut report = Vec::new();
    let mut plans = Vec::new();
    for (name, table) in &source {
        let (rows, digest) = table_digest(&connection, "export_source", name, table)?;
        let disposition = if retired_cluster_table(name) {
            "retired-cluster-metadata"
        } else if derived_table(name, table) {
            "rebuilt-derived-index"
        } else if let Some(destination) = target.get(name) {
            if !compatible(table, destination) {
                if rows != 0 {
                    return Err(refusal(format!(
                        "unmapped source columns or table shape in {name}"
                    )));
                }
                "empty-incompatible-source"
            } else {
                plans.push(name.clone());
                "copied-native-data"
            }
        } else if rows == 0 {
            "empty-source-only-table"
        } else {
            return Err(refusal(format!(
                "nonempty source table {name} has no native mapping"
            )));
        };
        report.push(StandaloneTableTransfer {
            table: name.clone(),
            disposition: disposition.to_owned(),
            source_columns: table
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
            source_rows: rows,
            source_sha256: digest,
        });
    }
    connection
        .execute_batch("PRAGMA foreign_keys=OFF; PRAGMA synchronous=FULL; BEGIN IMMEDIATE;")?;
    let transaction: Result<(), StoreError> = (|| {
        for (name, _) in &triggers {
            connection.execute_batch(&format!("DROP TRIGGER main.{}", quoted(name)))?;
        }
        for name in &plans {
            let table = &source[name];
            let columns = copy_columns(table)?;
            let columns = columns
                .iter()
                .map(|name| quoted(name))
                .collect::<Vec<_>>()
                .join(",");
            connection.execute_batch(&format!(
                "DELETE FROM main.{name}; INSERT INTO main.{name} ({columns}) SELECT {columns} FROM export_source.{name};",
                name = quoted(name),
            ))?;
        }
        for (_, sql) in &triggers {
            connection.execute_batch(sql)?;
        }
        if table_exists(&connection, "main", "items_fts")? {
            connection.execute("INSERT INTO items_fts(items_fts) VALUES('rebuild')", [])?;
        }
        if table_exists(&connection, "main", "classification_fts")? {
            connection.execute("DELETE FROM classification_fts", [])?;
            connection.execute(&crate::store::classification::rebuild_sql(), [])?;
        }
        for entry in &report {
            if entry.disposition == "copied-native-data" {
                let (rows, digest) =
                    table_digest(&connection, "main", &entry.table, &target[&entry.table])?;
                if rows != entry.source_rows || digest != entry.source_sha256 {
                    return Err(refusal(format!(
                        "native conversion parity failed for {}",
                        entry.table
                    )));
                }
            }
        }
        let dangling: i64 =
            connection.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        if dangling != 0 {
            return Err(refusal("native conversion has dangling foreign keys"));
        }
        integrity(&connection)?;
        connection.execute_batch("COMMIT;")?;
        Ok(())
    })();
    if transaction.is_err() {
        let _ = connection.execute_batch("ROLLBACK;");
    }
    transaction?;
    connection.execute_batch("DETACH DATABASE export_source; PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
    drop(connection);
    crate::storage::durability::sync_regular(target_path)
        .map_err(|error| refusal(format!("syncing converted native database: {error}")))?;
    Ok(report)
}

fn compatible(source: &Table, target: &Table) -> bool {
    source.kind == "table"
        && target.kind == "table"
        && source.without_rowid == target.without_rowid
        && source.columns.len() == target.columns.len()
        && source.columns.iter().all(|column| {
            target.columns.iter().any(|other| {
                column.name == other.name
                    && column.kind.eq_ignore_ascii_case(&other.kind)
                    && column.hidden == other.hidden
            })
        })
}

fn copy_columns(table: &Table) -> Result<Vec<String>, StoreError> {
    let mut columns = table
        .columns
        .iter()
        .filter(|column| column.hidden == 0)
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    if !table.without_rowid {
        let alias = ["_rowid_", "rowid", "oid"]
            .into_iter()
            .find(|alias| !columns.iter().any(|column| column == alias))
            .ok_or_else(|| refusal("source table hides every implicit row identity alias"))?;
        columns.insert(0, alias.to_owned());
    }
    Ok(columns)
}

fn table_digest(
    connection: &Connection,
    schema: &str,
    name: &str,
    table: &Table,
) -> Result<(u64, String), StoreError> {
    let mut columns = table
        .columns
        .iter()
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    columns.sort();
    if table.kind == "table" && !table.without_rowid {
        let alias = ["_rowid_", "rowid", "oid"]
            .into_iter()
            .find(|alias| !columns.iter().any(|column| column == alias))
            .ok_or_else(|| refusal("source table hides every implicit row identity alias"))?;
        columns.insert(0, alias.to_owned());
    }
    let projection = columns
        .iter()
        .map(|name| quoted(name))
        .collect::<Vec<_>>()
        .join(",");
    // A real row identity is unique and indexed, so ordinary large domain
    // tables stream in primary storage order without sorting wide probe/blob
    // payloads. WITHOUT ROWID/derived tables use typed canonical row order.
    let order = if table.kind == "table" && !table.without_rowid {
        quoted(&columns[0])
    } else {
        columns
            .iter()
            .map(|name| format!("{} COLLATE BINARY", quoted(name)))
            .collect::<Vec<_>>()
            .join(",")
    };
    let sql = format!(
        "SELECT {projection} FROM {}.{} ORDER BY {order}",
        quoted(schema),
        quoted(name)
    );
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query([])?;
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    while let Some(row) = rows.next()? {
        digest.update([0xff]);
        for index in 0..columns.len() {
            match row.get_ref(index)? {
                ValueRef::Null => digest.update([0]),
                ValueRef::Integer(value) => {
                    digest.update([1]);
                    digest.update(value.to_be_bytes());
                }
                ValueRef::Real(value) => {
                    digest.update([2]);
                    digest.update(value.to_bits().to_be_bytes());
                }
                ValueRef::Text(value) => {
                    digest.update([3]);
                    digest.update((value.len() as u64).to_be_bytes());
                    digest.update(value);
                }
                ValueRef::Blob(value) => {
                    digest.update([4]);
                    digest.update((value.len() as u64).to_be_bytes());
                    digest.update(value);
                }
            }
        }
        count = count
            .checked_add(1)
            .ok_or_else(|| refusal("conversion row count overflow"))?;
    }
    Ok((count, hex::encode(digest.finalize())))
}

fn table_exists(connection: &Connection, schema: &str, name: &str) -> Result<bool, StoreError> {
    connection
        .query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM {}.sqlite_schema WHERE type='table' AND name=?1)",
                quoted(schema)
            ),
            [name],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn derived_table(name: &str, table: &Table) -> bool {
    matches!(name, "sqlite_stat1" | "sqlite_stat4")
        || (table.kind == "virtual"
            && matches!(name, "items_fts" | "items_fts_vocab" | "classification_fts"))
        || (table.kind == "shadow"
            && matches!(
                name,
                "items_fts_data"
                    | "items_fts_idx"
                    | "items_fts_docsize"
                    | "items_fts_config"
                    | "classification_fts_data"
                    | "classification_fts_idx"
                    | "classification_fts_content"
                    | "classification_fts_docsize"
                    | "classification_fts_config"
            ))
}

// These are cluster protocol/transport facts, not application rows. Their
// complete original values remain in the immutable source image and their
// typed digests remain in the conversion receipt. Domain tables whose names
// happen to start with `cluster_` are deliberately not covered by this list.
fn retired_cluster_table(name: &str) -> bool {
    matches!(
        name,
        "_metadata"
            | "sharing_source_boot_intents"
            | "sharing_purpose_census_intents"
            | "cluster_meta"
            | "cluster_sharing_join_intents"
            | "cluster_sharing_join_declarations"
            | "cluster_sharing_membership_intents"
            | "cluster_sharing_membership_generation"
            | "cluster_sharing_purpose_bootstrap_guard"
            | "cluster_membership_meta"
            | "cluster_join_tokens"
            | "cluster_learner_join_intents"
            | "cluster_nodes"
            | "cluster_node_removals"
            | "cluster_node_removal_attempts"
            | "cluster_node_removal_intents"
            | "cluster_node_capabilities"
            | "cluster_node_progress"
            | "cluster_node_promotions"
            | "cluster_node_maintenance"
            | "cluster_operation_leases"
            | "cluster_operation_lease_releases"
            | "cluster_operation_lease_intents"
            | "cluster_cache_admin_revocation_leases"
            | "cluster_cache_admin_revocation_lease_releases"
            | "cluster_cache_admin_revocation_release_watermark"
            | "cluster_cache_admin_revocation_lease_intents"
            | "cluster_credential_mutation_intents"
            | "cluster_credential_guard_activation"
            | "cluster_node_maintenance_heartbeat_intents"
            | "cluster_node_heartbeat_intents"
            | "cluster_node_join_staging"
            | "cluster_live_tv_join_intents"
            | "cluster_live_tv_activation"
            | "cluster_node_http"
            | "cluster_node_http_claims"
            | "cluster_node_hostnames"
            | "cluster_node_activity_keys"
    )
}

fn integrity(connection: &Connection) -> Result<(), StoreError> {
    let result: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if result != "ok" {
        return Err(refusal("conversion image failed SQLite integrity_check"));
    }
    Ok(())
}

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn read_only_uri(path: &Path) -> String {
    let mut encoded = String::from("file:");
    for byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(byte) {
            encoded.push(char::from(*byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded.push_str("?mode=ro&immutable=1");
    encoded
}

fn cluster_error(error: hiqlite::Error) -> StoreError {
    StoreError::Database(error.to_string())
}
fn refusal(message: impl Into<String>) -> StoreError {
    StoreError::Migration(message.into())
}

#[cfg(test)]
#[path = "standalone_export_tests.rs"]
mod tests;
