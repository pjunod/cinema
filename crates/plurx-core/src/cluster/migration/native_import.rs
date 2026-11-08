//! Current native SQLite -> fresh replicated storage, through real Raft writes.
//!
//! This deliberately does not relax the legacy importer's lineage guard. The
//! coordinator owns a closed, verified native snapshot and a fresh incoming
//! voter; neither becomes authoritative until complete row parity succeeds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use hiqlite::macros::params;
use hiqlite::{Client, Param, Params, Row};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::storage::mode::{StorageManifest, StorageMode};
use crate::store::{SqliteImportTableDigest, SQLITE_SCHEMA_VERSION};

const PAGE_ROWS: usize = 16;
const TRANSACTION_BYTES: usize = 512 * 1024;
const ENTRY_RESERVE: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeImportDisposition {
    pub table: String,
    pub columns: Vec<String>,
    pub disposition: String,
    pub rows: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeImportReceipt {
    pub version: u32,
    pub source_schema_version: i64,
    pub replicated_schema_version: i64,
    pub source_sha256: String,
    pub source_storage_generation: String,
    pub imported_rows: u64,
    pub tables: Vec<SqliteImportTableDigest>,
    pub census: Vec<NativeImportDisposition>,
}

#[derive(Clone, Debug)]
struct Column {
    name: String,
    kind: String,
    pk: i64,
    hidden: i64,
}

#[derive(Clone, Debug)]
struct Table {
    name: String,
    kind: String,
    without_rowid: bool,
    columns: Vec<Column>,
    parents: BTreeSet<String>,
    row_count: u64,
    digest: String,
}

#[derive(Clone)]
struct SourceInventory {
    path: PathBuf,
    tables: BTreeMap<String, Table>,
    instance_id: String,
    principal_layout: bool,
}

/// The supplied snapshot must be immutable/closed and originate in the exact
/// native generation recorded by `source_manifest`. All target writes, even
/// maintenance DDL, are committed through the actual incoming Raft client.
pub async fn import_verified_native(
    client: &Client,
    source_path: &Path,
    expected_sha256: &str,
    source_manifest: &StorageManifest,
) -> Result<NativeImportReceipt, StoreError> {
    let path = source_path.to_path_buf();
    let expected = expected_sha256.to_owned();
    let manifest = source_manifest.clone();
    let source = tokio::task::spawn_blocking(move || inspect_source(&path, &expected, &manifest))
        .await
        .map_err(task_error)??;
    for table in ["users", "items", "files"] {
        if target_count(client, table).await? != 0 {
            return Err(invalid(format!(
                "native import target already has application rows in {table}"
            )));
        }
    }
    let actual = client
        .local_db_raft_metrics()
        .map_err(cluster_error)?
        .membership_snapshot();
    if !actual.running
        || !actual.committed
        || actual.members != BTreeSet::from([actual.node_id])
        || actual.voters != BTreeSet::from([actual.node_id])
    {
        return Err(invalid(
            "native import requires an actual fresh singleton voter",
        ));
    }
    crate::cluster::membership::prepare_fresh_native_import_membership_schema(
        client,
        actual.node_id,
    )
    .await
    .map_err(|error| invalid(error.to_string()))?;
    crate::cluster::membership::register_singleton_observation_identity(
        client,
        &source_manifest.node_id,
        actual.node_id,
    )
    .await
    .map_err(|error| invalid(error.to_string()))?;
    if source.principal_layout {
        crate::store::sharing_source_schema::prepare_fresh_cluster_for_native_import(
            client,
            &source_manifest.instance_id,
            &source_manifest.node_id,
            actual.node_id,
        )
        .await?;
    } else if source
        .tables
        .contains_key("sharing_purpose_key_installation")
    {
        for sql in crate::store::sharing_purpose_keys::replicated_import_material_statements() {
            write(client, vec![(sql, params!())]).await?;
        }
    }

    let target = target_inventory(client).await?;
    let (mut plans, mut census) = plan_tables(&source.tables, &target)?;
    for table in &plans {
        let count = target_count(client, &table.name).await?;
        if count != 0 && !fresh_seed(client, table, count, &source.instance_id).await? {
            return Err(invalid(format!(
                "native import target is not fresh in {}",
                table.name
            )));
        }
    }
    plans = order_tables(plans)?;
    let triggers = target_json(client,
        "SELECT json_array(name,sql) AS payload FROM sqlite_schema WHERE type='trigger' AND sql IS NOT NULL ORDER BY name".into(),params!(),
    ).await?;
    let triggers = triggers
        .into_iter()
        .map(|value| Ok((json_string(&value, 0)?, json_string(&value, 1)?)))
        .collect::<Result<Vec<_>, StoreError>>()?;
    // The entire target is disposable until its parity receipt is published.
    // A failed/crashed import is discarded, never served with missing guards.
    for (name, _) in &triggers {
        write(
            client,
            vec![(format!("DROP TRIGGER {}", quoted(name)), params!())],
        )
        .await?;
    }
    if target.contains_key("item_identity_watermark") {
        write(client,vec![("UPDATE item_identity_watermark SET importing=1 WHERE singleton=1 AND high_water=0 AND importing=0".into(),params!())]).await?;
    }
    for table in plans.iter().rev() {
        if table.name != "item_identity_watermark" {
            write(
                client,
                vec![(format!("DELETE FROM {}", quoted(&table.name)), params!())],
            )
            .await?;
        }
    }
    let mut imported_rows = 0_u64;
    for table in &plans {
        if table.name == "item_identity_watermark" || table.name == "sqlite_sequence" {
            continue;
        }
        copy_table(client, &source.path, table).await?;
        imported_rows = imported_rows
            .checked_add(table.row_count)
            .ok_or_else(|| invalid("imported row count overflow"))?;
    }
    // Deleted IDs remain unavailable. This is not derivable from MAX(id).
    if let Some(sequence) = plans.iter().find(|table| table.name == "sqlite_sequence") {
        write(
            client,
            vec![("DELETE FROM sqlite_sequence".into(), params!())],
        )
        .await?;
        copy_table(client, &source.path, sequence).await?;
        imported_rows = imported_rows
            .checked_add(sequence.row_count)
            .ok_or_else(|| invalid("imported row count overflow"))?;
    }
    if let Some(watermark) = plans
        .iter()
        .find(|table| table.name == "item_identity_watermark")
    {
        write(
            client,
            vec![("DELETE FROM item_identity_watermark".into(), params!())],
        )
        .await?;
        copy_table(client, &source.path, watermark).await?;
        imported_rows = imported_rows
            .checked_add(watermark.row_count)
            .ok_or_else(|| invalid("imported row count overflow"))?;
    }
    for (_, sql) in triggers {
        write(client, vec![(sql, params!())]).await?;
    }
    if target.contains_key("items_fts") {
        // The replicated catalogue has a contentless FTS index. Its rebuild
        // command is unsupported; reconstruct from the copied durable rows.
        write(client, vec![("DELETE FROM items_fts".into(), params!())]).await?;
        write(client,vec![("INSERT INTO items_fts(rowid,title,overview,tags) SELECT id,title,overview,tags FROM items".into(),params!())]).await?;
    }
    if target.contains_key("classification_fts") {
        write(
            client,
            vec![("DELETE FROM classification_fts".into(), params!())],
        )
        .await?;
        write(
            client,
            vec![(crate::store::classification::rebuild_sql(), params!())],
        )
        .await?;
    }
    if source
        .tables
        .get("sharing_source_schema_installation")
        .is_some_and(|table| table.row_count > 0)
    {
        let installed = target_json(
            client,
            format!(
                "SELECT json_array(CASE WHEN ({}) THEN 1 ELSE 0 END) AS payload",
                crate::store::sharing_source_schema::installed_guard()
            ),
            params!(),
        )
        .await?;
        if one_integer(&installed)? != 1 {
            return Err(invalid(
                "replicated Source installation failed its complete schema guard",
            ));
        }
    }
    let dangling = target_json(
        client,
        "SELECT json_array(count(*)) AS payload FROM pragma_foreign_key_check".into(),
        params!(),
    )
    .await?;
    if one_integer(&dangling)? != 0 {
        return Err(invalid(
            "replicated native import has dangling foreign keys",
        ));
    }
    let mut tables = Vec::new();
    for table in &plans {
        let (count, digest) = target_digest(client, table).await?;
        if count != table.row_count || digest != table.digest {
            return Err(invalid(format!(
                "native import parity mismatch in {}",
                table.name
            )));
        }
        tables.push(SqliteImportTableDigest {
            table: table.name.clone(),
            row_count: count,
            sha256: digest,
        });
    }
    let path = source.path.clone();
    let hash = tokio::task::spawn_blocking(move || super::sha256_file(&path))
        .await
        .map_err(task_error)??;
    if hash != expected_sha256 {
        return Err(invalid("native source changed during import"));
    }
    census.sort_by(|a, b| a.table.cmp(&b.table));
    let final_membership = client
        .local_db_raft_metrics()
        .map_err(cluster_error)?
        .membership_snapshot();
    if !final_membership.running
        || !final_membership.committed
        || final_membership.node_id != actual.node_id
        || final_membership.members != BTreeSet::from([actual.node_id])
        || final_membership.voters != BTreeSet::from([actual.node_id])
    {
        return Err(invalid(
            "native import target membership changed before parity publication",
        ));
    }
    let schema = target_json(
        client,
        "SELECT json_array(schema_version) AS payload FROM cluster_meta WHERE singleton=1".into(),
        params!(),
    )
    .await?;
    let replicated_schema_version = one_integer(&schema)?;
    Ok(NativeImportReceipt {
        version: 1,
        source_schema_version: SQLITE_SCHEMA_VERSION,
        replicated_schema_version,
        source_sha256: hash,
        source_storage_generation: source_manifest.generation.clone(),
        imported_rows,
        tables,
        census,
    })
}

fn inspect_source(
    path: &Path,
    expected: &str,
    manifest: &StorageManifest,
) -> Result<SourceInventory, StoreError> {
    if manifest.version != 1
        || manifest.mode != StorageMode::Sqlite
        || manifest.schema_version != SQLITE_SCHEMA_VERSION
        || uuid::Uuid::parse_str(&manifest.generation).is_err()
        || super::sha256_file(path)? != expected
    {
        return Err(invalid(
            "native source manifest, schema or immutable image hash mismatch",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("native source has no owned parent"))?;
    if crate::storage::mode::owned_path(parent, &manifest.database)? != path {
        return Err(invalid("native source is not the manifest database"));
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| invalid(format!("native image metadata: {error}")))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid("native source is not an owned regular image"));
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
        if std::fs::symlink_metadata(&sidecar).is_ok() {
            return Err(invalid(
                "native source must be a closed standalone snapshot without sidecars",
            ));
        }
    }
    manifest.validate_sqlite(parent)?;
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let identity: (String, String, String) = connection.query_row(
        "SELECT instance_id,node_id,generation FROM storage_identity WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let instance: String = connection.query_row(
        "SELECT value FROM settings WHERE key='instance.id'",
        [],
        |row| row.get(0),
    )?;
    if version != SQLITE_SCHEMA_VERSION
        || identity
            != (
                manifest.instance_id.clone(),
                manifest.node_id.clone(),
                manifest.generation.clone(),
            )
        || instance != manifest.instance_id
    {
        return Err(invalid(
            "native image belongs to another identity or storage generation",
        ));
    }
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    let dangling: i64 =
        connection.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    if integrity != "ok" || dangling != 0 {
        return Err(invalid(
            "native image failed integrity/foreign-key verification",
        ));
    }
    crate::storage::ingress_quiescence::require_quiescent_backend_transition(&connection)?;
    let mut tables = source_inventory(&connection)?;
    for table in tables.values_mut() {
        let (count, digest) = source_digest(&connection, table)?;
        table.row_count = count;
        table.digest = digest;
    }
    if let Some(table) = tables.get("watch_state") {
        if table
            .columns
            .iter()
            .any(|column| column.name == "manual_origin")
        {
            let active: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM watch_state WHERE manual_origin IS NOT NULL)",
                [],
                |row| row.get(0),
            )?;
            if active {
                return Err(invalid(
                    "native image contains unfinished watch mutation context",
                ));
            }
        }
    }
    let families = crate::store::sharing_source_schema::FAMILIES;
    let principal_count = families
        .iter()
        .filter(|name| {
            tables.get(**name).is_some_and(|table| {
                table
                    .columns
                    .iter()
                    .any(|column| column.name == "principal_kind")
            })
        })
        .count();
    if principal_count != 0 && principal_count != families.len() {
        return Err(invalid("native Source session layout is partial"));
    }
    let principal_layout = principal_count == families.len();
    if tables
        .get("sharing_source_schema_installation")
        .is_some_and(|table| table.row_count > 0)
    {
        let ready: bool = connection.query_row(
            &format!(
                "SELECT ({})",
                crate::store::sharing_source_schema::native_installed_guard()
            ),
            [],
            |row| row.get(0),
        )?;
        if !ready {
            return Err(invalid(
                "native Source installation is incomplete or belongs to another generation",
            ));
        }
    }
    Ok(SourceInventory {
        path: path.to_path_buf(),
        tables,
        instance_id: instance,
        principal_layout,
    })
}

