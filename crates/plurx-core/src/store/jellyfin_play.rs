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
    /// Direct plays only: the file grant minted with the negotiation. Its
    /// unguessable secret is the play's scoped media link (returned once, as
    /// the source ETag); only the grant reference is stored here, in the
    /// binding's own column rather than the payload.
    #[serde(skip)]
    pub media_grant_id: Option<String>,
    /// The compatibility switch generation this negotiation was admitted
    /// under. Every later transition commits only while that exact generation
    /// is saved and enabled, so turning the switch off ends in-flight plays
    /// even if it is turned on again before they commit.
    #[serde(default)]
    pub switch_generation: String,
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
    /// Resolve the play that owns one direct-play file grant. The scoped media
    /// link presents only the grant secret, so this is how it learns its play,
    /// login and current state; Stop and supersession must be observed exactly.
    async fn jellyfin_play_for_direct_grant(
        &self,
        grant_id: &str,
    ) -> Result<Option<JellyfinPlay>, StoreError>;
    /// The plays one activation of this login's play ended. The coordinator
    /// releases their exact native session or direct grant; a replacement that
    /// never sent Stopped must not leave its predecessor's resources behind.
    async fn jellyfin_plays_superseded_by(
        &self,
        play_id: &str,
        scope: &JellyfinPlayScope,
    ) -> Result<Vec<JellyfinPlay>, StoreError>;
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
pub(crate) fn reference(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
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
        || play
            .media_grant_id
            .as_deref()
            .is_some_and(|id| !reference(id))
        || super::jellyfin_login::validate_generation(&play.switch_generation).is_err()
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
/// An active binding whose native session has been terminal for the whole
/// tombstone window, or whose direct grant expired or was revoked that long
/// ago, has no resource left and no client still reporting through it. End it
/// so terminal retention can delete it; within the window a failed final Stop
/// can still retry through the binding. Runs in the admission transaction, so
/// active metadata is bounded by negotiation rather than by a timer.
pub(crate) const RETIRE_ORPHANED_ACTIVE: &str = r#"
UPDATE jellyfin_plays SET state='ended',expires_at_ms=$1+86400000
WHERE state='active' AND (
 (native_incarnation_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM media_sessions m WHERE m.incarnation_id=jellyfin_plays.native_incarnation_id
   AND (m.state IN ('starting','active') OR m.updated_at_ms>$1-86400000)))
 OR (direct_grant_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM file_grants g WHERE g.id=jellyfin_plays.direct_grant_id
   AND ((g.revoked_at IS NULL AND g.expires_at>$1/1000) OR g.revoked_at>$1/1000-86400))))
