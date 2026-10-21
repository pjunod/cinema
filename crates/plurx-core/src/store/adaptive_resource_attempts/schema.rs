//! Canonical additive schema. Unknown/partial/future objects are preserved.
//! Main composes the exact layout-specific trigger set into Source migration.

use rusqlite::{Connection, OptionalExtension};

use crate::error::StoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Legacy,
    Principal,
}

impl Layout {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Legacy => "local-v1",
            Self::Principal => "principal-v1",
        }
    }

    pub fn owner(self, alias: &str) -> String {
        match self {
            Self::Legacy => format!("('local:'||{alias}.user_id)"),
            Self::Principal => format!("{alias}.owner_key"),
        }
    }
}

pub(crate) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

const TABLES: &[(&str, &str)] = &[
    ("media_resource_schema", "CREATE TABLE media_resource_schema (singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=1), layout TEXT NOT NULL CHECK(layout IN('local-v1','principal-v1')), revision INTEGER NOT NULL CHECK(revision>=0), mutation_claim TEXT CHECK(mutation_claim IS NULL OR (json_valid(mutation_claim) AND length(CAST(mutation_claim AS BLOB))<=4096))) STRICT"),
    ("media_resource_requests", "CREATE TABLE media_resource_requests (owner_key TEXT NOT NULL CHECK(length(CAST(owner_key AS BLOB)) BETWEEN 1 AND 256), request_id TEXT NOT NULL CHECK(length(CAST(request_id AS BLOB)) BETWEEN 1 AND 128), request_incarnation_id TEXT NOT NULL CHECK(length(request_incarnation_id)=36), binding_json TEXT NOT NULL CHECK(json_valid(binding_json) AND length(CAST(binding_json AS BLOB))<=8192), next_ordinal INTEGER NOT NULL CHECK(next_ordinal BETWEEN 2 AND 4), winner_attempt_id TEXT CHECK(winner_attempt_id IS NULL OR length(winner_attempt_id)=36), disposition TEXT NOT NULL CHECK(disposition IN('open','cancelled','adopted')), sequence INTEGER NOT NULL CHECK(sequence>0), last_operation_id TEXT NOT NULL CHECK(length(last_operation_id)=36), last_payload_digest TEXT NOT NULL CHECK(length(last_payload_digest)=64), PRIMARY KEY(owner_key,request_id,request_incarnation_id), UNIQUE(owner_key,request_id)) STRICT"),
    ("media_resource_attempts", "CREATE TABLE media_resource_attempts (attempt_id TEXT PRIMARY KEY CHECK(length(attempt_id)=36), owner_key TEXT NOT NULL, request_id TEXT NOT NULL, request_incarnation_id TEXT NOT NULL, ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 1 AND 3), route_incarnation_id TEXT NOT NULL UNIQUE CHECK(length(route_incarnation_id)=36), session_id TEXT NOT NULL UNIQUE CHECK(length(session_id)=36), worker_node_id TEXT NOT NULL CHECK(length(CAST(worker_node_id AS BLOB)) BETWEEN 1 AND 256), owner_epoch INTEGER NOT NULL CHECK(owner_epoch=ordinal), key_json TEXT NOT NULL CHECK(json_valid(key_json) AND length(CAST(key_json AS BLOB))<=2048), worker_instance_id TEXT NOT NULL CHECK(length(worker_instance_id)=36), serving_generation BLOB NOT NULL CHECK(length(serving_generation)=8), domains_json TEXT NOT NULL CHECK(json_valid(domains_json) AND json_type(domains_json)='array' AND json_array_length(domains_json) BETWEEN 1 AND 32 AND length(CAST(domains_json AS BLOB))<=16384), worker_json TEXT CHECK(worker_json IS NULL OR (json_valid(worker_json) AND length(CAST(worker_json AS BLOB))<=4096)), phase TEXT NOT NULL CHECK(phase IN('provisional','prepared','winner','losing','cleanup','closed')), sequence INTEGER NOT NULL CHECK(sequence>0), last_operation_id TEXT NOT NULL CHECK(length(last_operation_id)=36), last_payload_digest TEXT NOT NULL CHECK(length(last_payload_digest)=64), updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms>0), CHECK(phase NOT IN('prepared','winner') OR worker_json IS NOT NULL), UNIQUE(owner_key,request_id,request_incarnation_id,ordinal), UNIQUE(owner_key,request_id,request_incarnation_id,worker_node_id), FOREIGN KEY(owner_key,request_id,request_incarnation_id) REFERENCES media_resource_requests(owner_key,request_id,request_incarnation_id)) STRICT"),
    ("media_resource_operations", "CREATE TABLE media_resource_operations (operation_id TEXT PRIMARY KEY CHECK(length(operation_id)=36), owner_key TEXT NOT NULL, request_id TEXT NOT NULL, request_incarnation_id TEXT NOT NULL, attempt_id TEXT NOT NULL, action TEXT NOT NULL CHECK(action IN('claim','prepared','winner','adopt','fence')), payload_digest TEXT NOT NULL CHECK(length(payload_digest)=64), request_sequence INTEGER NOT NULL CHECK(request_sequence>0), attempt_sequence INTEGER NOT NULL CHECK(attempt_sequence>0), FOREIGN KEY(attempt_id) REFERENCES media_resource_attempts(attempt_id)) STRICT"),
    ("media_resource_assertions", "CREATE TABLE media_resource_assertions (singleton INTEGER PRIMARY KEY CHECK(singleton=1), passed INTEGER NOT NULL CHECK(passed=1)) STRICT"),
    ("media_resource_worker_inventory", "CREATE INDEX media_resource_worker_inventory ON media_resource_attempts(worker_node_id,attempt_id) WHERE phase!='closed'"),
];

