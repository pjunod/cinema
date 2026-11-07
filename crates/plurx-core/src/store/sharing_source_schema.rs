//! Closed Source layout inputs for startup and compatible live coordination.
//! Reads never repair missing or partial layouts; guarded dispatch preserves Local work.
use super::{
    sharing_catalogue_source, sharing_source_sessions, MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA,
};

#[cfg(feature = "hiqlite-store")]
mod native_import;
#[cfg(feature = "hiqlite-store")]
pub(crate) use native_import::prepare_fresh_cluster_for_native_import;

// Frozen installed Source family layout remains71; custody is an adjunct.
pub(crate) const SOURCE_LAYOUT_VERSION: i64 = 71;
pub(crate) const SOURCE_SCHEMA_VERSION: i64 = 82;
pub(crate) const SOURCE_SCHEMA_PREDECESSOR: i64 = 81;
/// Additive, wire-compatible dispatch assertion. Never retains successful rows.
pub(crate) const DISPATCH_GUARD_SCHEMA: &str = "CREATE TABLE sharing_source_dispatch_guard (singleton INTEGER NOT NULL PRIMARY KEY CHECK(singleton=1),passed INTEGER NOT NULL CONSTRAINT plurx_source_layout_guard_v1 CHECK(passed=1)) STRICT";

pub(crate) fn dispatch_guard_shape() -> String {
    format!(
        "({}) AND NOT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND tbl_name='sharing_source_dispatch_guard') AND NOT EXISTS(SELECT 1 FROM sharing_source_dispatch_guard)",
        exact_object(DISPATCH_GUARD_SCHEMA)
    )
}

pub(crate) const BOOT_INTENTS_SCHEMA: &str = "CREATE TABLE sharing_source_boot_intents (node_id TEXT NOT NULL PRIMARY KEY CHECK(length(node_id) BETWEEN 1 AND 256),raft_id INTEGER NOT NULL CHECK(raft_id>0),attempt_id TEXT NOT NULL CHECK(length(attempt_id)=36),master_fingerprint TEXT NOT NULL CHECK(length(master_fingerprint)=64),membership_generation INTEGER NOT NULL CHECK(membership_generation>=0)) STRICT";
pub(crate) const INSTALLATION_SCHEMA: &str = "CREATE TABLE sharing_source_schema_installation (singleton INTEGER NOT NULL PRIMARY KEY CHECK(singleton=1),schema_version INTEGER NOT NULL CHECK(schema_version=71),master_fingerprint TEXT NOT NULL CHECK(length(master_fingerprint)=64),installed_at_ms INTEGER NOT NULL CHECK(installed_at_ms>0)) STRICT";
pub(crate) const TRANSACTION_SCHEMA: &str = "CREATE TABLE sharing_source_schema_transaction_guard (singleton INTEGER NOT NULL PRIMARY KEY CHECK(singleton=1),passed INTEGER NOT NULL CHECK(passed=1)) STRICT";
#[cfg(feature = "hiqlite-store")]
struct DispatchGuardRow(i64);
#[cfg(feature = "hiqlite-store")]
impl From<&mut hiqlite::Row<'_>> for DispatchGuardRow {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self(row.get("count"))
    }
}