"#;
// The transaction assigns private ordering metadata; equal wall-clock times
// cannot make a later pending ask look older. Legacy rows precede new asks.
pub(crate) const CREATE: &str = r#"
INSERT INTO jellyfin_plays(play_id,user_id,token_digest,device_digest,client_family,playback_id,item_id,file_id,payload,expires_at_ms,item_wire_id,file_wire_id,state,manual_revision,direct_grant_id)
SELECT $1,$2,$3,$4,$5,$6,$7,$8,json_set($9,'$.negotiation_order',(SELECT COALESCE(MAX(json_extract(payload,'$.negotiation_order')),0)+1 FROM jellyfin_plays WHERE user_id=$2 AND playback_id=$6)),$10,$11,$12,'pending',COALESCE((SELECT manual_revision FROM watch_state WHERE user_id=$2 AND item_id=$7),0),$13
FROM files f JOIN jellyfin_login_tokens l ON l.user_id=$2 AND l.token_hash=$3 AND l.device_digest=$4 AND l.client_family=$5
JOIN jellyfin_entity_ids i ON i.wire_id=$11 AND i.entity_kind='item' AND i.native_id=$7 AND i.retired=0
JOIN jellyfin_entity_ids s ON s.wire_id=$12 AND s.entity_kind='file' AND s.native_id=$8 AND s.retired=0
WHERE f.id=$8 AND f.item_id=$7
AND COALESCE((SELECT MAX(json_extract(payload,'$.negotiation_order')) FROM jellyfin_plays WHERE user_id=$2 AND playback_id=$6),0)<9223372036854775807
AND (SELECT COUNT(*) FROM jellyfin_plays WHERE state='pending' AND user_id=$2 AND token_digest=$3 AND device_digest=$4 AND client_family=$5)<64
AND (SELECT COUNT(*) FROM jellyfin_plays WHERE state='pending')<4096
AND ($13 IS NULL OR EXISTS(SELECT 1 FROM file_grants g WHERE g.id=$13 AND g.user_id=$2 AND g.file_id=$8 AND g.source_token_hash=$3 AND g.revoked_at IS NULL))
AND lower(trim(COALESCE((SELECT value FROM settings WHERE key='compat.jellyfin.enabled'),''),char(9)||char(10)||char(11)||char(12)||char(13)||' ')) IN ('1','true','yes','on') AND (SELECT value FROM settings WHERE key='compat.jellyfin.generation')=json_extract($9,'$.switch_generation')
ON CONFLICT(play_id) DO NOTHING
"#;
pub(crate) const READ: &str = "SELECT payload,state,expires_at_ms,manual_revision,native_incarnation_id,direct_grant_id FROM jellyfin_plays WHERE play_id=$1 AND user_id=$2 AND token_digest=$3 AND device_digest=$4 AND client_family=$5";
/// The scope columns are returned through the payload, so the caller learns
/// the exact login and device that negotiated this grant.
pub(crate) const READ_BY_DIRECT_GRANT: &str = "SELECT payload,state,expires_at_ms,manual_revision,native_incarnation_id,direct_grant_id FROM jellyfin_plays WHERE direct_grant_id=$1";
/// Bounded by the player's pending and active rows at the moment of one
/// activation; the chosen play must belong to the asking login.
pub(crate) const READ_SUPERSEDED_BY: &str = "SELECT payload,state,expires_at_ms,manual_revision,native_incarnation_id,direct_grant_id FROM jellyfin_plays WHERE user_id=$1 AND state='ended' AND json_extract(payload,'$.superseded_by')=$2
AND EXISTS(SELECT 1 FROM jellyfin_plays chosen WHERE chosen.play_id=$2 AND chosen.user_id=$1 AND chosen.token_digest=$3 AND chosen.device_digest=$4 AND chosen.client_family=$5)";
/// The native pointer transaction fences prior compatibility events before
/// its replacement becomes observable. A negotiation alone never runs this.
/// Pending asks newer than the selected ask are retained.
/// The chosen play is the exact reserved request (`$5`, `jellyfin:<play>`) the
/// native activation carries, never whichever pending ask happens to share
/// its recipe fingerprint.
pub(crate) const SUPERSEDE_AT_NATIVE_POINTER: &str = r#"
WITH args AS (SELECT $1,$2,$3,$4,$5)
UPDATE jellyfin_plays SET state='ended',expires_at_ms=$4,payload=json_set(payload,'$.superseded_by',substr($5,10))
WHERE user_id=$1 AND playback_id=$2 AND state IN ('pending','active')
AND substr($5,1,9)='jellyfin:' AND play_id!=substr($5,10)
AND EXISTS(SELECT 1 FROM media_playback_pointers p JOIN media_sessions m ON m.incarnation_id=p.current_incarnation_id
 WHERE p.user_id=$1 AND p.playback_id=$2 AND p.current_incarnation_id=$3 AND m.state='active'
 AND EXISTS(SELECT 1 FROM jellyfin_plays chosen WHERE chosen.play_id=substr($5,10) AND chosen.user_id=$1 AND chosen.playback_id=$2
  AND chosen.state='pending' AND json_extract(chosen.payload,'$.native_request_fingerprint')=m.request_fingerprint
  AND (jellyfin_plays.state='active' OR COALESCE(json_extract(jellyfin_plays.payload,'$.negotiation_order'),0)<COALESCE(json_extract(chosen.payload,'$.negotiation_order'),0))))
"#;