/// A claim is opened and removed in the *same* serialized transaction. This
/// predicate matches a specific row/action, not a global maintenance bypass.
fn claim(predicate: &str) -> String {
    format!("EXISTS(SELECT 1 FROM media_resource_schema m WHERE m.singleton=1 AND m.version=1 AND m.mutation_claim IS NOT NULL AND ({predicate}))")
}

fn field(name: &str) -> String {
    format!("json_extract(m.mutation_claim,'$.{name}')")
}

fn scoped(alias: &str) -> String {
    format!(
        "{}={alias}.owner_key AND {}={alias}.request_id AND {}={alias}.request_incarnation_id",
        field("owner_key"),
        field("request_id"),
        field("request_incarnation_id")
    )
}

pub fn trigger_objects(layout: Layout) -> Vec<(String, String)> {
    let mut objects = Vec::new();
    let mut trigger = |name: &str, event: &str, table: &str, predicate: String| {
        let name = format!("media_resource_{name}");
        let sql = format!("CREATE TRIGGER {name} BEFORE {event} ON {table} WHEN {predicate} BEGIN SELECT RAISE(ABORT,'adaptive resource writer fenced'); END");
        objects.push((name, sql));
    };
    for (table, short) in [
        ("media_resource_requests", "request"),
        ("media_resource_attempts", "attempt"),
        ("media_resource_operations", "operation"),
    ] {
        trigger(&format!("{short}_delete"), "DELETE", table, "1".into());
        let authorized = claim(&scoped("NEW"));
        trigger(
            &format!("{short}_insert"),
            "INSERT",
            table,
            format!("NOT({authorized})"),
        );
        // INSERT guards also cover REPLACE with recursive_triggers=OFF. A
        // successful INSERT must never overwrite retained operation history.
        let key = match short {
            "request" => "owner_key=NEW.owner_key AND request_id=NEW.request_id",
            "attempt" => "attempt_id=NEW.attempt_id OR route_incarnation_id=NEW.route_incarnation_id OR session_id=NEW.session_id OR (owner_key=NEW.owner_key AND request_id=NEW.request_id AND (ordinal=NEW.ordinal OR worker_node_id=NEW.worker_node_id))",
            _ => "operation_id=NEW.operation_id",
        };
        trigger(
            &format!("{short}_replace"),
            "INSERT",
            table,
            format!("EXISTS(SELECT 1 FROM {table} WHERE {key})"),
        );
        let immutable = match short {
            "request" => "NEW.owner_key!=OLD.owner_key OR NEW.request_id!=OLD.request_id OR NEW.request_incarnation_id!=OLD.request_incarnation_id OR NEW.binding_json!=OLD.binding_json",
            "attempt" => "NEW.attempt_id!=OLD.attempt_id OR NEW.owner_key!=OLD.owner_key OR NEW.request_id!=OLD.request_id OR NEW.request_incarnation_id!=OLD.request_incarnation_id OR NEW.ordinal!=OLD.ordinal OR NEW.owner_epoch!=OLD.owner_epoch OR NEW.route_incarnation_id!=OLD.route_incarnation_id OR NEW.session_id!=OLD.session_id OR NEW.worker_node_id!=OLD.worker_node_id OR NEW.key_json!=OLD.key_json OR NEW.worker_instance_id!=OLD.worker_instance_id OR NEW.serving_generation!=OLD.serving_generation OR NEW.domains_json!=OLD.domains_json OR (OLD.worker_json IS NOT NULL AND NEW.worker_json IS NOT OLD.worker_json) OR NEW.phase='closed'",
            _ => "1",
        };
        trigger(
            &format!("{short}_update"),
            "UPDATE",
            table,
            format!("({immutable}) OR NOT({authorized})"),
        );
    }
    for (event, alias) in [("INSERT", "NEW"), ("UPDATE", "OLD"), ("DELETE", "OLD")] {
        let owner = layout.owner(alias);
        let affected = format!("EXISTS(SELECT 1 FROM media_resource_requests r WHERE r.owner_key={owner} AND r.request_id={alias}.request_id)");
        let permitted = claim(&format!(
            "{}={owner} AND {}={alias}.request_id AND {}='adopt'",
            field("owner_key"),
            field("request_id"),
            field("action")
        ));
        trigger(
            &format!("root_{}", event.to_ascii_lowercase()),
            event,
            "media_session_requests",
            format!("({affected}) AND NOT({permitted})"),
        );
    }
    // Protect both the old and proposed identities on update/REPLACE. Ordinary
    // adopted exact-owner renewal/publication/end is allowed, but takeover,
    // recipe rewriting, and deleting history cannot hide an obligation.
    for (event, alias) in [("INSERT", "NEW"), ("UPDATE", "OLD"), ("DELETE", "OLD")] {
        let affected = format!("EXISTS(SELECT 1 FROM media_resource_attempts a WHERE a.route_incarnation_id={alias}.incarnation_id OR a.session_id={alias}.session_id)");
        let permitted = claim(&format!("{}={alias}.incarnation_id AND {}={alias}.session_id AND {} IN('claim','adopt','fence')",field("route_incarnation_id"),field("session_id"),field("action")));
        let ordinary = if event == "UPDATE" {
            format!("EXISTS(SELECT 1 FROM media_resource_attempts a JOIN media_resource_requests r USING(owner_key,request_id,request_incarnation_id) WHERE a.route_incarnation_id=OLD.incarnation_id AND r.disposition='adopted' AND r.winner_attempt_id=a.attempt_id AND NEW.incarnation_id=OLD.incarnation_id AND NEW.session_id=OLD.session_id AND {}={} AND NEW.playback_id=OLD.playback_id AND NEW.request_fingerprint=OLD.request_fingerprint AND NEW.owner_node_id=OLD.owner_node_id AND NEW.owner_epoch=OLD.owner_epoch AND NEW.recipe_json=OLD.recipe_json AND NEW.recovery_epoch=OLD.recovery_epoch AND NEW.state IN('active','ended'))",layout.owner("NEW"),layout.owner("OLD"))
        } else {
            "0".into()
        };
        trigger(
            &format!("route_{}", event.to_ascii_lowercase()),
            event,
            "media_sessions",
            format!("({affected}) AND NOT(({permitted}) OR ({ordinary}))"),
        );
        let lease_affected = format!("EXISTS(SELECT 1 FROM media_resource_attempts a WHERE {alias}.resource='session:'||a.route_incarnation_id)");
        let lease_permitted = claim(&format!(
            "{alias}.resource='session:'||{} AND {} IN('claim','adopt','fence')",
            field("route_incarnation_id"),
            field("action")
        ));
        let lease_ordinary = if event == "UPDATE" {
            "EXISTS(SELECT 1 FROM media_resource_attempts a JOIN media_resource_requests r USING(owner_key,request_id,request_incarnation_id) WHERE OLD.resource='session:'||a.route_incarnation_id AND r.disposition='adopted' AND r.winner_attempt_id=a.attempt_id AND NEW.resource=OLD.resource AND NEW.owner_node_id=OLD.owner_node_id AND NEW.fence=OLD.fence)"
        } else {
            "0"
        };
        trigger(
            &format!("lease_{}", event.to_ascii_lowercase()),
            event,
            "job_leases",
            format!("({lease_affected}) AND NOT(({lease_permitted}) OR ({lease_ordinary}))"),
        );
    }
    for event in ["INSERT", "UPDATE"] {
        let affected="EXISTS(SELECT 1 FROM media_resource_attempts a WHERE a.route_incarnation_id=NEW.current_incarnation_id)";
        let permitted = claim(&format!(
            "{}=NEW.current_incarnation_id AND {}='adopt'",
            field("route_incarnation_id"),
            field("action")
        ));
        trigger(
            &format!("pointer_{}", event.to_ascii_lowercase()),
            event,
            "media_playback_pointers",
            format!("({affected}) AND NOT({permitted})"),
        );
    }
    objects
}

