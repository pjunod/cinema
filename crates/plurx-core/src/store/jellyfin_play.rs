//! Compatibility negotiation identities reference native ownership; they do
//! not renew routes or grant producer authority.
use super::JellyfinClientFamily;
use crate::error::StoreError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub const JELLYFIN_PENDING_PLAY_TTL_MS: i64 = 600_000;
pub const JELLYFIN_TERMINAL_PLAY_TTL_MS: i64 = 86_400_000;
pub const JELLYFIN_PENDING_PLAYS_PER_LOGIN: usize = 64;
pub const JELLYFIN_PENDING_PLAYS_SERVER: usize = 4096;

#[derive(Clone, Serialize, Deserialize)]
pub struct JellyfinPlayScope {
    pub user_id: i64,
    pub token_digest: String,
    pub device_digest: String,
    pub client_family: JellyfinClientFamily,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct NewJellyfinPlay {
    pub play_id: String,
    pub scope: JellyfinPlayScope,
    pub playback_id: String,
    pub item_id: i64,
    pub file_id: i64,
    pub item_wire_id: String,
    pub file_wire_id: String,
    pub source_fingerprint: String,
    pub profile_fingerprint: String,
    pub native_request_fingerprint: String,
    pub selection_json: String,
    pub source_origin_ms: i64,
    pub created_at_ms: i64,
}
#[derive(Clone)]
pub struct JellyfinPlay {
    pub negotiation: NewJellyfinPlay,
    pub state: String,
    pub expires_at_ms: i64,
    pub manual_revision: i64,
    pub native_incarnation_id: Option<String>,
    pub direct_grant_id: Option<String>,
}
/// An exact native reference, never a client-provided session capability.
pub enum JellyfinPlayActivation {
    MediaIncarnation(String),
    DirectGrant(String),
}
#[async_trait]
pub trait JellyfinPlayStore: Send + Sync {
    /// Admission and expired pending/terminal cleanup are one transaction.
    /// Active rows are never evicted to make room for negotiations.
    async fn create_jellyfin_play(&self, play: NewJellyfinPlay) -> Result<bool, StoreError>;
    /// Terminalize only this authenticated login's plays, returning references
    /// for the coordinator to release through their native resource owners.
    async fn end_jellyfin_login_plays(
        &self,
        scope: &JellyfinPlayScope,
        now_ms: i64,
    ) -> Result<Vec<JellyfinPlay>, StoreError>;

