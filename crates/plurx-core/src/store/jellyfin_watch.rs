//! Manual edit revisions fence compatibility beats without changing native progress.
use super::JellyfinPlayScope;
use crate::error::StoreError;

pub(crate) const SCHEMA: &str = r#"
ALTER TABLE watch_state ADD COLUMN manual_revision INTEGER NOT NULL DEFAULT 0 CHECK(manual_revision>=0);
ALTER TABLE watch_state ADD COLUMN manual_origin TEXT CHECK(manual_origin IS NULL OR json_valid(manual_origin));
CREATE TRIGGER IF NOT EXISTS jellyfin_watch_own_insert AFTER INSERT ON watch_state
WHEN NEW.manual_revision=1 AND NEW.manual_origin IS NOT NULL
BEGIN
 UPDATE jellyfin_plays SET manual_revision=NEW.manual_revision
 WHERE user_id=NEW.user_id AND item_id=NEW.item_id AND state='active' AND manual_revision=0
 AND token_digest=json_extract(NEW.manual_origin,'$.token_digest')
 AND device_digest=json_extract(NEW.manual_origin,'$.device_digest')
 AND client_family=json_extract(NEW.manual_origin,'$.client_family')
 AND EXISTS(SELECT 1 FROM jellyfin_login_tokens l JOIN tokens t ON t.token_hash=l.token_hash
   WHERE l.token_hash=jellyfin_plays.token_digest AND l.user_id=NEW.user_id AND l.device_digest=jellyfin_plays.device_digest AND l.client_family=jellyfin_plays.client_family)
 AND ((direct_grant_id IS NOT NULL AND EXISTS(SELECT 1 FROM file_grants g WHERE g.id=jellyfin_plays.direct_grant_id AND g.user_id=NEW.user_id AND g.file_id=jellyfin_plays.file_id AND g.source_token_hash=jellyfin_plays.token_digest AND g.revoked_at IS NULL AND g.expires_at>json_extract(NEW.manual_origin,'$.observed_at_sec'))) OR (native_incarnation_id IS NOT NULL AND EXISTS(SELECT 1 FROM media_sessions m WHERE m.incarnation_id=jellyfin_plays.native_incarnation_id AND m.user_id=NEW.user_id AND m.state='active' AND m.lease_expires_at_ms>json_extract(NEW.manual_origin,'$.observed_at_sec')*1000)))
 AND (SELECT COUNT(*) FROM jellyfin_plays p WHERE p.user_id=NEW.user_id AND p.item_id=NEW.item_id AND p.state='active'
   AND p.token_digest=json_extract(NEW.manual_origin,'$.token_digest') AND p.device_digest=json_extract(NEW.manual_origin,'$.device_digest') AND p.client_family=json_extract(NEW.manual_origin,'$.client_family') AND ((p.direct_grant_id IS NOT NULL AND EXISTS(SELECT 1 FROM file_grants g WHERE g.id=p.direct_grant_id AND g.user_id=NEW.user_id AND g.file_id=p.file_id AND g.source_token_hash=p.token_digest AND g.revoked_at IS NULL AND g.expires_at>json_extract(NEW.manual_origin,'$.observed_at_sec'))) OR (p.native_incarnation_id IS NOT NULL AND EXISTS(SELECT 1 FROM media_sessions m WHERE m.incarnation_id=p.native_incarnation_id AND m.user_id=NEW.user_id AND m.state='active' AND m.lease_expires_at_ms>json_extract(NEW.manual_origin,'$.observed_at_sec')*1000))))=1;