/// Provision only at Store construction, never from serving session I/O.
/// Competing initializers may adopt an exact winner; fragments are never repaired.
#[cfg(feature = "hiqlite-store")]
pub(crate) async fn provision_dispatch_guard(
    client: &hiqlite::Client,
) -> Result<(), crate::error::StoreError> {
    use crate::error::StoreError;
    let rows=client.query_consistent_map::<DispatchGuardRow,_>("SELECT count(*) AS count FROM sqlite_master WHERE name='sharing_source_dispatch_guard'",hiqlite::params!()).await.map_err(|error|StoreError::Database(error.to_string()))?;
    if matches!(rows.as_slice(),[row] if row.0==0) {
        let creation = client
            .execute(DISPATCH_GUARD_SCHEMA, hiqlite::params!())
            .await;
        // Do not interpret an uncertain creation response as absence. Verify the
        // actual committed complete shape; no mutation is replayed here.
        let rows = client
            .query_consistent_map::<DispatchGuardRow, _>(
                format!(
                    "SELECT CASE WHEN ({}) THEN 1 ELSE 0 END AS count",
                    dispatch_guard_shape()
                ),
                hiqlite::params!(),
            )
            .await
            .map_err(|error| StoreError::Database(error.to_string()))?;
        if !matches!(rows.as_slice(),[row] if row.0==1) {
            creation.map_err(|error| StoreError::Database(error.to_string()))?;
            return Err(StoreError::Migration(
                "Source dispatch guard is not exact".to_owned(),
            ));
        }
    } else if !matches!(rows.as_slice(),[row] if row.0==1) {
        return Err(StoreError::Migration(
            "Source dispatch guard name is ambiguous".to_owned(),
        ));
    }
    let rows = client
        .query_consistent_map::<DispatchGuardRow, _>(
            format!(
                "SELECT CASE WHEN ({}) THEN 1 ELSE 0 END AS count",
                dispatch_guard_shape()
            ),
            hiqlite::params!(),
        )
        .await
        .map_err(|error| StoreError::Database(error.to_string()))?;
    if !matches!(rows.as_slice(),[row] if row.0==1) {
        return Err(StoreError::Migration(
            "Source dispatch guard is not exact".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn restore_interest_trigger() -> &'static str {
    include_str!("background_jobs_source_interest.sql")
        .split("-- next statement")
        .map(str::trim)
        .find(|sql| {
            sql.starts_with("CREATE TRIGGER IF NOT EXISTS playback_analysis_restore_effects\n")
        })
        .expect("closed Source interest trigger")
}

/// ALTER TABLE validates external triggers and views while a table is absent.
/// Only audited definitions may be detached and restored within the rebuild.
pub(crate) fn validated_external_rebuild_dependencies(
    objects: Vec<(String, String, String, String)>,
) -> Result<Vec<(String, String)>, crate::error::StoreError> {
    validated_external_rebuild_dependencies_with(objects, &[])
}

pub(crate) fn validated_external_rebuild_dependencies_with(
    objects: Vec<(String, String, String, String)>,
    additional_trigger_definitions: &[String],
) -> Result<Vec<(String, String)>, crate::error::StoreError> {
    use crate::error::StoreError;
    let mut detached = Vec::new();
    for (kind, name, table, sql) in objects {
        if FAMILIES.contains(&table.as_str())
            || !sql
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .any(|word| {
                    FAMILIES
                        .iter()
                        .any(|table| word.eq_ignore_ascii_case(table))
                })
        {
            continue;
        }
        if kind == "trigger" && additional_trigger_definitions.contains(&sql) {
            detached.push((name, sql));
            continue;
        }
        // Their exact declarations were already checked by predecessor_layout_guard.
        if kind == "trigger"
            && matches!(
                name.as_str(),
                "background_analysis_viewer_refresh"
                    | "jellyfin_watch_own_insert"
                    | "jellyfin_watch_own_update"
            )
        {
            continue;
        }
        let expected = restore_interest_trigger()
            .trim_end_matches(';')
            .replace("CREATE TRIGGER IF NOT EXISTS", "CREATE TRIGGER");
        if kind != "trigger"
            || name != "playback_analysis_restore_effects"
            || table != "background_job_commands"
            || sql != expected
        {
            return Err(StoreError::Migration(format!(
                "Source rebuild has an unrecognized dependent {kind}: {name}"
            )));
        }
        detached.push((name, sql));
    }
    if !detached
        .iter()
        .any(|(name, _)| name == "playback_analysis_restore_effects")
    {
        return Err(StoreError::Migration(
            "Source interest dependency is missing".into(),
        ));
    }
    Ok(detached)
}

#[cfg(feature = "hiqlite-store")]
pub(crate) struct ReplicatedRebuildDependencies {
    pub(crate) inventory: String,
    pub(crate) detached: Vec<(String, String)>,
}

/// Read the genuine committed schema through Raft. The captured inventory is
/// asserted again by the actual schema-write transaction before detachment.
#[cfg(feature = "hiqlite-store")]
pub(crate) async fn capture_replicated_rebuild_dependencies(
    client: &hiqlite::Client,
) -> Result<ReplicatedRebuildDependencies, crate::error::StoreError> {
    use crate::error::StoreError;
    struct DependencyRow(String);
    impl From<&mut hiqlite::Row<'_>> for DependencyRow {
        fn from(row: &mut hiqlite::Row<'_>) -> Self {
            Self(row.get("payload"))
        }
    }
    let objects=client.query_consistent_map::<DependencyRow,_>(
        "SELECT json_array(type,name,tbl_name,sql) AS payload FROM sqlite_master WHERE type IN('trigger','view') AND sql IS NOT NULL ORDER BY type,name",
        hiqlite::params!(),
    ).await.map_err(|error|StoreError::Database(error.to_string()))?.into_iter().map(|row|serde_json::from_str::<(String,String,String,String)>(&row.0).map_err(|error|StoreError::Migration(error.to_string()))).collect::<Result<Vec<_>,_>>()?;
    let detached = validated_external_rebuild_dependencies_with(
        objects.clone(),
        &crate::cluster::membership::source_rebuild_membership_trigger_definitions(),
    )?;
    Ok(ReplicatedRebuildDependencies {
        inventory: serde_json::to_string(&objects)
            .map_err(|error| StoreError::Migration(error.to_string()))?,
        detached,
    })
}

#[cfg(feature = "hiqlite-store")]
pub(crate) fn rebuild_dependency_inventory_guard(parameter: usize) -> String {
    assert!((1..=256).contains(&parameter));
    format!("(SELECT json_group_array(json_array(type,name,tbl_name,sql)) FROM (SELECT type,name,tbl_name,sql FROM sqlite_master WHERE type IN('trigger','view') AND sql IS NOT NULL ORDER BY type,name))=${parameter}")
}

#[cfg(feature = "hiqlite-store")]
pub(crate) fn restored_dependency_guard(parameter: usize) -> String {
    assert!((1..=256).contains(&parameter));
    format!("NOT EXISTS(SELECT 1 FROM json_each(${parameter}) expected WHERE NOT EXISTS(SELECT 1 FROM sqlite_master current WHERE current.type='trigger' AND current.name=json_extract(expected.value,'$[0]') AND current.sql=json_extract(expected.value,'$[1]')))")
}

pub(crate) const FAMILIES: [&str; 7] = [
    "media_session_requests",
    "media_playback_pointers",
    "media_sessions",
    "media_session_preparations",
    "media_playback_desired",
    "media_session_producer_recovery",
    "library_channel_session_recipes",
];
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
fn exact_object(statement: &str) -> String {
    let words: Vec<_> = statement.split_whitespace().collect();
    let name = words[2].split('(').next().expect("closed schema name");
    format!(
        "EXISTS(SELECT 1 FROM sqlite_master WHERE type={} AND name={} AND sql={})",
        quote(&words[1].to_ascii_lowercase()),
        quote(name),
        quote(statement)
    )
}
pub(crate) fn boot_shape_guard() -> String {
    format!(
        "({}) AND NOT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND tbl_name='sharing_source_boot_intents')",
        exact_object(BOOT_INTENTS_SCHEMA)
    )
}
/// The captured attempt set must still be the complete active roster, with
/// each attempt explicitly advertised by that process's current heartbeat.
/// Mere retained startup rows from a stopped process confer no authority.
#[cfg(feature = "hiqlite-store")]
pub(crate) fn boot_authority_guard(parameter: usize) -> String {
    assert!((1..=256).contains(&parameter));
    format!(
        "({}) AND json_type(${parameter})='array' AND json_array_length(${parameter}) BETWEEN 1 AND 256 AND (SELECT count(*) FROM cluster_nodes WHERE removed_at IS NULL)=json_array_length(${parameter}) AND NOT EXISTS(SELECT 1 FROM cluster_nodes node WHERE node.removed_at IS NULL AND NOT EXISTS(SELECT 1 FROM json_each(${parameter}) expected JOIN sharing_source_boot_intents intent ON intent.node_id=node.node_id AND intent.raft_id=node.raft_id WHERE json_extract(expected.value,'$[0]')=intent.node_id AND json_extract(expected.value,'$[1]')=intent.raft_id AND json_extract(expected.value,'$[2]')=intent.attempt_id AND json_extract(expected.value,'$[3]')=intent.master_fingerprint AND json_extract(expected.value,'$[4]')=intent.membership_generation AND intent.membership_generation=(SELECT generation FROM cluster_sharing_membership_generation WHERE singleton=1) AND EXISTS(SELECT 1 FROM cluster_node_capabilities proof WHERE proof.node_id=node.node_id AND proof.last_seen_at=node.last_seen_at AND proof.capability='sharing_source_boot_v1:'||intent.attempt_id)))",
        boot_shape_guard()
    )
}
fn installed_shape_guard_at(version: i64) -> String {
    format!(
        "({}) AND ({}) AND ({}) AND ({}) AND ({}) AND EXISTS(SELECT 1 FROM cluster_meta WHERE singleton=1 AND schema_version={version}) AND EXISTS(SELECT 1 FROM sharing_source_schema_installation WHERE singleton=1 AND schema_version={SOURCE_LAYOUT_VERSION}) AND EXISTS(SELECT 1 FROM item_identity_watermark WHERE singleton=1 AND importing=0 AND high_water>=coalesce((SELECT max(id) FROM items),0)) AND NOT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND tbl_name IN('sharing_source_schema_installation','sharing_source_schema_transaction_guard'))",
        super::jellyfin_watch::session_dependency_guard(),
        boot_shape_guard(),
        exact_object(INSTALLATION_SCHEMA),
        exact_object(TRANSACTION_SCHEMA),
        sharing_source_sessions::schema_guard()
    )
}
pub(crate) fn installed_shape_guard() -> String {
    format!(
        "({}) AND ({})",
        installed_shape_guard_at(SOURCE_SCHEMA_VERSION),
        super::sharing_ingress_custody::schema_guard()
    )
}
/// Coordination refusal fence only: this does not create a cleanup proof.
/// Original owners must retire held reservations before upgrading their marker.
#[cfg(all(test, feature = "hiqlite-store"))]
pub(crate) fn legacy_source_work_drained_guard() -> &'static str {
    "NOT EXISTS(SELECT 1 FROM sharing_source_session_bindings WHERE reservation_state='held') AND NOT EXISTS(SELECT 1 FROM media_session_requests WHERE principal_kind='sharing' AND state='starting') AND NOT EXISTS(SELECT 1 FROM media_sessions WHERE principal_kind='sharing' AND state!='ended') AND NOT EXISTS(SELECT 1 FROM media_session_preparations WHERE principal_kind='sharing')"
}
pub(crate) fn installed_guard() -> String {
    format!(
        "({}) AND EXISTS(SELECT 1 FROM sharing_source_schema_transaction_guard WHERE singleton=1 AND passed=1) AND EXISTS(SELECT 1 FROM sharing_source_schema_installation WHERE master_fingerprint NOT GLOB '*[^0-9a-f]*')",
        installed_shape_guard()
    )
}
/// Complete absence, not a count of new columns, is the only accepted predecessor.
/// Persisted Local ownership remains untouched and blocks this nonrolling rebuild.
fn legacy_columns(table: &str) -> Vec<(String, String)> {
    let marker = format!("INSERT INTO {table}_principal_new (");
    let projection = MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA
        .split(&marker)
        .nth(1)
        .expect("frozen copy projection")
        .split(')')
        .next()
        .expect("copy columns");
    let create = format!("CREATE TABLE {table}_principal_new (");
    let definition = MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA
        .split(&create)
        .nth(1)
        .expect("frozen definition")
        .split("-- next statement")
        .next()
        .expect("one definition");
    projection
        .split(',')
        .map(str::trim)
        .filter(|name| {
            !matches!(
                *name,
                "owner_key" | "principal_kind" | "share_grant_id" | "share_viewer_key"
            )
        })
        .map(|name| {
            let column_type = definition
                .lines()
                .find_map(|line| {
                    let mut words = line.split_whitespace();
                    (words.next() == Some(name)).then(|| {
                        words
                            .next()
                            .expect("frozen column type")
                            .trim_end_matches(',')
                            .to_owned()
                    })
                })
                .expect("copied column is declared");
            (name.to_owned(), column_type)
        })
        .collect()
}
/// Frozen baseline metadata includes all seven families and every object the
/// rebuild replaces, including the refresh trigger on background_job_commands.
/// Exact SQL preserves old CHECK, UNIQUE, foreign-key and trigger semantics;
/// additional indexes/triggers or any substituted definition refuse activation.
fn legacy_object_guard() -> String {
    type Object = (String, String, String, Vec<Option<String>>);
    let objects: Vec<Object> =
        serde_json::from_str(include_str!("sharing_source_legacy_schema.json"))
            .expect("compiled closed predecessor snapshot");
    let families = FAMILIES
        .iter()
        .map(|name| quote(name))
        .collect::<Vec<_>>()
        .join(",");
    let predicates = objects.iter().map(|(kind,name,table,sql)| {
        let sql = sql.iter().map(|sql|sql.as_ref().map_or_else(|| "sql IS NULL".to_owned(), |sql| format!("sql={}",quote(sql)))).collect::<Vec<_>>().join(" OR ");
        format!("EXISTS(SELECT 1 FROM sqlite_master WHERE type={} AND name={} AND tbl_name={} AND ({sql}))",quote(kind),quote(name),quote(table))
    }).collect::<Vec<_>>();
    format!(
        "(SELECT count(*) FROM sqlite_master WHERE tbl_name IN({families}) OR name='background_analysis_viewer_refresh')={} AND ({})",
        objects.len(),
        predicates.join(") AND (")
    )
}
pub(crate) fn predecessor_layout_guard() -> String {
    predecessor_layout_guard_for(&format!("EXISTS(SELECT 1 FROM cluster_meta WHERE singleton=1 AND schema_version={SOURCE_SCHEMA_PREDECESSOR})"))
}
/// The exact closed recognizer used by singleton export and packaging cohort
/// descriptors. This builds SQL only; it neither reads nor admits a source.
pub(crate) fn migration_source_layout_guard(auth_version: i64) -> Option<String> {
    match auth_version {
        SOURCE_SCHEMA_PREDECESSOR => Some(predecessor_layout_guard()),
        SOURCE_SCHEMA_VERSION => Some(installed_guard()),
        _ => None,
    }
}

pub(crate) fn native_predecessor_layout_guard() -> String {
    predecessor_layout_guard_for(
        "EXISTS(SELECT 1 FROM pragma_user_version WHERE user_version>=124)",
    )
}
fn predecessor_layout_guard_for(marker: &str) -> String {
    let mut objects = vec![
        "sharing_source_schema_installation".to_owned(),
        "sharing_source_schema_transaction_guard".to_owned(),
        "media_session_principal_relay_backup".to_owned(),
        "media_session_principal_delivery_backup".to_owned(),
    ];
    for table in FAMILIES {
        objects.push(format!("{table}_principal_new"));
    }
    for statement in sharing_catalogue_source::candidate_statements()
        .into_iter()
        .chain(sharing_catalogue_source::candidate_item_identity_statements())
        .chain(sharing_source_sessions::candidate_statements())
    {
        let Some(start) = statement.find("CREATE ") else {
            continue;
        };
        let words: Vec<_> = statement[start..].split_whitespace().collect();
        objects.push(
            words[2]
                .split('(')
                .next()
                .expect("closed object")
                .to_owned(),
        );
    }
    let names = objects
        .iter()
        .map(|name| quote(name))
        .collect::<Vec<_>>()
        .join(",");
    let mut guards = vec![
        marker.to_owned(),
        format!("NOT EXISTS(SELECT 1 FROM sqlite_master WHERE name IN({names}))"),
    ];
    for table in FAMILIES {
        guards.push(format!("EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='{table}') AND NOT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name IN('owner_key','principal_kind','share_grant_id','share_viewer_key')) AND NOT EXISTS(SELECT 1 FROM {table} WHERE user_id<=0 OR user_id IS NULL)"));
        let primary = if table == "media_sessions" {
            "incarnation_id"
        } else {
            "user_id"
        };
        guards.push(format!(
            "EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name='{primary}' AND pk=1)"
        ));
        let columns = legacy_columns(table);
        let expected = columns
            .iter()
            .map(|(name, kind)| format!("(name={} AND type={})", quote(name), quote(kind)))
            .collect::<Vec<_>>()
            .join(" OR ");
        guards.push(format!("(SELECT count(*) FROM pragma_table_info('{table}'))={} AND NOT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE NOT ({expected})) AND EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name='user_id' AND \"notnull\"=1) AND EXISTS(SELECT 1 FROM pragma_table_list WHERE schema='main' AND name='{table}' AND strict=1)",columns.len()));
    }
    guards.push(legacy_object_guard());
    guards.push(super::jellyfin_watch::session_dependency_guard());
    format!("({})", guards.join(") AND ("))
}
#[cfg(feature = "hiqlite-store")]
pub(crate) fn predecessor_guard(now_parameter: usize) -> String {
    assert!((1..=256).contains(&now_parameter));
    format!(
        "({}) AND NOT EXISTS(SELECT 1 FROM media_sessions WHERE state!='ended') AND NOT EXISTS(SELECT 1 FROM media_session_requests WHERE state='starting') AND NOT EXISTS(SELECT 1 FROM media_session_preparations) AND NOT EXISTS(SELECT 1 FROM job_leases WHERE expires_at_ms>${now_parameter})",
        predecessor_layout_guard()
    )
}

pub(crate) const NATIVE_BOOT_SCHEMA: &str = "CREATE TABLE sharing_native_source_boot (singleton INTEGER NOT NULL PRIMARY KEY CHECK(singleton=1),node_id TEXT NOT NULL,storage_generation TEXT NOT NULL,boot_id TEXT NOT NULL CHECK(length(boot_id)=36),generation INTEGER NOT NULL CHECK(generation>0),master_fingerprint TEXT NOT NULL CHECK(length(master_fingerprint)=64)) STRICT";

pub(crate) fn native_layout_shape_guard() -> String {
    format!("({}) AND ({}) AND ({}) AND ({}) AND ({}) AND ({}) AND EXISTS(SELECT 1 FROM pragma_user_version WHERE user_version>=124) AND EXISTS(SELECT 1 FROM item_identity_watermark WHERE singleton=1 AND importing=0 AND high_water>=coalesce((SELECT max(id) FROM items),0))",
        super::jellyfin_watch::session_dependency_guard(),
        exact_object(NATIVE_BOOT_SCHEMA),exact_object(INSTALLATION_SCHEMA),exact_object(TRANSACTION_SCHEMA),
        sharing_source_sessions::schema_guard_for(false),dispatch_guard_shape())
}
pub(crate) fn native_installed_shape_guard() -> String {
    format!("({}) AND EXISTS(SELECT 1 FROM sharing_source_schema_installation WHERE singleton=1 AND schema_version={SOURCE_LAYOUT_VERSION})",native_layout_shape_guard())
}
pub(crate) fn native_installed_guard() -> String {
    format!("({}) AND ({}) AND EXISTS(SELECT 1 FROM sharing_source_schema_transaction_guard WHERE singleton=1 AND passed=1) AND EXISTS(SELECT 1 FROM sharing_source_schema_installation i JOIN sharing_native_source_boot b ON b.singleton=i.singleton AND b.master_fingerprint=i.master_fingerprint JOIN storage_identity s ON s.singleton=1 AND s.node_id=b.node_id AND s.generation=b.storage_generation)",native_installed_shape_guard(),super::sharing_ingress_custody::schema_guard())
}

/// Frozen statement order preserves child authority rows and then seeds the
/// allocator before order-maintenance triggers can observe ordinary inserts.
pub(crate) fn layout_statements() -> Vec<String> {
    let mut statements = super::media_session_principal_rebuild_after_external_detach()
        .split("-- next statement\n")
        .map(|part| {
            part.lines()
                .skip_while(|line| line.trim().is_empty() || line.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .trim_end_matches(';')
                .to_owned()
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    statements.extend(sharing_catalogue_source::candidate_item_identity_statements());
    statements.extend(sharing_catalogue_source::candidate_statements());
    statements.extend(sharing_source_sessions::candidate_statements());
    statements
}
#[cfg(all(test, feature = "hiqlite-store"))]
mod tests {
    use super::*;
    use crate::store::{SqliteStore, SQLITE_SCHEMA_VERSION};
    use rusqlite::Connection;
    fn database() -> Connection {
        let conn = Connection::open_in_memory().expect("database");
        SqliteStore::apply_migrations_for_test(&conn, SQLITE_SCHEMA_VERSION).expect("baseline");
        conn.execute_batch("CREATE TABLE cluster_meta(singleton INTEGER PRIMARY KEY,schema_version INTEGER); INSERT INTO cluster_meta VALUES(1,81);").expect("replicated marker fixture");
        conn
    }
    fn evaluate(conn: &Connection, guard: &str) -> bool {
        let sql = format!("SELECT CASE WHEN {guard} THEN 1 ELSE 0 END");
        let mut statement = conn.prepare(&sql).expect("predicate");
        let result = if statement.parameter_count() == 0 {
            statement.query_row([], |row| row.get::<_, i64>(0))
        } else {
            statement.query_row(rusqlite::params![10000], |row| row.get::<_, i64>(0))
        };
        result.expect("closed predicate") == 1
    }

    #[test]
    fn sharing_source_rebuild_refuses_schema_race_and_requires_exact_dependency_restore() {
        let conn = Connection::open_in_memory().expect("schema mutation fixture");
        conn.execute_batch("CREATE TABLE media_sessions(id INTEGER); CREATE TABLE commands(id INTEGER); CREATE TRIGGER retained_dependency AFTER INSERT ON commands BEGIN DELETE FROM media_sessions WHERE id=NEW.id; END").expect("external dependency");
        let objects=conn.prepare("SELECT type,name,tbl_name,sql FROM sqlite_master WHERE type IN('trigger','view') AND sql IS NOT NULL ORDER BY type,name").expect("inventory").query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?))).expect("inventory rows").collect::<Result<Vec<_>,_>>().expect("inventory values");
        let inventory = serde_json::to_string(&objects).expect("captured inventory");
        let predicate = |guard: String, value: &str| {
            conn.query_row(&format!("SELECT ({guard})"), [value], |row| {
                row.get::<_, bool>(0)
            })
            .expect("actual schema guard")
        };
        assert!(predicate(rebuild_dependency_inventory_guard(1), &inventory));
        conn.execute_batch("CREATE VIEW raced_dependency AS SELECT * FROM media_sessions")
            .expect("concurrent dependency change");
        assert!(
            !predicate(rebuild_dependency_inventory_guard(1), &inventory),
            "schema change after observation cannot enter the ownership rebuild"
        );
        conn.execute_batch("DROP VIEW raced_dependency")
            .expect("remove race fixture");
        let retained = serde_json::to_string(&[(objects[0].1.clone(), objects[0].3.clone())])
            .expect("retained exact SQL");
        conn.execute_batch("DROP TRIGGER retained_dependency; CREATE TRIGGER retained_dependency AFTER INSERT ON commands BEGIN SELECT 1; END").expect("same name substituted semantics");
        assert!(!predicate(restored_dependency_guard(1), &retained));
        conn.execute_batch("DROP TRIGGER retained_dependency")
            .expect("remove substitution");
        conn.execute_batch(&objects[0].3)
            .expect("restore original semantics");
        assert!(predicate(restored_dependency_guard(1), &retained));
        assert!(predicate(rebuild_dependency_inventory_guard(1), &inventory));
    }

    #[test]
    fn sharing_source_custody_upgrade_refuses_held_and_active_legacy_obligations() {
        // SQL refusal predicate only, never a production Source handle/proof.
        let conn = Connection::open_in_memory().expect("database");
        conn.execute_batch("CREATE TABLE sharing_source_session_bindings(reservation_state TEXT); CREATE TABLE media_session_requests(principal_kind TEXT,state TEXT); CREATE TABLE media_sessions(principal_kind TEXT,state TEXT); CREATE TABLE media_session_preparations(principal_kind TEXT);").expect("predicate fixture");
        assert!(evaluate(&conn, legacy_source_work_drained_guard()));
        conn.execute(
            "INSERT INTO sharing_source_session_bindings VALUES('held')",
            [],
        )
        .expect("retained debt");
        assert!(!evaluate(&conn, legacy_source_work_drained_guard()));
        conn.execute(
            "UPDATE sharing_source_session_bindings SET reservation_state='released'",
            [],
        )
        .expect("predicate-only released fixture");
        conn.execute("INSERT INTO media_sessions VALUES('sharing','active')", [])
            .expect("active route");
        assert!(!evaluate(&conn, legacy_source_work_drained_guard()));
        conn.execute("UPDATE media_sessions SET state='ended'", [])
            .expect("terminal metadata");
        conn.execute(
            "INSERT INTO media_session_requests VALUES('sharing','starting')",
            [],
        )
        .expect("pending start");
        assert!(!evaluate(&conn, legacy_source_work_drained_guard()));
        conn.execute("UPDATE media_session_requests SET state='failed'", [])
            .expect("failed metadata");
        conn.execute(
            "INSERT INTO media_session_preparations VALUES('sharing')",
            [],
        )
        .expect("prepared successor");
        assert!(!evaluate(&conn, legacy_source_work_drained_guard()));
    }
    #[test]
    fn sharing_source_schema_predecessor_refuses_partial_layout_and_live_local_ownership() {
        let conn = database();
        let guard = predecessor_guard(1);
        assert!(evaluate(&conn, &guard));
        conn.execute_batch(
            "ALTER TABLE media_session_requests ADD COLUMN unpublished_private_state TEXT",
        )
        .expect("unknown persisted column");
        assert!(
            !evaluate(&conn, &guard),
            "the installer must never drop an unmodeled Local column"
        );
        let conn = database();
        conn.execute_batch("DROP TRIGGER media_sessions_drain_ownership_fence_au; CREATE TRIGGER media_sessions_drain_ownership_fence_au AFTER UPDATE ON media_sessions BEGIN SELECT 1; END").expect("substituted legacy invariant");
        assert!(!evaluate(&conn, &guard));
        let conn = database();
        conn.execute_batch("CREATE INDEX unmodeled_local_index ON media_sessions(playback_id)")
            .expect("unknown persisted index");
        assert!(!evaluate(&conn, &guard));
        let conn = database();
        conn.execute_batch("ALTER TABLE media_session_requests ADD COLUMN owner_key TEXT")
            .expect("partial layout");
        assert!(!evaluate(&conn, &guard));
        let conn = database();
        conn.execute_batch(include_str!(
            "../../tests/fixtures/session-principal-local.sql"
        ))
        .expect("Local history and ownership");
        assert!(!evaluate(&conn, &guard));
        conn.execute_batch("UPDATE media_sessions SET state='ended'; UPDATE media_session_requests SET state='failed'; DELETE FROM media_session_preparations; UPDATE job_leases SET expires_at_ms=0").expect("test-only explicit drain");
        assert!(evaluate(&conn, &guard));
        conn.execute_batch("CREATE TABLE item_identity_watermark(singleton INTEGER)")
            .expect("partial allocator");
        assert!(!evaluate(&conn, &guard));
    }
    #[test]
    fn sharing_source_schema_accepts_frozen_replicated_v10_additive_session_shape() {
        let conn = database();
        conn.execute_batch("DROP TRIGGER jellyfin_watch_own_insert; DROP TRIGGER jellyfin_watch_own_update; DROP TABLE media_sessions")
            .expect("replace empty SQL fixture family");
        conn.execute_batch(super::super::hiqlite_sessions::MEDIA_SESSIONS_V10_SCHEMA)
            .expect("exact historical v10 declaration");
        for sql in [
            super::super::hiqlite_sessions::MEDIA_SESSION_TERMINAL_REASON_MIGRATION,
            super::super::hiqlite_sessions::MEDIA_SESSION_PUBLICATION_FENCE_MIGRATION,
            super::super::MEDIA_SESSION_DRAIN_DEADLINE_COLUMN,
            super::super::MEDIA_SESSION_RECOVERY_EPOCH_SCHEMA,
        ] {
            conn.execute_batch(sql).expect("actual additive migration");
        }
        type Object = (String, String, String, Vec<Option<String>>);
        let objects: Vec<Object> =
            serde_json::from_str(include_str!("sharing_source_legacy_schema.json"))
                .expect("frozen metadata");
        for (kind, _, table, variants) in objects {
            if table == "media_sessions" && kind != "table" {
                if let Some(sql) = variants.first().and_then(Option::as_ref) {
                    conn.execute_batch(sql)
                        .expect("restore known index/trigger");
                }
            }
        }
        for (_, sql) in super::super::jellyfin_watch::session_dependency_triggers() {
            conn.execute_batch(sql)
                .expect("restore exact external watch dependency");
        }
        assert!(evaluate(&conn, &predecessor_guard(1)));
    }
    #[test]
    fn sharing_source_schema_accepts_published_session_upgrades_and_refuses_substitution() {
        fn upgraded(published: &str, add_drain: bool, substitute: bool) -> Connection {
            let conn = database();
            conn.execute_batch("DROP TRIGGER jellyfin_watch_own_insert; DROP TRIGGER jellyfin_watch_own_update; DROP TABLE media_sessions")
                .expect("replace empty session family");
            let ddl = if substitute {
                published.replace("CHECK (owner_epoch > 0)", "CHECK (owner_epoch >= 0)")
            } else {
                published.to_owned()
            };
            conn.execute_batch(&ddl)
                .expect("published fresh declaration");
            if add_drain {
                conn.execute_batch(super::super::MEDIA_SESSION_DRAIN_DEADLINE_COLUMN)
                    .expect("actual additive drain deadline migration");
            }
            conn.execute_batch("INSERT INTO media_sessions(incarnation_id,session_id,user_id,playback_id,request_fingerprint,owner_node_id,owner_epoch,lease_expires_at_ms,state,terminal_reason,publication_ready_at_ms,recipe_json,response_json,updated_at_ms,drain_deadline_ms) VALUES('historical-session','historical-physical-id',7,'historical-playback','historical-fingerprint','historical-node',1,0,'ended','admin_stop',0,'{}','{}',42,123)")
                .expect("real predecessor row");
            conn.execute_batch(super::super::MEDIA_SESSION_RECOVERY_EPOCH_SCHEMA)
                .expect("actual additive recovery epoch migration");
            let row: (String, String, i64, i64, i64) = conn.query_row(
                "SELECT recovery_epoch,terminal_reason,publication_ready_at_ms,updated_at_ms,drain_deadline_ms FROM media_sessions WHERE incarnation_id='historical-session'",
                [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            ).expect("additive default and unchanged historical fields");
            assert_eq!(row, ("".into(), "admin_stop".into(), 0, 42, 123));
            type Object = (String, String, String, Vec<Option<String>>);
            let objects: Vec<Object> =
                serde_json::from_str(include_str!("sharing_source_legacy_schema.json"))
                    .expect("closed predecessor inventory");
            for (kind, _, table, variants) in objects {
                if table == "media_sessions" && kind != "table" {
                    if let Some(sql) = variants.first().and_then(Option::as_ref) {
                        conn.execute_batch(sql)
                            .expect("restore exact index and trigger");
                    }
                }
            }
            for (_, sql) in super::super::jellyfin_watch::session_dependency_triggers() {
                conn.execute_batch(sql)
                    .expect("restore exact external dependency");
            }
            conn
        }
        let guard = predecessor_layout_guard();
        for (published, add_drain) in [
            (
                include_str!("../../tests/fixtures/media-sessions-v31.sql"),
                false,
            ),
            (
                include_str!("../../tests/fixtures/media-sessions-v18.sql"),
                true,
            ),
        ] {
            let conn = upgraded(published, add_drain, false);
            assert!(
                evaluate(&conn, &guard),
                "published upgrade layout is recognized exactly"
            );
            assert!(
                evaluate(&conn, &predecessor_guard(1)),
                "ended historical ownership is drained"
            );
            conn.execute_batch("CREATE INDEX unmodeled_v31_index ON media_sessions(playback_id)")
                .expect("unknown historical index");
            assert!(
                !evaluate(&conn, &guard),
                "additional objects remain refused"
            );
            let conn = upgraded(published, add_drain, true);
            assert!(
                !evaluate(&conn, &guard),
                "altered owner constraint remains refused"
            );
        }
    }
    #[test]
    fn sharing_source_schema_accepts_replicated_pointer_additive_upgrade() {
        let conn = database();
        conn.execute_batch("DROP TABLE media_playback_pointers")
            .expect("replace empty pointer fixture");
        conn.execute_batch(include_str!(
            "../../tests/fixtures/media-playback-pointers-v10.sql"
        ))
        .expect("historical replicated pointer declaration");
        conn.execute_batch(super::super::MEDIA_PLAYBACK_POINTER_DESIRED_REVISION_COLUMN)
            .expect("actual additive desired-revision migration");
        type Object = (String, String, String, Vec<Option<String>>);
        let objects: Vec<Object> =
            serde_json::from_str(include_str!("sharing_source_legacy_schema.json"))
                .expect("frozen metadata");
        for (kind, _, table, variants) in objects {
            if table == "media_playback_pointers" && kind != "table" {
                if let Some(sql) = variants.first().and_then(Option::as_ref) {
                    conn.execute_batch(sql)
                        .expect("restore exact pointer index/trigger");
                }
            }
        }
        assert!(evaluate(&conn, &predecessor_layout_guard()));
        assert!(evaluate(&conn, &predecessor_guard(1)));
        conn.execute_batch(
            "CREATE INDEX unmodeled_pointer_index ON media_playback_pointers(updated_at_ms)",
        )
        .expect("unknown pointer index");
        assert!(!evaluate(&conn, &predecessor_layout_guard()));
    }
    #[test]
    fn sharing_source_schema_refuses_substituted_external_watch_authority_trigger() {
        let conn = database();
        assert!(evaluate(&conn, &predecessor_layout_guard()));
        conn.execute_batch("DROP TRIGGER jellyfin_watch_own_update; CREATE TRIGGER jellyfin_watch_own_update AFTER UPDATE ON watch_state BEGIN SELECT 1; END;").expect("substituted authority fixture");
        assert!(!evaluate(&conn, &predecessor_layout_guard()));
    }
    #[test]
    fn sharing_source_schema_boot_registry_refuses_shape_substitution_and_attached_triggers() {
        let conn = database();
        conn.execute_batch(BOOT_INTENTS_SCHEMA)
            .expect("closed startup registry");
        assert!(evaluate(&conn, &boot_shape_guard()));
        conn.execute_batch("CREATE TRIGGER boot_ignore BEFORE DELETE ON sharing_source_boot_intents BEGIN SELECT RAISE(IGNORE); END").expect("ignored release fixture");
        assert!(!evaluate(&conn, &boot_shape_guard()));
    }
    #[test]
    fn sharing_source_schema_boot_authority_requires_exact_current_attempt_and_generation() {
        let conn = database();
        conn.execute_batch(BOOT_INTENTS_SCHEMA).expect("registry");
        conn.execute_batch("CREATE TABLE cluster_nodes(node_id TEXT,raft_id INTEGER,last_seen_at INTEGER,removed_at INTEGER); CREATE TABLE cluster_node_capabilities(node_id TEXT,capability TEXT,last_seen_at INTEGER); CREATE TABLE cluster_sharing_membership_generation(singleton INTEGER,generation INTEGER); INSERT INTO cluster_sharing_membership_generation VALUES(1,2); INSERT INTO cluster_nodes VALUES('actual-fixture',1,100,NULL);").expect("SQL authority fixture, not a Raft observation");
        let attempt = uuid::Uuid::new_v4().to_string();
        let next = uuid::Uuid::new_v4().to_string();
        let fingerprint = "a".repeat(64);
        conn.execute(
            "INSERT INTO sharing_source_boot_intents VALUES('actual-fixture',1,?1,?2,2)",
            rusqlite::params![attempt, fingerprint],
        )
        .expect("captured startup intent");
        conn.execute(
            "INSERT INTO cluster_node_capabilities VALUES('actual-fixture',?1,100)",
            [format!("sharing_source_boot_v1:{attempt}")],
        )
        .expect("same heartbeat attempt");
        let captured =
            serde_json::json!([["actual-fixture", 1, attempt, fingerprint, 2]]).to_string();
        let sql = format!(
            "SELECT CASE WHEN {} THEN 1 ELSE 0 END",
            boot_authority_guard(1)
        );
        let ready = || {
            conn.query_row(&sql, [&captured], |row| row.get::<_, i64>(0))
                .expect("current boot predicate")
        };
        assert_eq!(ready(), 1);
        conn.execute_batch("CREATE TABLE installer_assertion(passed INTEGER CHECK(passed=1));")
            .expect("same-write CHECK fixture");
        let mut statement = conn.prepare("BEGIN").expect("begin");
        statement.execute([]).expect("transaction");
        drop(statement);
        conn.execute("DELETE FROM sharing_source_boot_intents", [])
            .expect("ordered revocation");
        let mut assertion = conn
            .prepare(&format!(
                "INSERT INTO installer_assertion SELECT CASE WHEN {} THEN 1 ELSE 0 END",
                boot_authority_guard(1)
            ))
            .expect("same-write assertion");
        assert!(assertion.execute([&captured]).is_err());
        drop(assertion);
        conn.execute_batch("ROLLBACK")
            .expect("rollback revoked queued installer");
        assert_eq!(
            ready(),
            1,
            "failed guarded transaction restores all prior state"
        );
        conn.execute("UPDATE cluster_nodes SET last_seen_at=101", [])
            .expect("new heartbeat lacking attempt proof");
        assert_eq!(ready(), 0);
        conn.execute("UPDATE cluster_node_capabilities SET last_seen_at=101", [])
            .expect("retained same boot");
        assert_eq!(ready(), 1);
        conn.execute(
            "UPDATE sharing_source_boot_intents SET attempt_id=?1",
            [&next],
        )
        .expect("different startup attempt");
        assert_eq!(ready(), 0);
        conn.execute(
            "UPDATE sharing_source_boot_intents SET attempt_id=?1",
            [&attempt],
        )
        .expect("restore fixture");
        conn.execute(
            "UPDATE cluster_sharing_membership_generation SET generation=3",
            [],
        )
        .expect("membership changed");
        assert_eq!(ready(), 0);
        conn.execute("DELETE FROM sharing_source_boot_intents", [])
            .expect("confirmed release");
        assert_eq!(ready(), 0, "queued old installer is fenced after release");
    }
}