pub fn objects(layout: Layout) -> Vec<(String, String)> {
    TABLES
        .iter()
        .map(|(name, sql)| ((*name).to_owned(), (*sql).to_owned()))
        .chain(trigger_objects(layout))
        .collect()
}

pub const CATALOG_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE sql IS NOT NULL AND (name GLOB 'media_resource_*' OR tbl_name GLOB 'media_resource_*') ORDER BY name";

pub fn exact_objects_guard(layout: Layout) -> String {
    let objects = objects(layout);
    let predicates = objects
        .iter()
        .map(|(name, sql)| {
            format!(
                "EXISTS(SELECT 1 FROM sqlite_master WHERE name={} AND sql={})",
                quote(name),
                quote(sql)
            )
        })
        .collect::<Vec<_>>();
    format!("(SELECT count(*) FROM sqlite_master WHERE sql IS NOT NULL AND (name GLOB 'media_resource_*' OR tbl_name GLOB 'media_resource_*'))={} AND ({})",objects.len(),predicates.join(") AND ("))
}

/// Names only support the main-owned baseline count. Admission ALSO requires
/// exact canonical adjunct objects, never a name-only whitelist.
pub fn session_trigger_names(layout: Layout) -> Vec<String> {
    trigger_objects(layout)
        .into_iter()
        .filter_map(|(name, sql)| {
            [
                "media_session_requests",
                "media_sessions",
                "media_playback_pointers",
            ]
            .iter()
            .any(|table| sql.contains(&format!(" ON {table} ")))
            .then_some(name)
        })
        .collect()
}