END;
CREATE TRIGGER IF NOT EXISTS jellyfin_watch_own_update AFTER UPDATE OF manual_revision ON watch_state
WHEN NEW.manual_revision=OLD.manual_revision+1 AND NEW.manual_origin IS NOT NULL
BEGIN
 UPDATE jellyfin_plays SET manual_revision=NEW.manual_revision
 WHERE user_id=NEW.user_id AND item_id=NEW.item_id AND state='active' AND manual_revision=OLD.manual_revision
 AND token_digest=json_extract(NEW.manual_origin,'$.token_digest')
 AND device_digest=json_extract(NEW.manual_origin,'$.device_digest')
 AND client_family=json_extract(NEW.manual_origin,'$.client_family')
 AND EXISTS(SELECT 1 FROM jellyfin_login_tokens l JOIN tokens t ON t.token_hash=l.token_hash
   WHERE l.token_hash=jellyfin_plays.token_digest AND l.user_id=NEW.user_id AND l.device_digest=jellyfin_plays.device_digest AND l.client_family=jellyfin_plays.client_family)
 AND ((direct_grant_id IS NOT NULL AND EXISTS(SELECT 1 FROM file_grants g WHERE g.id=jellyfin_plays.direct_grant_id AND g.user_id=NEW.user_id AND g.file_id=jellyfin_plays.file_id AND g.source_token_hash=jellyfin_plays.token_digest AND g.revoked_at IS NULL AND g.expires_at>json_extract(NEW.manual_origin,'$.observed_at_sec'))) OR (native_incarnation_id IS NOT NULL AND EXISTS(SELECT 1 FROM media_sessions m WHERE m.incarnation_id=jellyfin_plays.native_incarnation_id AND m.user_id=NEW.user_id AND m.state='active' AND m.lease_expires_at_ms>json_extract(NEW.manual_origin,'$.observed_at_sec')*1000)))
 AND (SELECT COUNT(*) FROM jellyfin_plays p WHERE p.user_id=NEW.user_id AND p.item_id=NEW.item_id AND p.state='active'
   AND p.token_digest=json_extract(NEW.manual_origin,'$.token_digest') AND p.device_digest=json_extract(NEW.manual_origin,'$.device_digest') AND p.client_family=json_extract(NEW.manual_origin,'$.client_family') AND ((p.direct_grant_id IS NOT NULL AND EXISTS(SELECT 1 FROM file_grants g WHERE g.id=p.direct_grant_id AND g.user_id=NEW.user_id AND g.file_id=p.file_id AND g.source_token_hash=p.token_digest AND g.revoked_at IS NULL AND g.expires_at>json_extract(NEW.manual_origin,'$.observed_at_sec'))) OR (p.native_incarnation_id IS NOT NULL AND EXISTS(SELECT 1 FROM media_sessions m WHERE m.incarnation_id=p.native_incarnation_id AND m.user_id=NEW.user_id AND m.state='active' AND m.lease_expires_at_ms>json_extract(NEW.manual_origin,'$.observed_at_sec')*1000))))=1;
END;
CREATE TRIGGER IF NOT EXISTS jellyfin_watch_final_insert AFTER INSERT ON watch_state
WHEN NEW.manual_origin IS NOT NULL AND json_extract(NEW.manual_origin,'$.final_play_id') IS NOT NULL
BEGIN
 UPDATE jellyfin_plays SET state='ended',expires_at_ms=json_extract(NEW.manual_origin,'$.terminal_expiry')
 WHERE play_id=json_extract(NEW.manual_origin,'$.final_play_id') AND user_id=NEW.user_id AND item_id=NEW.item_id AND state='active' AND manual_revision=NEW.manual_revision
 AND token_digest=json_extract(NEW.manual_origin,'$.token_digest') AND device_digest=json_extract(NEW.manual_origin,'$.device_digest') AND client_family=json_extract(NEW.manual_origin,'$.client_family');
END;
CREATE TRIGGER IF NOT EXISTS jellyfin_watch_final_update AFTER UPDATE OF manual_origin ON watch_state
WHEN NEW.manual_origin IS NOT NULL AND json_extract(NEW.manual_origin,'$.final_play_id') IS NOT NULL
BEGIN
 UPDATE jellyfin_plays SET state='ended',expires_at_ms=json_extract(NEW.manual_origin,'$.terminal_expiry')
 WHERE play_id=json_extract(NEW.manual_origin,'$.final_play_id') AND user_id=NEW.user_id AND item_id=NEW.item_id AND state='active' AND manual_revision=NEW.manual_revision
 AND token_digest=json_extract(NEW.manual_origin,'$.token_digest') AND device_digest=json_extract(NEW.manual_origin,'$.device_digest') AND client_family=json_extract(NEW.manual_origin,'$.client_family');