fn source_inventory(connection: &Connection) -> Result<BTreeMap<String, Table>, StoreError> {
    let mut statement = connection
        .prepare("SELECT name,type,wr FROM pragma_table_list WHERE schema='main' ORDER BY name")?;
    let entries = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut tables = BTreeMap::new();
    for (name, kind, wr) in entries {
        if name == "sqlite_schema" {
            continue;
        }
        let mut statement =
            connection.prepare(&format!("PRAGMA table_xinfo({})", literal(&name)))?;
        let columns = statement
            .query_map([], |row| {
                Ok(Column {
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    pk: row.get(5)?,
                    hidden: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut statement =
            connection.prepare(&format!("PRAGMA foreign_key_list({})", literal(&name)))?;
        let parents = statement
            .query_map([], |row| row.get::<_, String>(2))?
            .collect::<rusqlite::Result<BTreeSet<_>>>()?;
        tables.insert(
            name.clone(),
            Table {
                name,
                kind,
                without_rowid: wr != 0,
                columns,
                parents,
                row_count: 0,
                digest: String::new(),
            },
        );
    }
    Ok(tables)
}

async fn target_inventory(client: &Client) -> Result<BTreeMap<String, Table>, StoreError> {
    let entries=target_json(client,"SELECT json_array(name,type,wr) AS payload FROM pragma_table_list WHERE schema='main' ORDER BY name".into(),params!()).await?;
    let mut tables = BTreeMap::new();
    for entry in entries {
        let name = json_string(&entry, 0)?;
        if name == "sqlite_schema" {
            continue;
        }
        let kind = json_string(&entry, 1)?;
        let without_rowid = json_integer(&entry, 2)? != 0;
        let columns=target_json(client,format!("SELECT json_array(name,type,pk,hidden) AS payload FROM pragma_table_xinfo({}) ORDER BY cid",literal(&name)),params!()).await?;
        let columns = columns
            .into_iter()
            .map(|column| {
                Ok(Column {
                    name: json_string(&column, 0)?,
                    kind: json_string(&column, 1)?,
                    pk: json_integer(&column, 2)?,
                    hidden: json_integer(&column, 3)?,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        let parents = target_json(
            client,
            format!(
                "SELECT json_array(\"table\") AS payload FROM pragma_foreign_key_list({})",
                literal(&name)
            ),
            params!(),
        )
        .await?;
        let parents = parents
            .into_iter()
            .map(|value| json_string(&value, 0))
            .collect::<Result<BTreeSet<_>, StoreError>>()?;
        tables.insert(
            name.clone(),
            Table {
                name,
                kind,
                without_rowid,
                columns,
                parents,
                row_count: 0,
                digest: String::new(),
            },
        );
    }
    Ok(tables)
}

fn plan_tables(
    source: &BTreeMap<String, Table>,
    target: &BTreeMap<String, Table>,
) -> Result<(Vec<Table>, Vec<NativeImportDisposition>), StoreError> {
    let mut plans = Vec::new();
    let mut census = Vec::new();
    for table in source.values() {
        let disposition = if cluster_control(&table.name) {
            if table.row_count != 0 {
                return Err(invalid(format!(
                    "native image contains foreign cluster authority in {}",
                    table.name
                )));
            }
            "empty-foreign-control-table-not-imported"
        } else if local_observation(&table.name) {
            validate_local_observation(table)?;
            "retired-local-observation"
        } else if local_telemetry(&table.name) {
            "copied-node-local-telemetry"
        } else if derived(table) {
            "rebuilt-derived-index"
        } else if let Some(destination) = target.get(&table.name) {
            if compatible(table, destination) {
                let mut plan = table.clone();
                plan.parents = destination.parents.clone();
                plans.push(plan);
                "copied-replicated-data"
            } else if table.row_count == 0 {
                "empty-incompatible-source"
            } else {
                return Err(invalid(format!(
                    "native columns lack an exact replicated mapping in {}",
                    table.name
                )));
            }
        } else if table.row_count == 0 {
            "empty-native-only-table"
        } else {
            return Err(invalid(format!(
                "nonempty native domain table {} has no replicated mapping",
                table.name
            )));
        };
        census.push(NativeImportDisposition {
            table: table.name.clone(),
            columns: table
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
            disposition: disposition.into(),
            rows: table.row_count,
            sha256: table.digest.clone(),
        });
    }
    Ok((plans, census))
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
                    && column.pk == other.pk
                    && column.hidden == other.hidden
            })
        })
}

fn order_tables(mut tables: Vec<Table>) -> Result<Vec<Table>, StoreError> {
    let names = tables
        .iter()
        .map(|table| table.name.clone())
        .collect::<BTreeSet<_>>();
    let mut done = BTreeSet::new();
    let mut ordered = Vec::new();
    while !tables.is_empty() {
        let next = tables.iter().position(|table| {
            table.row_count == 0
                || table.parents.iter().all(|parent| {
                    parent == &table.name || !names.contains(parent) || done.contains(parent)
                })
        });
        let Some(next) = next else {
            return Err(invalid("nonempty inter-table dependency cycle must drain or receive an explicit bounded import mapping"));
        };
        let table = tables.remove(next);
        done.insert(table.name.clone());
        ordered.push(table);
    }
    Ok(ordered)
}

async fn fresh_seed(
    client: &Client,
    table: &Table,
    count: u64,
    expected_instance: &str,
) -> Result<bool, StoreError> {
    if table.name == "settings" {
        let rows = target_json(
            client,
            "SELECT json_array(key,value) AS payload FROM settings ORDER BY key".into(),
            params!(),
        )
        .await?;
        return Ok(rows.len() == 1
            && json_string(&rows[0], 0)? == "instance.id"
            && json_string(&rows[0], 1)? == expected_instance);
    }
    if table.name == "sqlite_sequence" {
        let rows = target_json(
            client,
            "SELECT json_array(count(*)) AS payload FROM sqlite_sequence WHERE seq<>0".into(),
            params!(),
        )
        .await?;
        return Ok(one_integer(&rows)? == 0);
    }
    if count != 1 {
        return Ok(false);
    }
    let predicate = match table.name.as_str() {
        "background_job_migration" => "source_count=0",
        "item_identity_watermark" => "singleton=1 AND high_water=0 AND importing=0",
        "live_tv_resource_revision" => "singleton=1 AND revision=0 AND nonce=''",
        "playback_input_generation" => "singleton=1 AND generation=0",
        name if name.ends_with("_schema")
            && table.columns.iter().all(|column| {
                matches!(
                    column.name.as_str(),
                    "singleton" | "version" | "schema_version"
                )
            }) =>
        {
            "singleton=1"
        }
        _ => return Ok(false),
    };
    let rows = target_json(
        client,
        format!(
            "SELECT json_array(count(*)) AS payload FROM {} WHERE {predicate}",
            quoted(&table.name)
        ),
        params!(),
    )
    .await?;
    Ok(one_integer(&rows)? == 1)
}

async fn copy_table(client: &Client, path: &Path, table: &Table) -> Result<(), StoreError> {
    let columns = write_columns(table)?;
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quoted(&table.name),
        columns
            .iter()
            .map(|name| quoted(name))
            .collect::<Vec<_>>()
            .join(","),
        (1..=columns.len())
            .map(|index| format!("${index}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    if table.name == "items"
        && table
            .columns
            .iter()
            .any(|column| column.name == "parent_id")
    {
        let source_path = path.to_path_buf();
        let ids = tokio::task::spawn_blocking(move || parent_first_items(&source_path))
            .await
            .map_err(task_error)??;
        for ids in ids.chunks(PAGE_ROWS) {
            let path = path.to_path_buf();
            let table = table.clone();
            let ids = ids.to_vec();
            let rows = tokio::task::spawn_blocking(move || read_items(&path, &table, &ids))
                .await
                .map_err(task_error)??;
            write_rows(client, &sql, rows).await?;
        }
        return Ok(());
    }
    let mut cursor = None;
    loop {
        let path = path.to_path_buf();
        let plan = table.clone();
        let last = cursor.clone();
        let chunk = tokio::task::spawn_blocking(move || source_page(&path, &plan, last))
            .await
            .map_err(task_error)??;
        if chunk.rows.is_empty() {
            break;
        }
        cursor = chunk.cursor;
        write_rows(client, &sql, chunk.rows).await?;
    }
    Ok(())
}

struct Chunk {
    rows: Vec<Params>,
    cursor: Option<Params>,
}

fn source_page(path: &Path, table: &Table, cursor: Option<Params>) -> Result<Chunk, StoreError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let columns = write_columns(table)?;
    let keys = row_keys(table)?;
    let (filter, bindings) = cursor_filter(&keys, cursor);
    let sql = format!(
        "SELECT {} FROM {} {filter} ORDER BY {} LIMIT {PAGE_ROWS}",
        columns
            .iter()
            .map(|name| quoted(name))
            .collect::<Vec<_>>()
            .join(","),
        quoted(&table.name),
        keys.iter()
            .map(|name| quoted(name))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut statement = connection.prepare(&sql)?;
    let binding_values = bindings
        .iter()
        .map(sqlite_value)
        .collect::<Result<Vec<_>, _>>()?;
    let mut rows = statement.query(rusqlite::params_from_iter(binding_values))?;
    let mut result = Vec::new();
    let mut cursor = None;
    let mut bytes = 0;
    while let Some(row) = rows.next()? {
        let values = (0..columns.len())
            .map(|index| value_param(row.get_ref(index)?))
            .collect::<rusqlite::Result<Params>>()?;
        cursor = Some(
            keys.iter()
                .map(|key| {
                    columns
                        .iter()
                        .position(|name| name == key)
                        .map(|index| values[index].clone())
                        .ok_or_else(|| invalid("import row cursor is not projected"))
                })
                .collect::<Result<Vec<_>, _>>()?,
        );
        bytes += encoded_bytes(&values)?;
        result.push(values);
        if bytes >= TRANSACTION_BYTES {
            break;
        }
    }
    Ok(Chunk {
        rows: result,
        cursor,
    })
}

fn parent_first_items(path: &Path) -> Result<Vec<i64>, StoreError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement=connection.prepare("WITH RECURSIVE ordered(id,depth) AS (SELECT id,0 FROM items WHERE parent_id IS NULL UNION ALL SELECT item.id,parent.depth+1 FROM items item JOIN ordered parent ON item.parent_id=parent.id) SELECT id FROM ordered ORDER BY depth,id")?;
    let ids = statement
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let count: i64 = connection.query_row("SELECT count(*) FROM items", [], |row| row.get(0))?;
    if usize::try_from(count).ok() != Some(ids.len()) {
        return Err(invalid("items contain an unrooted parent cycle"));
    }
    Ok(ids)
}

fn read_items(path: &Path, table: &Table, ids: &[i64]) -> Result<Vec<Params>, StoreError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let columns = write_columns(table)?;
    let sql = format!(
        "SELECT {} FROM items WHERE id=?1",
        columns
            .iter()
            .map(|name| quoted(name))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut statement = connection.prepare(&sql)?;
    ids.iter()
        .map(|id| {
            statement
                .query_row([id], |row| {
                    (0..columns.len())
                        .map(|index| value_param(row.get_ref(index)?))
                        .collect::<rusqlite::Result<Params>>()
                })
                .map_err(Into::into)
        })
        .collect()
}

async fn write_rows(client: &Client, sql: &str, rows: Vec<Params>) -> Result<(), StoreError> {
    let mut batch = Vec::new();
    let mut bytes = 256;
    for row in rows {
        let row_bytes = sql.len() + 32 + encoded_bytes(&row)?;
        if row_bytes + 256 + ENTRY_RESERVE > super::HIQLITE_WAL_USABLE_PAYLOAD_BYTES {
            return Err(invalid(
                "native row exceeds the replicated WAL's indivisible entry bound",
            ));
        }
        if !batch.is_empty() && bytes + row_bytes > TRANSACTION_BYTES {
            insert_batch(client, std::mem::take(&mut batch)).await?;
            bytes = 256;
        }
        bytes += row_bytes;
        batch.push((sql.to_owned(), row));
    }
    if !batch.is_empty() {
        insert_batch(client, batch).await?;
    }
    Ok(())
}

async fn insert_batch(client: &Client, batch: Vec<(String, Params)>) -> Result<(), StoreError> {
    let results = client.txn(batch).await.map_err(cluster_error)?;
    if results.into_iter().any(|result| !matches!(result, Ok(1))) {
        return Err(invalid(
            "native row insert did not acknowledge exactly one row",
        ));
    }
    Ok(())
}

async fn write(client: &Client, batch: Vec<(String, Params)>) -> Result<(), StoreError> {
    let bytes = batch.iter().try_fold(256usize, |total, (sql, values)| {
        total
            .checked_add(sql.len() + 32 + encoded_bytes(values)?)
            .ok_or_else(|| invalid("transaction byte count overflow"))
    })?;
    if bytes + ENTRY_RESERVE > super::HIQLITE_WAL_USABLE_PAYLOAD_BYTES {
        return Err(invalid(
            "native maintenance transaction exceeds the replicated WAL entry bound",
        ));
    }
    for result in client.txn(batch).await.map_err(cluster_error)? {
        result.map_err(cluster_error)?;
    }
    Ok(())
}

fn source_digest(connection: &Connection, table: &Table) -> Result<(u64, String), StoreError> {
    let keys = row_keys(table)?;
    let sql = format!(
        "SELECT {} FROM {} ORDER BY {}",
        canonical_row(table)?,
        quoted(&table.name),
        keys.iter()
            .map(|name| quoted(name))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query([])?;
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    while let Some(row) = rows.next()? {
        let payload: String = row.get(0)?;
        let payload: serde_json::Value =
            serde_json::from_str(&payload).map_err(|error| invalid(error.to_string()))?;
        let payload =
            serde_json::to_string(&payload).map_err(|error| invalid(error.to_string()))?;
        push_digest(&mut digest, &payload);
        count += 1;
    }
    Ok((count, hex::encode(digest.finalize())))
}

async fn target_digest(client: &Client, table: &Table) -> Result<(u64, String), StoreError> {
    let keys = row_keys(table)?;
    let mut cursor = None;
    let mut count = 0_u64;
    let mut digest = Sha256::new();
    loop {
        let (filter, bindings) = cursor_filter(&keys, cursor);
        let key_json = typed_array(&keys);
        let sql=format!("SELECT json_array({}, {key_json}) AS payload FROM {} {filter} ORDER BY {} LIMIT {PAGE_ROWS}",canonical_row(table)?,quoted(&table.name),keys.iter().map(|name|quoted(name)).collect::<Vec<_>>().join(","));
        let rows = target_json(client, sql, bindings).await?;
        if rows.is_empty() {
            break;
        }
        let mut next = None;
        for row in rows {
            let payload = serde_json::to_string(
                row.get(0)
                    .ok_or_else(|| invalid("target parity row is absent"))?,
            )
            .map_err(|error| invalid(error.to_string()))?;
            push_digest(&mut digest, &payload);
            count += 1;
            next = Some(decode_params(
                row.get(1)
                    .ok_or_else(|| invalid("target parity cursor is absent"))?,
            )?);
        }
        cursor = next;
    }
    Ok((count, hex::encode(digest.finalize())))
}

fn canonical_row(table: &Table) -> Result<String, StoreError> {
    let mut columns = table
        .columns
        .iter()
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    columns.sort();
    if table.kind == "table" && !table.without_rowid {
        columns.insert(0, row_keys(table)?[0].clone());
    }
    Ok(typed_array(&columns))
}

fn typed_array(columns: &[String]) -> String {
    format!("json_array({})",columns.iter().map(|name|{
        let column=quoted(name);
        format!("json_array(typeof({column}),CASE typeof({column}) WHEN 'blob' THEN hex({column}) WHEN 'integer' THEN CAST({column} AS TEXT) WHEN 'real' THEN printf('%!.17g',{column}) ELSE {column} END)")
    }).collect::<Vec<_>>().join(","))
}

fn row_keys(table: &Table) -> Result<Vec<String>, StoreError> {
    if table.kind == "table" && !table.without_rowid {
        let key = ["_rowid_", "rowid", "oid"]
            .into_iter()
            .find(|alias| !table.columns.iter().any(|column| column.name == *alias))
            .ok_or_else(|| invalid("table hides every implicit row identity"))?;
        return Ok(vec![key.to_owned()]);
    }
    let mut keys = table
        .columns
        .iter()
        .filter(|column| column.pk > 0)
        .collect::<Vec<_>>();
    keys.sort_by_key(|column| column.pk);
    if keys.is_empty() {
        return Ok(table
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect());
    }
    Ok(keys.into_iter().map(|column| column.name.clone()).collect())
}

fn write_columns(table: &Table) -> Result<Vec<String>, StoreError> {
    let mut columns = table
        .columns
        .iter()
        .filter(|column| column.hidden == 0)
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    if !table.without_rowid {
        columns.insert(0, row_keys(table)?[0].clone());
    }
    Ok(columns)
}

fn cursor_filter(keys: &[String], cursor: Option<Params>) -> (String, Params) {
    match cursor {
        Some(values) => (
            format!(
                "WHERE ({}) > ({})",
                keys.iter()
                    .map(|name| quoted(name))
                    .collect::<Vec<_>>()
                    .join(","),
                (1..=values.len())
                    .map(|index| format!("${index}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            values,
        ),
        None => (String::new(), params!()),
    }
}

fn decode_params(value: &serde_json::Value) -> Result<Params, StoreError> {
    value
        .as_array()
        .ok_or_else(|| invalid("parity cursor is not an array"))?
        .iter()
        .map(|cell| {
            let kind = json_string(cell, 0)?;
            let value = cell
                .get(1)
                .ok_or_else(|| invalid("cursor value is absent"))?;
            match kind.as_str() {
                "null" => Ok(Param::Null),
                "integer" => Ok(Param::Integer(
                    value
                        .as_str()
                        .ok_or_else(|| invalid("integer cursor is not text"))?
                        .parse()
                        .map_err(|_| invalid("integer cursor overflow"))?,
                )),
                "real" => Ok(Param::Real(
                    value
                        .as_str()
                        .ok_or_else(|| invalid("real cursor is not text"))?
                        .parse()
                        .map_err(|_| invalid("invalid real cursor"))?,
                )),
                "text" => Ok(Param::Text(
                    value
                        .as_str()
                        .ok_or_else(|| invalid("text cursor is absent"))?
                        .to_owned(),
                )),
                "blob" => Ok(Param::Blob(
                    hex::decode(
                        value
                            .as_str()
                            .ok_or_else(|| invalid("blob cursor is absent"))?,
                    )
                    .map_err(|_| invalid("invalid blob cursor"))?,
                )),
                _ => Err(invalid("unknown SQLite cursor storage class")),
            }
        })
        .collect()
}

fn value_param(value: ValueRef<'_>) -> rusqlite::Result<Param> {
    Ok(match value {
        ValueRef::Null => Param::Null,
        ValueRef::Integer(value) => Param::Integer(value),
        ValueRef::Real(value) => Param::Real(value),
        ValueRef::Text(value) => Param::Text(
            std::str::from_utf8(value)
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?
                .to_owned(),
        ),
        ValueRef::Blob(value) => Param::Blob(value.to_vec()),
    })
}

fn sqlite_value(value: &Param) -> Result<rusqlite::types::Value, StoreError> {
    Ok(match value {
        Param::Null => rusqlite::types::Value::Null,
        Param::Integer(value) => rusqlite::types::Value::Integer(*value),
        Param::Real(value) => rusqlite::types::Value::Real(*value),
        Param::Text(value) => rusqlite::types::Value::Text(value.clone()),
        Param::Blob(value) => rusqlite::types::Value::Blob(value.clone()),
        _ => {
            return Err(invalid(
                "statement-output references are forbidden in source cursors",
            ))
        }
    })
}

fn encoded_bytes(value: &Params) -> Result<usize, StoreError> {
    bincode::serde::encode_to_vec(value, bincode::config::legacy())
        .map(|value| value.len())
        .map_err(|error| invalid(error.to_string()))
}
fn push_digest(hash: &mut Sha256, value: &str) {
    hash.update((value.len() as u64).to_be_bytes());
    hash.update(value.as_bytes());
}

struct JsonRow {
    payload: String,
}
impl From<&mut Row<'_>> for JsonRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            payload: row.get("payload"),
        }
    }
}

async fn target_json(
    client: &Client,
    sql: String,
    bindings: Params,
) -> Result<Vec<serde_json::Value>, StoreError> {
    client
        // authority: a quiescent incoming voter must certify schema/freshness and full row parity from its committed replicated state before mode publication.
        .query_consistent_map::<JsonRow, _>(sql, bindings)
        .await
        .map_err(cluster_error)?
        .into_iter()
        .map(|row| {
            serde_json::from_str(&row.payload)
                .map_err(|error| invalid(format!("invalid metadata JSON: {error}")))
        })
        .collect()
}
async fn target_count(client: &Client, name: &str) -> Result<u64, StoreError> {
    let rows = target_json(
        client,
        format!(
            "SELECT json_array(count(*)) AS payload FROM {}",
            quoted(name)
        ),
        params!(),
    )
    .await?;
    u64::try_from(one_integer(&rows)?).map_err(|_| invalid("negative table count"))
}
fn one_integer(rows: &[serde_json::Value]) -> Result<i64, StoreError> {
    match rows {
        [row] => json_integer(row, 0),
        _ => Err(invalid("metadata query did not return exactly one row")),
    }
}
fn json_string(value: &serde_json::Value, index: usize) -> Result<String, StoreError> {
    value
        .get(index)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| invalid("metadata text field is absent"))
}
fn json_integer(value: &serde_json::Value, index: usize) -> Result<i64, StoreError> {
    value
        .get(index)
        .and_then(|value| value.as_i64())
        .ok_or_else(|| invalid("metadata integer field is absent"))
}
fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
fn cluster_control(name: &str) -> bool {
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

fn local_observation(name: &str) -> bool {
    matches!(
        name,
        "storage_identity" | "sharing_local_census" | "sharing_native_source_boot"
    )
}
fn validate_local_observation(table: &Table) -> Result<(), StoreError> {
    let expected: &[&str] = match table.name.as_str() {
        "storage_identity" => &["singleton", "instance_id", "node_id", "generation"],
        "sharing_local_census" => &["node_id", "attempt_id", "generation", "claimed_at_ms"],
        "sharing_native_source_boot" => &[
            "singleton",
            "node_id",
            "storage_generation",
            "boot_id",
            "generation",
            "master_fingerprint",
        ],
        _ => return Err(invalid("unclassified native observation")),
    };
    let names = table
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect::<BTreeSet<_>>();
    if table.kind != "table"
        || names != expected.iter().copied().collect::<BTreeSet<_>>()
        || table.columns.iter().any(|column| column.hidden != 0)
    {
        return Err(invalid(format!(
            "unknown durable columns in native observation {}",
            table.name
        )));
    }
    Ok(())
}

fn local_telemetry(name: &str) -> bool {
    matches!(
        name,
        "playback_events"
            | "network_priors"
            | "candidate_link_priors"
            | "fragment_indexes"
            | "fragment_index_outcomes"
            | "rendition_plans"
    )
}
fn derived(table: &Table) -> bool {
    matches!(table.name.as_str(), "sqlite_stat1" | "sqlite_stat4")
        || (table.kind == "virtual"
            && matches!(
                table.name.as_str(),
                "items_fts" | "items_fts_vocab" | "classification_fts"
            ))
        || (table.kind == "shadow"
            && matches!(
                table.name.as_str(),
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
fn task_error(error: tokio::task::JoinError) -> StoreError {
    StoreError::Task(format!("native import source worker: {error}"))
}
fn cluster_error(error: hiqlite::Error) -> StoreError {
    StoreError::Database(error.to_string())
}
fn invalid(message: impl std::fmt::Display) -> StoreError {
    StoreError::Migration(format!("current native import refused: {message}"))
}

#[cfg(test)]
#[path = "native_import_tests.rs"]
mod tests;