/// Exact base layout; no optional adjunct table reads, so it is safe before
/// installation. Unknown additional protected-family objects refuse admission.
pub fn session_shape_guard(layout: Layout) -> String {
    if layout == Layout::Principal {
        return crate::store::sharing_source_sessions::schema_guard();
    }
    type Object = (String, String, String, Vec<Option<String>>);
    let objects: Vec<Object> =
        serde_json::from_str(include_str!("../sharing_source_legacy_schema.json"))
            .expect("frozen recognized legacy session layout");
    let families = [
        "media_session_requests",
        "media_playback_pointers",
        "media_sessions",
        "media_session_preparations",
        "media_playback_desired",
        "media_session_producer_recovery",
        "library_channel_session_recipes",
    ];
    let families = families
        .iter()
        .map(|table| quote(table))
        .collect::<Vec<_>>()
        .join(",");
    let excluded = session_trigger_names(layout)
        .iter()
        .map(|name| quote(name))
        .collect::<Vec<_>>()
        .join(",");
    let exact=objects.iter().map(|(kind,name,table,definitions)| {
        let alternatives=definitions.iter().map(|sql|sql.as_ref().map_or_else(||"sql IS NULL".into(),|sql|format!("sql={}",quote(sql)))).collect::<Vec<_>>().join(" OR ");
        format!("EXISTS(SELECT 1 FROM sqlite_master WHERE type={} AND name={} AND tbl_name={} AND ({alternatives}))",quote(kind),quote(name),quote(table))
    }).collect::<Vec<_>>().join(") AND (");
    format!("(SELECT count(*) FROM sqlite_master WHERE (tbl_name IN({families}) OR name='background_analysis_viewer_refresh') AND name NOT IN({excluded}))={} AND ({exact})",objects.len())
}

/// For use INSIDE the same transaction/snapshot as the coordination read/write.
pub fn ready_guard(layout: Layout) -> String {
    format!("({}) AND ({}) AND EXISTS(SELECT 1 FROM media_resource_schema WHERE singleton=1 AND version=1 AND layout={} AND revision>=0 AND mutation_claim IS NULL) AND NOT EXISTS(SELECT 1 FROM media_resource_assertions)",exact_objects_guard(layout),session_shape_guard(layout),quote(layout.tag()))
}

pub fn absent_guard() -> &'static str {
    "NOT EXISTS(SELECT 1 FROM sqlite_master WHERE name GLOB 'media_resource_*' OR tbl_name GLOB 'media_resource_*')"
}

/// Evaluate only after authoritative present/absent dispatch. Even expired or
/// ended routes cannot erase a nonclosed attempt during destructive rebuild.
pub fn migration_drained_guard() -> String {
    format!(
        "({}) AND NOT EXISTS(SELECT 1 FROM media_resource_attempts WHERE phase!='closed')",
        ready_guard(Layout::Legacy)
    )
}

pub fn detach_session_triggers(layout: Layout) -> Vec<String> {
    session_trigger_names(layout)
        .into_iter()
        .map(|name| format!("DROP TRIGGER {name}"))
        .collect()
}