END;
"#;

/// External watch-state triggers must be removed while the referenced session
/// table is replaced, then restored verbatim in the same rebuild transaction.
pub(crate) fn session_dependency_triggers() -> impl Iterator<Item = (&'static str, &'static str)> {
    ["jellyfin_watch_own_insert", "jellyfin_watch_own_update"]
        .into_iter()
        .map(|name| {
            let marker = format!("CREATE TRIGGER IF NOT EXISTS {name} ");
            let start = SCHEMA
                .find(&marker)
                .expect("closed watch trigger definition");
            let end = start
                + SCHEMA[start..]
                    .find("\nEND;")
                    .expect("closed watch trigger body")
                + "\nEND;".len();
            (name, &SCHEMA[start..end])
        })
}

pub(crate) fn session_dependency_guard() -> String {
    session_dependency_triggers().map(|(name, sql)| {
        let declaration = sql.trim_end_matches(';').replace("CREATE TRIGGER IF NOT EXISTS", "CREATE TRIGGER").replace('\'', "''");
        format!("EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND name='{name}' AND tbl_name='watch_state' AND sql='{declaration}')")
    }).collect::<Vec<_>>().join(" AND ")
}

pub(crate) fn origin_json(
    user_id: i64,
    origin: Option<&JellyfinPlayScope>,
    now: i64,
) -> Result<Option<String>, StoreError> {
    let Some(origin) = origin else {
        return Ok(None);
    };
    super::jellyfin_play::validate_scope(origin)?;
    if origin.user_id != user_id {
        return Err(StoreError::Identity(
            "manual edit origin user mismatch".into(),
        ));
    }
    if now < 0 {
        return Err(StoreError::Identity("invalid manual edit clock".into()));
    }
    let mut context =
        serde_json::to_value(origin).map_err(|e| StoreError::Identity(e.to_string()))?;
    context["observed_at_sec"] = serde_json::json!(now);
    serde_json::to_string(&context)
        .map(Some)
        .map_err(|e| StoreError::Identity(e.to_string()))
}

/// A single statement keeps the edit, revision bump and eligible own-play
/// advance atomic on both backends. The materialized prior rows retain the
/// native notification set and timestamp behavior even for explicit no-ops.
pub(crate) fn manual_sql(tree: bool, watched: bool) -> String {
    let targets = if tree {
        "WITH RECURSIVE tree(id) AS (SELECT id FROM items WHERE id=$1 UNION SELECT i.id FROM items i JOIN tree t ON i.parent_id=t.id), targets(id) AS (SELECT i.id FROM tree t JOIN items i ON i.id=t.id WHERE i.kind IN ('movie','episode','video','audiobook'))"
    } else {
        "WITH targets(id) AS (SELECT $1)"
    };
    let value = if watched { "1" } else { "0" };
    let changed = if watched {
        "w.watched=0"
    } else {
        "w.watched=1 OR w.position_ms<>0"
    };
    let updated = if tree {
        "CASE WHEN (SELECT changed FROM prior WHERE id=watch_state.item_id) THEN $3 ELSE watch_state.updated_at END"
    } else {
        "$3"
    };
    format!("{targets}, prior AS MATERIALIZED (SELECT t.id,CASE WHEN w.item_id IS NULL OR {changed} THEN 1 ELSE 0 END AS changed FROM targets t LEFT JOIN watch_state w ON w.user_id=$2 AND w.item_id=t.id) INSERT INTO watch_state(user_id,item_id,position_ms,watched,updated_at,manual_revision,manual_origin) SELECT $2,id,0,{value},$3,1,$4 FROM prior WHERE true ON CONFLICT(user_id,item_id) DO UPDATE SET watched={value},position_ms=CASE WHEN {value}=0 THEN 0 ELSE watch_state.position_ms END,updated_at={updated},manual_revision=watch_state.manual_revision+1,manual_origin=$4 RETURNING item_id AS id,(SELECT changed FROM prior WHERE id=watch_state.item_id) AS changed")
}