    async fn jellyfin_play(
        &self,
        play_id: &str,
        scope: &JellyfinPlayScope,
    ) -> Result<Option<JellyfinPlay>, StoreError>;
    async fn activate_jellyfin_play(
        &self,
        play_id: &str,
        scope: &JellyfinPlayScope,
        activation: JellyfinPlayActivation,
        now_ms: i64,
    ) -> Result<bool, StoreError>;
    /// Terminalizes only this UUID, retaining its exact native reference.
    /// Native resource release is still the coordinator's operation.
    async fn end_jellyfin_play(
        &self,
        play_id: &str,
        scope: &JellyfinPlayScope,
        now_ms: i64,
    ) -> Result<bool, StoreError>;
}
fn invalid() -> StoreError {
    StoreError::Identity("invalid compatibility play binding".into())
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn wire(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && uuid::Uuid::parse_str(s).is_ok_and(|id| !id.is_nil())
}
pub(crate) fn validate_scope(scope: &JellyfinPlayScope) -> Result<(), StoreError> {
    if scope.user_id <= 0 || !digest(&scope.token_digest) || !digest(&scope.device_digest) {
        return Err(invalid());
    }
    Ok(())
}
pub(crate) fn validate_key(play_id: &str, scope: &JellyfinPlayScope) -> Result<(), StoreError> {
    validate_scope(scope)?;
    if !wire(play_id) {
        return Err(invalid());
    }
    Ok(())
}
pub(crate) fn encode(play: &NewJellyfinPlay) -> Result<(String, i64), StoreError> {
    validate_key(&play.play_id, &play.scope)?;
    if play.item_id <= 0
        || play.file_id <= 0
        || !wire(&play.item_wire_id)
        || !wire(&play.file_wire_id)
        || play.playback_id.is_empty()
        || play.playback_id.len() > 128
        || play.playback_id.chars().any(char::is_control)
        || !digest(&play.source_fingerprint)
        || !digest(&play.profile_fingerprint)
        || !digest(&play.native_request_fingerprint)
        || play.selection_json.len() > 8192
        || play.source_origin_ms < 0
        || play.created_at_ms < 0
    {
        return Err(invalid());
    }
    let selection: serde_json::Value =
        serde_json::from_str(&play.selection_json).map_err(|_| invalid())?;
    if !selection.is_object() {
        return Err(invalid());
    }
    let expires = play
        .created_at_ms
        .checked_add(JELLYFIN_PENDING_PLAY_TTL_MS)
        .ok_or_else(invalid)?;
    let json = serde_json::to_string(play).map_err(|_| invalid())?;
    Ok((json, expires))
}
pub(crate) fn terminal_expiry(now_ms: i64) -> Result<i64, StoreError> {
    if now_ms < 0 {
        return Err(invalid());
    }
    now_ms
        .checked_add(JELLYFIN_TERMINAL_PLAY_TTL_MS)
        .ok_or_else(invalid)
}
pub(crate) fn decode(
    payload: &str,
    state: String,
    expires_at_ms: i64,
    manual_revision: i64,
    native_incarnation_id: Option<String>,
    direct_grant_id: Option<String>,
) -> Result<JellyfinPlay, StoreError> {
    Ok(JellyfinPlay {
        negotiation: serde_json::from_str(payload).map_err(|_| invalid())?,
        state,
        expires_at_ms,
        manual_revision,
        native_incarnation_id,
        direct_grant_id,
    })
}
pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS jellyfin_plays (
 play_id TEXT PRIMARY KEY,
 user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 token_digest TEXT NOT NULL,
 device_digest TEXT NOT NULL,
 client_family TEXT NOT NULL CHECK(client_family IN ('infuse','android_tv')),
 playback_id TEXT NOT NULL,
 item_id INTEGER NOT NULL,
 file_id INTEGER NOT NULL,
 item_wire_id TEXT NOT NULL REFERENCES jellyfin_entity_ids(wire_id),
 file_wire_id TEXT NOT NULL REFERENCES jellyfin_entity_ids(wire_id),
 payload TEXT NOT NULL CHECK(json_valid(payload)),
 state TEXT NOT NULL CHECK(state IN ('pending','active','ended')),
 expires_at_ms INTEGER NOT NULL,
 manual_revision INTEGER NOT NULL DEFAULT 0,
 native_incarnation_id TEXT,
 direct_grant_id TEXT,
 CHECK(native_incarnation_id IS NULL OR direct_grant_id IS NULL)
) STRICT;
CREATE INDEX IF NOT EXISTS jellyfin_plays_scope ON jellyfin_plays(user_id,token_digest,device_digest,client_family,state);
CREATE INDEX IF NOT EXISTS jellyfin_plays_expiry ON jellyfin_plays(state,expires_at_ms);
CREATE UNIQUE INDEX IF NOT EXISTS jellyfin_plays_native_reference ON jellyfin_plays(native_incarnation_id) WHERE native_incarnation_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS jellyfin_plays_direct_reference ON jellyfin_plays(direct_grant_id) WHERE direct_grant_id IS NOT NULL;
"#;
pub(crate) const CLEANUP: &str =
    "DELETE FROM jellyfin_plays WHERE state IN ('pending','ended') AND expires_at_ms <= $1";
pub(crate) const CREATE: &str = r#"
INSERT INTO jellyfin_plays(play_id,user_id,token_digest,device_digest,client_family,playback_id,item_id,file_id,payload,expires_at_ms,item_wire_id,file_wire_id,state,manual_revision)
SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,'pending',COALESCE((SELECT manual_revision FROM watch_state WHERE user_id=$2 AND item_id=$7),0)
FROM files f JOIN jellyfin_login_tokens l ON l.user_id=$2 AND l.token_hash=$3 AND l.device_digest=$4 AND l.client_family=$5
JOIN jellyfin_entity_ids i ON i.wire_id=$11 AND i.entity_kind='item' AND i.native_id=$7 AND i.retired=0
JOIN jellyfin_entity_ids s ON s.wire_id=$12 AND s.entity_kind='file' AND s.native_id=$8 AND s.retired=0
WHERE f.id=$8 AND f.item_id=$7
AND (SELECT COUNT(*) FROM jellyfin_plays WHERE state='pending' AND user_id=$2 AND token_digest=$3 AND device_digest=$4 AND client_family=$5)<64
AND (SELECT COUNT(*) FROM jellyfin_plays WHERE state='pending')<4096
ON CONFLICT(play_id) DO NOTHING
"#;
pub(crate) const READ: &str = "SELECT payload,state,expires_at_ms,manual_revision,native_incarnation_id,direct_grant_id FROM jellyfin_plays WHERE play_id=$1 AND user_id=$2 AND token_digest=$3 AND device_digest=$4 AND client_family=$5";
pub(crate) const ACTIVATE_MEDIA: &str = r#"
UPDATE jellyfin_plays SET native_incarnation_id=$1,state='active',expires_at_ms=$2
WHERE play_id=$3 AND user_id=$4 AND token_digest=$5 AND device_digest=$6 AND client_family=$7 AND state='pending' AND expires_at_ms>$8
AND EXISTS(SELECT 1 FROM tokens WHERE token_hash=$5 AND user_id=$4)
AND EXISTS(SELECT 1 FROM jellyfin_entity_ids WHERE wire_id=jellyfin_plays.item_wire_id AND retired=0)
AND EXISTS(SELECT 1 FROM jellyfin_entity_ids WHERE wire_id=jellyfin_plays.file_wire_id AND retired=0)
AND EXISTS(SELECT 1 FROM media_sessions m WHERE m.incarnation_id=$1 AND m.user_id=$4 AND m.playback_id=jellyfin_plays.playback_id AND m.state='active' AND m.publication_ready_at_ms=0 AND m.request_fingerprint=json_extract(jellyfin_plays.payload,'$.native_request_fingerprint') AND m.media_origin_ms=json_extract(jellyfin_plays.payload,'$.source_origin_ms'))
"#;
pub(crate) const ACTIVATE_DIRECT: &str = r#"
UPDATE jellyfin_plays SET direct_grant_id=$1,state='active',expires_at_ms=$2
WHERE play_id=$3 AND user_id=$4 AND token_digest=$5 AND device_digest=$6 AND client_family=$7 AND state='pending' AND expires_at_ms>$8
AND EXISTS(SELECT 1 FROM tokens WHERE token_hash=$5 AND user_id=$4)
AND EXISTS(SELECT 1 FROM jellyfin_entity_ids WHERE wire_id=jellyfin_plays.item_wire_id AND retired=0)
AND EXISTS(SELECT 1 FROM jellyfin_entity_ids WHERE wire_id=jellyfin_plays.file_wire_id AND retired=0)
AND EXISTS(SELECT 1 FROM file_grants g WHERE g.id=$1 AND g.user_id=$4 AND g.file_id=jellyfin_plays.file_id AND g.source_token_hash=$5 AND g.revoked_at IS NULL AND g.expires_at>$8/1000)
"#;
pub(crate) const END: &str = "UPDATE jellyfin_plays SET state='ended',expires_at_ms=$1 WHERE play_id=$2 AND user_id=$3 AND token_digest=$4 AND device_digest=$5 AND client_family=$6 AND state IN ('pending','active')";

pub(crate) struct RawPlay {
    pub payload: String,
    pub state: String,
    pub expires_at_ms: i64,
    pub manual_revision: i64,
    pub native_incarnation_id: Option<String>,
    pub direct_grant_id: Option<String>,
}
impl RawPlay {
    pub(crate) fn decode(self) -> Result<JellyfinPlay, StoreError> {
        decode(
            &self.payload,
            self.state,
            self.expires_at_ms,
            self.manual_revision,
            self.native_incarnation_id,
            self.direct_grant_id,
        )
    }
}
pub(crate) fn activation_sql(
    activation: JellyfinPlayActivation,
) -> Result<(&'static str, String), StoreError> {
    let (sql, reference) = match activation {
        JellyfinPlayActivation::MediaIncarnation(id) => (ACTIVATE_MEDIA, id),
        JellyfinPlayActivation::DirectGrant(id) => (ACTIVATE_DIRECT, id),
    };
    if reference.is_empty() || reference.len() > 256 || reference.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok((sql, reference))
}

pub(crate) const END_LOGIN: &str = "UPDATE jellyfin_plays SET state='ended',expires_at_ms=$1 WHERE user_id=$2 AND token_digest=$3 AND device_digest=$4 AND client_family=$5 AND state IN ('pending','active','ended') RETURNING payload,state,expires_at_ms,manual_revision,native_incarnation_id,direct_grant_id";