/// Main appends these INSIDE the already-admitted Source rebuild transaction.
/// Lease/adjunct guards remain installed; all request/attempt/history rows stay.
pub fn rebind_principal_statements() -> Vec<String> {
    let names = session_trigger_names(Layout::Principal);
    let mut statements = trigger_objects(Layout::Principal)
        .into_iter()
        .filter(|(name, _)| names.contains(name))
        .map(|(_, sql)| sql)
        .collect::<Vec<_>>();
    statements.push("UPDATE media_resource_schema SET layout='principal-v1',revision=revision+1 WHERE singleton=1 AND layout='local-v1' AND mutation_claim IS NULL AND revision<9223372036854775807".into());
    statements.push(assertion(&ready_guard(Layout::Principal)));
    statements
}

pub(crate) fn ordinary_request_filter(layout: Layout, alias: &str, installed: bool) -> String {
    if !installed {
        return "1".into();
    }
    format!("NOT EXISTS(SELECT 1 FROM media_resource_requests adaptive WHERE adaptive.owner_key={} AND adaptive.request_id={alias}.request_id)", layout.owner(alias))
}

pub(crate) fn ordinary_route_filter(alias: &str, installed: bool) -> String {
    if !installed {
        return "1".into();
    }
    format!("NOT EXISTS(SELECT 1 FROM media_resource_attempts adaptive WHERE adaptive.route_incarnation_id={alias}.incarnation_id)")
}

pub(crate) fn ordinary_lease_filter(alias: &str, installed: bool) -> String {
    if !installed {
        return "1".into();
    }
    format!("NOT EXISTS(SELECT 1 FROM media_resource_attempts adaptive WHERE {alias}.resource='session:'||adaptive.route_incarnation_id)")
}

/// Exact canonical adjunct objects, not an object-name whitelist.
pub fn verify_shape(rows: &[(String, String)], layout: Layout) -> bool {
    let expected = objects(layout);
    rows.len() == expected.len() && expected.iter().all(|object| rows.contains(object))
}

pub fn sqlite_ready(conn: &Connection, layout: Layout) -> Result<bool, StoreError> {
    let rows = conn
        .prepare(CATALOG_SQL)?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<(String, String)>, _>>()?;
    if !verify_shape(&rows, layout) {
        return Ok(false);
    }
    Ok(conn
        .query_row(&format!("SELECT ({})", ready_guard(layout)), [], |r| {
            r.get::<_, bool>(0)
        })
        .optional()?
        .unwrap_or(false))
}

/// Startup-only additive installation. Unknown objects are never repaired.
/// Caller owns the existing schema-migration admission and checked layout.
pub fn install_sqlite(conn: &Connection, layout: Layout) -> Result<(), StoreError> {
    let tx = conn.unchecked_transaction()?;
    let absent: bool = tx.query_row(&format!("SELECT ({})", absent_guard()), [], |r| r.get(0))?;
    if !absent {
        return Ok(());
    }
    let recognized: bool = tx.query_row(
        &format!("SELECT ({})", session_shape_guard(layout)),
        [],
        |row| row.get(0),
    )?;
    if !recognized {
        return Ok(());
    }
    for (_, sql) in objects(layout) {
        tx.execute(&sql, [])?;
    }
    tx.execute(
        &format!(
            "INSERT INTO media_resource_schema(singleton,version,layout,revision) VALUES(1,1,{},0)",
            quote(layout.tag())
        ),
        [],
    )?;
    if !sqlite_ready(&tx, layout)? {
        return Err(StoreError::Migration(
            "adaptive adjunct installation did not verify".into(),
        ));
    }
    tx.commit()?;
    Ok(())
}

/// Open-path dispatch for both recognized session layouts. Partial ownership
/// columns do not select a permissive interpretation.
pub fn install_sqlite_checked(conn: &Connection) -> Result<(), StoreError> {
    let count:i64=conn.query_row("SELECT count(*) FROM pragma_table_info('media_sessions') WHERE name IN('owner_key','principal_kind','share_grant_id','share_viewer_key')",[],|row|row.get(0))?;
    match count {
        0 => install_sqlite(conn, Layout::Legacy),
        4 => install_sqlite(conn, Layout::Principal),
        _ => Ok(()),
    }
}

/// Assert without relying on a conditional statement's row count. A failed
/// constraint aborts the actual enclosing transaction; transport errors remain
/// uncertain until authoritative operation reconciliation.
pub(crate) fn assertion(condition: &str) -> String {
    format!(
        "INSERT INTO media_resource_assertions(singleton,passed) SELECT 1,0 WHERE NOT({condition})"
    )
}