#[cfg(feature = "hiqlite-store")]
pub(crate) struct EditRow {
    pub id: i64,
    pub changed: bool,
}
#[cfg(feature = "hiqlite-store")]
impl From<&mut hiqlite::Row<'_>> for EditRow {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self {
            id: row.get("id"),
            changed: row.get::<i64>("changed") != 0,
        }
    }
}

#[cfg(feature = "hiqlite-store")]
pub(crate) struct ColumnRow {
    pub name: String,
}
#[cfg(feature = "hiqlite-store")]
impl From<&mut hiqlite::Row<'_>> for ColumnRow {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self {
            name: row.get("name"),
        }
    }
}
#[cfg(feature = "hiqlite-store")]
pub(crate) fn migration_statements(columns: &[ColumnRow]) -> Vec<(String, hiqlite::Params)> {
    super::hiqlite_library_channels::split_schema_statements(SCHEMA)
        .into_iter()
        .filter(|sql| {
            !(sql.starts_with("ALTER TABLE watch_state ADD COLUMN manual_revision ")
                && columns.iter().any(|c| c.name == "manual_revision")
                || sql.starts_with("ALTER TABLE watch_state ADD COLUMN manual_origin ")
                    && columns.iter().any(|c| c.name == "manual_origin"))
        })
        .map(|sql| (sql, hiqlite::params!()))
        .collect()
}

/// Queued observations retain their original revision; never relabel a beat
/// after a manual edit advances an eligible play binding.
#[derive(Clone)]
pub struct JellyfinProgressProvenance {
    pub play_id: String,
    pub scope: JellyfinPlayScope,
    pub manual_revision: i64,
}
#[derive(Clone)]
pub struct JellyfinProgressWrite {
    pub provenance: JellyfinProgressProvenance,
    pub item_id: i64,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub final_commit: bool,
}