/// Direct and media binding activation share the same player supersession
/// boundary. Ended old bindings cannot re-enter this predicate on replay.
pub(crate) const SUPERSEDE_AFTER_BINDING_ACTIVATION: &str = r#"
WITH args AS (SELECT $1,$2,$3,$4,$5,$6,$7)
UPDATE jellyfin_plays SET state='ended',expires_at_ms=$6,payload=json_set(payload,'$.superseded_by',$1)
WHERE state IN ('pending','active') AND play_id!=$1
AND EXISTS(SELECT 1 FROM jellyfin_plays chosen WHERE chosen.play_id=$1
 AND chosen.user_id=$2 AND chosen.token_digest=$3 AND chosen.device_digest=$4 AND chosen.client_family=$5 AND chosen.state='active' AND json_extract(chosen.payload,'$.activation_nonce')=$7
 AND jellyfin_plays.user_id=chosen.user_id AND jellyfin_plays.playback_id=chosen.playback_id
 AND (jellyfin_plays.state='active' OR COALESCE(json_extract(jellyfin_plays.payload,'$.negotiation_order'),0)<COALESCE(json_extract(chosen.payload,'$.negotiation_order'),0)))
"#;

/// Save the cleanup reference before the native create response can escape.
/// Five parameters are user, reserved request, exact incarnation, now and nonce.
/// Replay checks the same live authority but retains the original nonce.
pub(crate) const BIND_NATIVE_PUBLICATION: &str = r#"
WITH args AS (SELECT $1,$2,$3,$4,$5)
UPDATE jellyfin_plays SET native_incarnation_id=$3,state='active',expires_at_ms=9223372036854775807,
 payload=CASE WHEN state='pending' THEN json_set(payload,'$.activation_nonce',$5) ELSE payload END
WHERE user_id=$1 AND play_id=substr($2,10) AND substr($2,1,9)='jellyfin:'
AND ((state='pending' AND expires_at_ms>$4) OR (state='active' AND native_incarnation_id=$3))
AND lower(trim(COALESCE((SELECT value FROM settings WHERE key='compat.jellyfin.enabled'),''),char(9)||char(10)||char(11)||char(12)||char(13)||' ')) IN ('1','true','yes','on') AND (SELECT value FROM settings WHERE key='compat.jellyfin.generation')=json_extract(jellyfin_plays.payload,'$.switch_generation')
AND EXISTS(SELECT 1 FROM jellyfin_login_tokens l JOIN tokens t ON t.token_hash=l.token_hash AND t.user_id=l.user_id
 WHERE l.user_id=jellyfin_plays.user_id AND l.token_hash=jellyfin_plays.token_digest
 AND l.device_digest=jellyfin_plays.device_digest AND l.client_family=jellyfin_plays.client_family)
AND EXISTS(SELECT 1 FROM jellyfin_entity_ids WHERE wire_id=jellyfin_plays.item_wire_id AND retired=0)
AND EXISTS(SELECT 1 FROM jellyfin_entity_ids WHERE wire_id=jellyfin_plays.file_wire_id AND retired=0)
AND EXISTS(SELECT 1 FROM media_sessions m JOIN media_playback_pointers p ON p.current_incarnation_id=m.incarnation_id
 JOIN media_session_requests r ON r.user_id=m.user_id AND r.request_id=$2 AND r.incarnation_id=m.incarnation_id
 WHERE m.user_id=$1 AND m.incarnation_id=$3 AND m.playback_id=jellyfin_plays.playback_id
 AND p.user_id=$1 AND p.playback_id=m.playback_id AND m.state='active' AND m.publication_ready_at_ms=0
 AND m.lease_expires_at_ms>$4 AND r.state IN ('starting','resolved')
 AND r.playback_id=m.playback_id AND r.request_fingerprint=m.request_fingerprint AND r.owner_node_id=m.owner_node_id
 AND m.request_fingerprint=json_extract(jellyfin_plays.payload,'$.native_request_fingerprint')
 AND m.media_origin_ms=json_extract(jellyfin_plays.payload,'$.source_origin_ms'))
