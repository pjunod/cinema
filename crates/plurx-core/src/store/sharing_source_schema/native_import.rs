//! Schema-only application preparation on a real, quiescent Raft import target.
//! Retained identities, sealed material and installation receipts are copied by
//! the cluster migration adapter after this transaction; no factory runs here.
use super::*;
use crate::error::StoreError;
use hiqlite::{params, Client, Row};

struct Dependency {
    kind: String,
    name: String,
    table: String,
    sql: String,
}
impl From<&mut Row<'_>> for Dependency {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            kind: row.get("type"),
            name: row.get("name"),
            table: row.get("tbl_name"),
            sql: row.get("sql"),
        }
    }
}

/// Requires the canonical membership schema and genuine initial local voter
/// registration first. The caller holds promotion's exclusive directory lock
/// and has not published either transport or serving authority.
pub(crate) async fn prepare_fresh_cluster_for_native_import(
    client: &Client,
    expected_instance_id: &str,
    expected_node_id: &str,
    actual_local_raft_id: u64,
) -> Result<(), StoreError> {
    let validate = || async {
        crate::cluster::membership::require_native_import_singleton(client, actual_local_raft_id)
            .await
            .map_err(|error| StoreError::Migration(error.to_string()))
    };
    let database = |error: hiqlite::Error| StoreError::Database(error.to_string());
    validate().await?;
    let ready = client
        .query_consistent_map::<DispatchGuardRow, _>(
            format!(
                "SELECT CASE WHEN ({}) AND ({}) THEN 1 ELSE 0 END AS count",
                predecessor_layout_guard(),
                dispatch_guard_shape()
            ),
            params!(),
        )
        .await
        .map_err(database)?;
    if !matches!(ready.as_slice(), [row] if row.0 == 1) {
        return Err(StoreError::Migration(
            "native import target is not the exact Source predecessor".into(),
        ));
    }
    let dependencies = client.query_consistent_map::<Dependency,_>(
        "SELECT type,name,tbl_name,sql FROM sqlite_master WHERE type IN('trigger','view') AND sql IS NOT NULL ORDER BY type,name",
        params!(),
    ).await.map_err(database)?.into_iter().map(|row|(row.kind,row.name,row.table,row.sql)).collect::<Vec<_>>();
    let detached = validated_external_rebuild_dependencies_with(
        dependencies.clone(),
        &crate::cluster::membership::source_rebuild_membership_trigger_definitions(),
    )?;
    let dependency_inventory = serde_json::to_string(&dependencies)
        .map_err(|error| StoreError::Migration(error.to_string()))?;
    let families_empty = FAMILIES
        .iter()
        .map(|table| format!("NOT EXISTS(SELECT 1 FROM {table})"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let material = super::super::sharing_purpose_keys::replicated_import_material_statements();
    let material_names = material
        .iter()
        .map(|sql| {
            quote(
                sql.split_whitespace()
                    .nth(2)
                    .expect("closed material DDL")
                    .split('(')
                    .next()
                    .expect("closed name"),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let raft_id = i64::try_from(actual_local_raft_id)
        .map_err(|_| StoreError::Migration("native import voter id exceeds SQLite range".into()))?;
    let mut statements=vec![
        (hiqlite::Error::SOURCE_LAYOUT_GUARD_SQL.to_owned(),params!(SOURCE_SCHEMA_PREDECESSOR)),
        (format!("INSERT INTO sharing_source_dispatch_guard VALUES(1,CASE WHEN ({}) AND ({families_empty}) AND NOT EXISTS(SELECT 1 FROM users) AND NOT EXISTS(SELECT 1 FROM items) AND NOT EXISTS(SELECT 1 FROM files) AND NOT EXISTS(SELECT 1 FROM sharing_identity) AND NOT EXISTS(SELECT 1 FROM sqlite_master WHERE name IN({material_names})) AND EXISTS(SELECT 1 FROM settings WHERE key='instance.id' AND value=$1) AND (SELECT count(*) FROM cluster_nodes)=1 AND EXISTS(SELECT 1 FROM cluster_nodes WHERE node_id=$2 AND raft_id=$3 AND role='voter' AND last_seen_at=0 AND removed_at IS NULL) AND (SELECT json_group_array(json_array(type,name,tbl_name,sql)) FROM (SELECT type,name,tbl_name,sql FROM sqlite_master WHERE type IN('trigger','view') AND sql IS NOT NULL ORDER BY type,name))=$4 THEN 1 ELSE 0 END)",predecessor_layout_guard()),params!(expected_instance_id,expected_node_id,raft_id,dependency_inventory)),
    ];
    for (name, _) in &detached {
        statements.push((format!("DROP TRIGGER {name}"), params!()));
    }
    statements.extend(layout_statements().into_iter().map(|sql| (sql, params!())));
    statements.extend(detached.into_iter().map(|(_, sql)| (sql, params!())));
    statements.extend(material.into_iter().map(|sql| (sql, params!())));
    statements.extend([
        (INSTALLATION_SCHEMA.to_owned(),params!()),
        (TRANSACTION_SCHEMA.to_owned(),params!()),
        (BOOT_INTENTS_SCHEMA.to_owned(),params!()),
        (format!("UPDATE cluster_meta SET schema_version={SOURCE_SCHEMA_VERSION} WHERE singleton=1 AND schema_version={SOURCE_SCHEMA_PREDECESSOR}"),params!()),
        ("DELETE FROM sharing_source_dispatch_guard".to_owned(),params!()),
        (format!("INSERT INTO sharing_source_dispatch_guard VALUES(1,CASE WHEN ({}) AND ({}) AND ({}) AND ({}) AND ({}) AND NOT EXISTS(SELECT 1 FROM sharing_source_schema_installation) AND NOT EXISTS(SELECT 1 FROM sharing_source_schema_transaction_guard) AND NOT EXISTS(SELECT 1 FROM sharing_source_boot_intents) AND NOT EXISTS(SELECT 1 FROM sharing_catalogue_keys) AND NOT EXISTS(SELECT 1 FROM sharing_file_locator_keys) AND NOT EXISTS(SELECT 1 FROM sharing_purpose_key_installation) AND NOT EXISTS(SELECT 1 FROM sharing_purpose_key_archive) AND NOT EXISTS(SELECT 1 FROM sharing_purpose_census_intents) AND NOT EXISTS(SELECT 1 FROM sharing_purpose_transaction_guard) AND NOT EXISTS(SELECT 1 FROM pragma_foreign_key_check) THEN 1 ELSE 0 END)",super::super::sharing_source_sessions::schema_guard(),boot_shape_guard(),exact_object(INSTALLATION_SCHEMA),exact_object(TRANSACTION_SCHEMA),super::super::sharing_ingress_custody::schema_guard()),params!()),
        ("DELETE FROM sharing_source_dispatch_guard".to_owned(),params!()),
    ]);
    validate().await?;
    client
        .txn(statements)
        .await
        .map_err(database)?
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(database)?;
    validate().await
}