pub(crate) const PROGRESS: &str = r#"
WITH args AS (SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12), eligible AS (
 SELECT p.item_id FROM jellyfin_plays p
 JOIN tokens t ON t.token_hash=p.token_digest AND t.user_id=p.user_id
 JOIN jellyfin_login_tokens l ON l.token_hash=t.token_hash AND l.user_id=p.user_id AND l.device_digest=p.device_digest AND l.client_family=p.client_family
 JOIN jellyfin_entity_ids i ON i.entity_kind='item' AND i.wire_id=p.item_wire_id AND i.native_id=p.item_id AND i.retired=0
 JOIN jellyfin_entity_ids f ON f.entity_kind='file' AND f.wire_id=p.file_wire_id AND f.native_id=p.file_id AND f.retired=0
 JOIN items supported_item ON supported_item.id=p.item_id AND supported_item.kind IN ('movie','episode')
 JOIN libraries supported_library ON supported_library.id=supported_item.library_id AND supported_library.kind IN ('movies','shows')
 JOIN files native_file ON native_file.id=p.file_id AND native_file.item_id=p.item_id AND (json_extract(json_extract(p.payload,'$.selection_json'),'$.source.size') IS NULL OR (native_file.size=json_extract(json_extract(p.payload,'$.selection_json'),'$.source.size') AND native_file.mtime=json_extract(json_extract(p.payload,'$.selection_json'),'$.source.mtime')))
 AND (json_type(json_extract(p.payload,'$.selection_json'),'$.source.probe') IS NULL OR native_file.probe_json IS json_extract(json_extract(p.payload,'$.selection_json'),'$.source.probe'))
 LEFT JOIN watch_state w ON w.user_id=p.user_id AND w.item_id=p.item_id
 WHERE p.play_id=$6 AND p.user_id=$4 AND p.token_digest=$7 AND p.device_digest=$8 AND p.client_family=$9
 AND p.item_id=$1 AND p.state='active' AND ((p.direct_grant_id IS NOT NULL AND ($12 IS NOT NULL OR EXISTS(SELECT 1 FROM file_grants g WHERE g.id=p.direct_grant_id AND g.user_id=p.user_id AND g.file_id=p.file_id AND g.source_token_hash=p.token_digest AND g.revoked_at IS NULL AND g.expires_at>$5)))
 OR EXISTS(SELECT 1 FROM media_sessions m WHERE m.incarnation_id=p.native_incarnation_id AND m.user_id=p.user_id AND m.playback_id=p.playback_id AND m.request_fingerprint=json_extract(p.payload,'$.native_request_fingerprint') AND m.media_origin_ms=json_extract(p.payload,'$.source_origin_ms')
 AND ((m.state='active' AND m.publication_ready_at_ms=0 AND m.lease_expires_at_ms>$5*1000 AND EXISTS(SELECT 1 FROM media_playback_pointers ptr WHERE ptr.user_id=p.user_id AND ptr.playback_id=p.playback_id AND ptr.current_incarnation_id=m.incarnation_id))
 OR ($12 IS NOT NULL AND m.state='ended' AND m.terminal_reason='deleted' AND NOT EXISTS(SELECT 1 FROM media_playback_pointers ptr WHERE ptr.user_id=p.user_id AND ptr.playback_id=p.playback_id))))) AND p.manual_revision=$10 AND COALESCE(w.manual_revision,0)=$10
 AND ($11 IS NULL OR (w.item_id IS NOT NULL AND w.position_ms=json_extract($11,'$.position_ms') AND w.duration_ms IS json_extract($11,'$.duration_ms') AND w.watched=json_extract($11,'$.watched') AND w.updated_at=json_extract($11,'$.updated_at')))
), input(duration_ms) AS (
 SELECT COALESCE((SELECT CASE WHEN i.kind='audiobook' THEN SUM(f.duration_ms) ELSE MAX(f.duration_ms) END FROM items i JOIN files f ON f.item_id=i.id WHERE i.id=$1 AND f.duration_ms>0 GROUP BY i.kind),CASE WHEN $2>0 THEN $2 END)
), normalized(position_ms,duration_ms) AS (
 SELECT CASE WHEN duration_ms IS NULL THEN MAX($3,0) ELSE MIN(MAX($3,0),duration_ms) END,duration_ms FROM input
)
INSERT INTO watch_state(user_id,item_id,position_ms,duration_ms,watched,updated_at,manual_origin)
SELECT $4,$1,position_ms,duration_ms,CASE WHEN duration_ms IS NOT NULL AND position_ms*1.0/duration_ms>=0.95 THEN 1 ELSE 0 END,$5,$12
FROM normalized WHERE EXISTS(SELECT 1 FROM eligible)
ON CONFLICT(user_id,item_id) DO UPDATE SET position_ms=excluded.position_ms,duration_ms=COALESCE(excluded.duration_ms,watch_state.duration_ms),watched=watch_state.watched OR excluded.watched,updated_at=excluded.updated_at,manual_origin=$12
RETURNING position_ms,duration_ms,watched,updated_at
"#;

pub(crate) fn progress_context(
    write: &JellyfinProgressWrite,
    now: i64,
    expected: Option<&crate::domain::WatchState>,
) -> Result<(Option<String>, Option<String>), StoreError> {
    super::jellyfin_play::validate_key(&write.provenance.play_id, &write.provenance.scope)?;
    if write.item_id <= 0 || write.provenance.manual_revision < 0 || now < 0 {
        return Err(StoreError::Identity(
            "invalid compatibility progress provenance".into(),
        ));
    }
    let expected = expected
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| StoreError::Identity(e.to_string()))?;
    let context = if write.final_commit {
        let millis = now
            .checked_mul(1000)
            .ok_or_else(|| StoreError::Identity("compatibility final time overflow".into()))?;
        let expiry = super::jellyfin_play::terminal_expiry(millis)?;
        Some(serde_json::json!({"final_play_id":write.provenance.play_id,"token_digest":write.provenance.scope.token_digest,"device_digest":write.provenance.scope.device_digest,"client_family":write.provenance.scope.client_family,"terminal_expiry":expiry}).to_string())
    } else {
        None
    };
    Ok((expected, context))
}