"#;
pub(crate) const SUPERSEDE_NATIVE_PUBLICATION: &str = r#"
WITH args AS (SELECT $1,$2,$3,$4,$5)
UPDATE jellyfin_plays SET state='ended',expires_at_ms=$4+86400000,payload=json_set(payload,'$.superseded_by',substr($2,10))
WHERE user_id=$1 AND state IN ('pending','active') AND 'jellyfin:'||play_id!=$2
AND EXISTS(SELECT 1 FROM jellyfin_plays chosen WHERE chosen.user_id=$1 AND chosen.play_id=substr($2,10) AND substr($2,1,9)='jellyfin:'
 AND chosen.state='active' AND chosen.native_incarnation_id=$3 AND json_extract(chosen.payload,'$.activation_nonce')=$5
 AND chosen.playback_id=jellyfin_plays.playback_id
 AND (jellyfin_plays.state='active' OR COALESCE(json_extract(jellyfin_plays.payload,'$.negotiation_order'),0)<COALESCE(json_extract(chosen.payload,'$.negotiation_order'),0)))
"#;

pub(crate) const ACTIVATE_MEDIA: &str = r#"
WITH args AS (SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9)
UPDATE jellyfin_plays SET native_incarnation_id=$1,state='active',expires_at_ms=$2,payload=json_set(payload,'$.activation_nonce',$9)
WHERE play_id=$3 AND user_id=$4 AND token_digest=$5 AND device_digest=$6 AND client_family=$7 AND state='pending' AND expires_at_ms>$8
AND lower(trim(COALESCE((SELECT value FROM settings WHERE key='compat.jellyfin.enabled'),''),char(9)||char(10)||char(11)||char(12)||char(13)||' ')) IN ('1','true','yes','on') AND (SELECT value FROM settings WHERE key='compat.jellyfin.generation')=json_extract(jellyfin_plays.payload,'$.switch_generation')
AND EXISTS(SELECT 1 FROM tokens WHERE token_hash=$5 AND user_id=$4)
AND EXISTS(SELECT 1 FROM jellyfin_entity_ids WHERE wire_id=jellyfin_plays.item_wire_id AND retired=0)
AND EXISTS(SELECT 1 FROM jellyfin_entity_ids WHERE wire_id=jellyfin_plays.file_wire_id AND retired=0)
AND EXISTS(SELECT 1 FROM media_sessions m WHERE m.incarnation_id=$1 AND m.user_id=$4 AND m.playback_id=jellyfin_plays.playback_id AND m.state='active' AND m.publication_ready_at_ms=0 AND m.request_fingerprint=json_extract(jellyfin_plays.payload,'$.native_request_fingerprint') AND m.media_origin_ms=json_extract(jellyfin_plays.payload,'$.source_origin_ms'))
"#;
pub(crate) const ACTIVATE_DIRECT: &str = r#"
WITH args AS (SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9)
UPDATE jellyfin_plays SET direct_grant_id=$1,state='active',expires_at_ms=$2,payload=json_set(payload,'$.activation_nonce',$9)
WHERE play_id=$3 AND user_id=$4 AND token_digest=$5 AND device_digest=$6 AND client_family=$7 AND state='pending' AND expires_at_ms>$8
AND (direct_grant_id IS NULL OR direct_grant_id=$1)
AND lower(trim(COALESCE((SELECT value FROM settings WHERE key='compat.jellyfin.enabled'),''),char(9)||char(10)||char(11)||char(12)||char(13)||' ')) IN ('1','true','yes','on') AND (SELECT value FROM settings WHERE key='compat.jellyfin.generation')=json_extract(jellyfin_plays.payload,'$.switch_generation')
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
    if !self::reference(&reference) {
        return Err(invalid());
    }
    Ok((sql, reference))
}

pub(crate) const END_LOGIN: &str = "UPDATE jellyfin_plays SET expires_at_ms=CASE WHEN state='ended' THEN expires_at_ms ELSE $1 END,state='ended' WHERE user_id=$2 AND token_digest=$3 AND device_digest=$4 AND client_family=$5 AND state IN ('pending','active','ended') RETURNING payload,state,expires_at_ms,manual_revision,native_incarnation_id,direct_grant_id";
