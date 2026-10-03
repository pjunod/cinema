//! Permanent protocol identities survive native row-ID reuse.
use crate::error::StoreError;
use async_trait::async_trait;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JellyfinEntityKind {
    User,
    Library,
    Item,
    File,
}
impl JellyfinEntityKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Library => "library",
            Self::Item => "item",
            Self::File => "file",
        }
    }
    pub(crate) fn table(self) -> &'static str {
        match self {
            Self::User => "users",
            Self::Library => "libraries",
            Self::Item => "items",
            Self::File => "files",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JellyfinEntityId {
    pub wire_id: String,
    pub native_id: i64,
}

#[async_trait]
pub trait JellyfinIdentityStore: Send + Sync {
    /// Return live IDs for existing entities, allocating only missing mappings.
    /// Missing native entities are omitted. At most 1,000 IDs per call.
    async fn jellyfin_entity_ids(
        &self,
        kind: JellyfinEntityKind,
        native_ids: &[i64],
    ) -> Result<Vec<JellyfinEntityId>, StoreError>;
    /// Retired IDs never resolve, even if a native integer is reused.
    async fn jellyfin_resolve_entity(
        &self,
        kind: JellyfinEntityKind,
        wire_id: &str,
    ) -> Result<Option<i64>, StoreError>;
}

pub(crate) fn checked_ids(ids: &[i64]) -> Result<Vec<i64>, StoreError> {
    if ids.len() > 1000 || ids.iter().any(|id| *id <= 0) {
        return Err(StoreError::Identity(
            "invalid Jellyfin identity batch".into(),
        ));
    }
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}
// Only enum-selected table names and generated placeholders enter SQL text.
pub(crate) fn allocation_sql(kind: JellyfinEntityKind) -> String {
    format!("INSERT INTO jellyfin_entity_ids (wire_id,entity_kind,native_id,incarnation) SELECT $1,$2,id,$3 FROM {} WHERE id=$4 ON CONFLICT DO NOTHING", kind.table())
}
pub(crate) fn selection_sql(kind: JellyfinEntityKind, ids: &[i64]) -> String {
    let placeholders = (1..=ids.len())
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("SELECT wire_id,native_id FROM jellyfin_entity_ids WHERE entity_kind='{}' AND retired=0 AND native_id IN ({placeholders}) ORDER BY native_id", kind.name())
}

pub(crate) const JELLYFIN_IDENTITY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS jellyfin_entity_ids (
 wire_id TEXT PRIMARY KEY,
 entity_kind TEXT NOT NULL CHECK(entity_kind IN ('user','library','item','file')),
 native_id INTEGER NOT NULL CHECK(native_id>0),
 incarnation TEXT NOT NULL,
 retired INTEGER NOT NULL DEFAULT 0 CHECK(retired IN (0,1)),
 UNIQUE(entity_kind,native_id,incarnation)
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS jellyfin_entity_live ON jellyfin_entity_ids(entity_kind,native_id) WHERE retired=0;
CREATE TRIGGER IF NOT EXISTS jellyfin_retire_user AFTER DELETE ON users BEGIN
 UPDATE jellyfin_entity_ids SET retired=1 WHERE entity_kind='user' AND native_id=OLD.id AND retired=0;
END;
CREATE TRIGGER IF NOT EXISTS jellyfin_retire_library AFTER DELETE ON libraries BEGIN
 UPDATE jellyfin_entity_ids SET retired=1 WHERE entity_kind='library' AND native_id=OLD.id AND retired=0;
END;
CREATE TRIGGER IF NOT EXISTS jellyfin_retire_item AFTER DELETE ON items BEGIN
 UPDATE jellyfin_entity_ids SET retired=1 WHERE entity_kind='item' AND native_id=OLD.id AND retired=0;
END;
CREATE TRIGGER IF NOT EXISTS jellyfin_retire_file AFTER DELETE ON files BEGIN
 UPDATE jellyfin_entity_ids SET retired=1 WHERE entity_kind='file' AND native_id=OLD.id AND retired=0;
END;
"#;