/// Read admission uses the same durable scope/revision authorities as the write.
/// Eventual commits still repeat these predicates atomically.
pub(crate) const CURRENT: &str = r#"
WITH args AS (SELECT $1,$2,$3,$4,$5,$6,$7,$8)
SELECT EXISTS(SELECT 1 FROM jellyfin_plays p
 JOIN tokens t ON t.token_hash=p.token_digest AND t.user_id=p.user_id
 JOIN jellyfin_login_tokens l ON l.token_hash=t.token_hash AND l.user_id=p.user_id AND l.device_digest=p.device_digest AND l.client_family=p.client_family
 JOIN jellyfin_entity_ids i ON i.entity_kind='item' AND i.wire_id=p.item_wire_id AND i.native_id=p.item_id AND i.retired=0
 JOIN jellyfin_entity_ids f ON f.entity_kind='file' AND f.wire_id=p.file_wire_id AND f.native_id=p.file_id AND f.retired=0
 JOIN items supported_item ON supported_item.id=p.item_id AND supported_item.kind IN ('movie','episode')
 JOIN libraries supported_library ON supported_library.id=supported_item.library_id AND supported_library.kind IN ('movies','shows')
 JOIN files native_file ON native_file.id=p.file_id AND native_file.item_id=p.item_id AND (json_extract(json_extract(p.payload,'$.selection_json'),'$.source.size') IS NULL OR (native_file.size=json_extract(json_extract(p.payload,'$.selection_json'),'$.source.size') AND native_file.mtime=json_extract(json_extract(p.payload,'$.selection_json'),'$.source.mtime')))
 AND (json_type(json_extract(p.payload,'$.selection_json'),'$.source.probe') IS NULL OR native_file.probe_json IS json_extract(json_extract(p.payload,'$.selection_json'),'$.source.probe'))
 LEFT JOIN watch_state w ON w.user_id=p.user_id AND w.item_id=p.item_id
 WHERE p.play_id=$1 AND p.user_id=$2 AND p.token_digest=$3 AND p.device_digest=$4 AND p.client_family=$5
 AND p.item_id=$7 AND p.state='active' AND (EXISTS(SELECT 1 FROM file_grants g WHERE g.id=p.direct_grant_id AND g.user_id=p.user_id AND g.file_id=p.file_id AND g.source_token_hash=p.token_digest AND g.revoked_at IS NULL AND g.expires_at>$8)
 OR EXISTS(SELECT 1 FROM media_sessions m JOIN media_playback_pointers ptr ON ptr.user_id=m.user_id AND ptr.playback_id=m.playback_id AND ptr.current_incarnation_id=m.incarnation_id WHERE m.incarnation_id=p.native_incarnation_id AND m.user_id=p.user_id AND m.playback_id=p.playback_id AND m.state='active' AND m.publication_ready_at_ms=0 AND m.lease_expires_at_ms>$8*1000 AND m.request_fingerprint=json_extract(p.payload,'$.native_request_fingerprint') AND m.media_origin_ms=json_extract(p.payload,'$.source_origin_ms'))) AND p.manual_revision=$6 AND COALESCE(w.manual_revision,0)=$6) AS current
"#;
#[cfg(feature = "hiqlite-store")]
pub(crate) struct CurrentRow {
    pub current: bool,
}
#[cfg(feature = "hiqlite-store")]
impl From<&mut hiqlite::Row<'_>> for CurrentRow {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self {
            current: row.get("current"),
        }
    }
}

#[cfg(all(test, feature = "hiqlite-store"))]
mod tests {
    #[test]
    fn jellyfin_watch_generated_manual_sql_keeps_all_four_bindings_in_order() {
        for tree in [false, true] {
            for watched in [false, true] {
                super::super::hiqlite::validate_sql(&super::manual_sql(tree, watched))
                    .expect("shared manual statement");
            }
        }
        super::super::hiqlite::validate_sql(super::PROGRESS).expect("commit-time fence statement");
        super::super::hiqlite::validate_sql(super::CURRENT).expect("read admission statement");
    }
}
