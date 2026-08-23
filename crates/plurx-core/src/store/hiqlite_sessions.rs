//! Replicated idempotency, ownership, and routing for live HLS sessions.

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::Row;

use super::hiqlite::{database_error, timeout_store, validate_sql, HiqliteAuthStore};
use super::MediaSessionStore;
use crate::cluster::coordination::removed_job_owner_key;
use crate::domain::{
    MediaSessionActivation, MediaSessionActivationOutcome, MediaSessionRenewal,
    MediaSessionRequestClaim, MediaSessionRoute, MediaSessionTakeover, OwnedMediaSessionLease,
};
use crate::error::StoreError;

const MAX_IN_FLIGHT_PER_USER: i64 = 32;
const MAX_CURRENT_PER_USER: i64 = 64;
const MAX_REQUEST_ROWS_PER_USER: i64 = 4_096;
const MAX_SESSION_ROWS_PER_USER: i64 = 4_096;
const MAX_RENEWALS: usize = 256;
const MAX_TAKEOVER_CANDIDATES: usize = 64;
const MAX_OWNED: i64 = 4_096;
const MAINTENANCE_BATCH: i64 = 256;
const MAX_MEDIA_MILLIS: i64 = 366 * 24 * 60 * 60 * 1_000;
const TAKEOVER_RECOVERY_MS: i64 = 60 * 1_000;
const FAILED_RETENTION_MS: i64 = 60 * 60 * 1_000;
const RESOLVED_RETENTION_MS: i64 = 24 * 60 * 60 * 1_000;

pub(super) const MEDIA_SESSION_REQUESTS_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS media_session_requests (
    user_id             INTEGER NOT NULL,
    request_id          TEXT NOT NULL CHECK (length(request_id) BETWEEN 1 AND 128),
    request_fingerprint TEXT NOT NULL,
    playback_id         TEXT NOT NULL CHECK (length(playback_id) BETWEEN 1 AND 128),
    state               TEXT NOT NULL CHECK (state IN ('starting', 'resolved', 'failed')),
    claim_expires_at_ms INTEGER NOT NULL,
    incarnation_id      TEXT NOT NULL,
    owner_node_id       TEXT,
    response_json       TEXT,
    updated_at_ms       INTEGER NOT NULL,
    PRIMARY KEY (user_id, request_id)
) STRICT";

pub(super) const MEDIA_SESSION_REQUESTS_EXPIRY_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS media_session_requests_expiry
        ON media_session_requests(state, claim_expires_at_ms)";

pub(super) const MEDIA_PLAYBACK_POINTERS_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS media_playback_pointers (
    user_id                INTEGER NOT NULL,
    playback_id            TEXT NOT NULL CHECK (length(playback_id) BETWEEN 1 AND 128),
    current_incarnation_id TEXT NOT NULL UNIQUE,
    updated_at_ms          INTEGER NOT NULL,
    PRIMARY KEY (user_id, playback_id)
) STRICT";

pub(super) const MEDIA_SESSIONS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS media_sessions (
    incarnation_id                TEXT PRIMARY KEY,
    session_id                    TEXT NOT NULL UNIQUE,
    user_id                       INTEGER NOT NULL,
    playback_id                   TEXT NOT NULL,
    request_fingerprint           TEXT NOT NULL,
    owner_node_id                 TEXT NOT NULL,
    owner_epoch                   INTEGER NOT NULL CHECK (owner_epoch > 0),
    lease_expires_at_ms           INTEGER NOT NULL,
    state                         TEXT NOT NULL CHECK (state IN ('starting', 'active', 'ended')),
    recipe_json                   TEXT NOT NULL,
    response_json                 TEXT NOT NULL,
    produced_playable_through_ms  INTEGER NOT NULL DEFAULT 0,
    fetched_through_ms            INTEGER NOT NULL DEFAULT 0,
    media_origin_ms               INTEGER NOT NULL DEFAULT 0,
    media_sequence                INTEGER NOT NULL DEFAULT 0,
    discontinuity_sequence        INTEGER NOT NULL DEFAULT 0,
    updated_at_ms                 INTEGER NOT NULL
) STRICT";

pub(super) const MEDIA_SESSIONS_OWNER_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS media_sessions_owner
        ON media_sessions(owner_node_id, state, lease_expires_at_ms)";
pub(super) const MEDIA_SESSIONS_USER_INDEX: &str = "CREATE INDEX IF NOT EXISTS media_sessions_user
        ON media_sessions(user_id, state, lease_expires_at_ms)";
pub(super) const MEDIA_SESSIONS_EXPIRY_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS media_sessions_expiry
        ON media_sessions(state, lease_expires_at_ms, incarnation_id)";
pub(super) const MEDIA_SESSIONS_RETENTION_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS media_sessions_retention
        ON media_sessions(state, updated_at_ms, incarnation_id)";

pub(super) async fn install_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    for sql in [
        MEDIA_SESSION_REQUESTS_SCHEMA,
        MEDIA_SESSION_REQUESTS_EXPIRY_INDEX,
        MEDIA_PLAYBACK_POINTERS_SCHEMA,
        MEDIA_SESSIONS_SCHEMA,
        MEDIA_SESSIONS_OWNER_INDEX,
        MEDIA_SESSIONS_USER_INDEX,
        MEDIA_SESSIONS_EXPIRY_INDEX,
        MEDIA_SESSIONS_RETENTION_INDEX,
    ] {
        validate_sql(sql)?;
        for result in timeout_store(client.batch(sql)).await? {
            result.map_err(database_error)?;
        }
    }
    Ok(())
}

const ROUTE_COLS: &str = "incarnation_id, session_id, user_id, playback_id,
    request_fingerprint, owner_node_id, owner_epoch, lease_expires_at_ms, state,
    recipe_json, response_json, produced_playable_through_ms, fetched_through_ms,
    media_origin_ms, media_sequence, discontinuity_sequence, updated_at_ms";

struct RouteRow(MediaSessionRoute);

impl From<&mut Row<'_>> for RouteRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(MediaSessionRoute {
            incarnation_id: row.get("incarnation_id"),
            session_id: row.get("session_id"),
            user_id: row.get("user_id"),
            playback_id: row.get("playback_id"),
            request_fingerprint: row.get("request_fingerprint"),
            owner_node_id: row.get("owner_node_id"),
            owner_epoch: row.get("owner_epoch"),
            lease_expires_at_ms: row.get("lease_expires_at_ms"),
            state: row.get("state"),
            recipe_json: row.get("recipe_json"),
            response_json: row.get("response_json"),
            produced_playable_through_ms: row.get("produced_playable_through_ms"),
            fetched_through_ms: row.get("fetched_through_ms"),
            media_origin_ms: row.get("media_origin_ms"),
            media_sequence: row.get("media_sequence"),
            discontinuity_sequence: row.get("discontinuity_sequence"),
            updated_at_ms: row.get("updated_at_ms"),
        })
    }
}

struct PointerRow(String);

impl From<&mut Row<'_>> for PointerRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("current_incarnation_id"))
    }
}

struct PendingMaintenanceRow(i64);

impl From<&mut Row<'_>> for PendingMaintenanceRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("pending"))
    }
}

struct OwnedLeaseRow(OwnedMediaSessionLease);

impl From<&mut Row<'_>> for OwnedLeaseRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(OwnedMediaSessionLease {
            incarnation_id: row.get("incarnation_id"),
            session_id: row.get("session_id"),
            owner_epoch: row.get("owner_epoch"),
            lease_expires_at_ms: row.get("lease_expires_at_ms"),
        })
    }
}

struct RequestRow {
    request_fingerprint: String,
    playback_id: String,
    state: String,
    incarnation_id: String,
    owner_node_id: Option<String>,
    claim_expires_at_ms: i64,
}

impl From<&mut Row<'_>> for RequestRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            request_fingerprint: row.get("request_fingerprint"),
            playback_id: row.get("playback_id"),
            state: row.get("state"),
            incarnation_id: row.get("incarnation_id"),
            owner_node_id: row.get("owner_node_id"),
            claim_expires_at_ms: row.get("claim_expires_at_ms"),
        }
    }
}

fn valid_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok()
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_claim(
    user_id: i64,
    request_id: &str,
    fingerprint: &str,
    playback_id: &str,
    incarnation_id: &str,
    now_ms: i64,
    expires_at_ms: i64,
) -> Result<(), StoreError> {
    if user_id <= 0
        || request_id.is_empty()
        || request_id.len() > 128
        || !valid_fingerprint(fingerprint)
        || playback_id.is_empty()
        || playback_id.len() > 128
        || playback_id
            .bytes()
            .any(|byte| matches!(byte, b'\r' | b'\n' | 0))
        || !valid_uuid(incarnation_id)
        || expires_at_ms <= now_ms
    {
        return Err(StoreError::Task(
            "invalid media-session request claim".to_owned(),
        ));
    }
    Ok(())
}

fn validate_activation(activation: &MediaSessionActivation) -> Result<(), StoreError> {
    let valid = valid_uuid(&activation.incarnation_id)
        && valid_uuid(&activation.session_id)
        && activation.user_id > 0
        && !activation.playback_id.is_empty()
        && activation.playback_id.len() <= 128
        && activation
            .expected_predecessor_incarnation_id
            .as_ref()
            .is_none_or(|value| valid_uuid(value))
        && (activation.fence_predecessor
            || activation.expected_predecessor_incarnation_id.is_none())
        && activation
            .request_id
            .as_ref()
            .is_none_or(|value| !value.is_empty() && value.len() <= 128)
        && valid_fingerprint(&activation.request_fingerprint)
        && !activation.owner_node_id.is_empty()
        && activation.owner_node_id.len() <= 256
        && activation.recipe_json.len() <= 32 * 1024
        && activation.response_json.len() <= 64 * 1024
        && (0..=MAX_MEDIA_MILLIS).contains(&activation.media_origin_ms)
        && activation.lease_expires_at_ms > activation.now_ms;
    valid
        .then_some(())
        .ok_or_else(|| StoreError::Task("invalid media-session activation".to_owned()))
}

fn valid_renewal(renewal: &MediaSessionRenewal) -> bool {
    valid_uuid(&renewal.incarnation_id)
        && renewal.owner_epoch > 0
        && (0..=MAX_MEDIA_MILLIS).contains(&renewal.produced_playable_through_ms)
        && (0..=renewal.produced_playable_through_ms).contains(&renewal.fetched_through_ms)
        && renewal.media_sequence >= 0
}

fn validate_takeover(takeover: &MediaSessionTakeover) -> Result<(), StoreError> {
    let valid = valid_uuid(&takeover.incarnation_id)
        && !takeover.expected_owner_node_id.is_empty()
        && takeover.expected_owner_node_id.len() <= 256
        && !takeover.next_owner_node_id.is_empty()
        && takeover.next_owner_node_id.len() <= 256
        && takeover.expected_owner_node_id != takeover.next_owner_node_id
        && takeover.expected_owner_epoch > 0
        && takeover.expected_owner_epoch < i64::MAX
        && takeover.lease_expires_at_ms > takeover.now_ms;
    if valid {
        Ok(())
    } else {
        Err(StoreError::Task(
            "invalid media-session takeover".to_owned(),
        ))
    }
}

async fn route_by(
    store: &HiqliteAuthStore,
    column: &str,
    value: &str,
) -> Result<Option<MediaSessionRoute>, StoreError> {
    let sql = format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE {column} = $1");
    validate_sql(&sql)?;
    Ok(store
        .client()
        .query_consistent_map::<RouteRow, _>(sql, params!(value))
        .await?
        .into_iter()
        .next()
        .map(|row| row.0))
}

async fn request_row(
    store: &HiqliteAuthStore,
    user_id: i64,
    request_id: &str,
) -> Result<Option<RequestRow>, StoreError> {
    Ok(store
        .client()
        .query_consistent_map::<RequestRow, _>(
            "SELECT request_fingerprint, playback_id, state, incarnation_id, owner_node_id,
                    claim_expires_at_ms
               FROM media_session_requests WHERE user_id = $1 AND request_id = $2",
            params!(user_id, request_id),
        )
        .await?
        .into_iter()
        .next())
}

async fn claim_from_row(
    store: &HiqliteAuthStore,
    row: RequestRow,
    fingerprint: &str,
    playback_id: &str,
) -> Result<MediaSessionRequestClaim, StoreError> {
    if row.request_fingerprint != fingerprint || row.playback_id != playback_id {
        return Ok(MediaSessionRequestClaim::Conflict);
    }
    if row.state == "resolved" {
        if let Some(route) = route_by(store, "incarnation_id", &row.incarnation_id).await? {
            return Ok(MediaSessionRequestClaim::Resolved(Box::new(route)));
        }
    }
    if row.state == "starting" || row.state == "resolved" {
        return Ok(MediaSessionRequestClaim::InFlight {
            incarnation_id: row.incarnation_id,
            owner_node_id: row.owner_node_id,
            claim_expires_at_ms: row.claim_expires_at_ms,
        });
    }
    Ok(MediaSessionRequestClaim::Conflict)
}

#[allow(clippy::too_many_arguments)]
async fn claim_existing_or_reacquire(
    store: &HiqliteAuthStore,
    row: RequestRow,
    user_id: i64,
    request_id: &str,
    fingerprint: &str,
    playback_id: &str,
    incarnation_id: &str,
    now_ms: i64,
    claim_expires_at_ms: i64,
) -> Result<MediaSessionRequestClaim, StoreError> {
    if row.request_fingerprint != fingerprint || row.playback_id != playback_id {
        return Ok(MediaSessionRequestClaim::Conflict);
    }
    if row.state == "failed" || (row.state == "starting" && row.claim_expires_at_ms <= now_ms) {
        let reacquired = store
            .client()
            .execute(
                "UPDATE media_session_requests
                    SET state = 'starting', claim_expires_at_ms = $1,
                        incarnation_id = $2, owner_node_id = NULL,
                        response_json = NULL, updated_at_ms = $3
                  WHERE user_id = $4 AND request_id = $5
                    AND (state = 'failed'
                      OR (state = 'starting' AND claim_expires_at_ms <= $3))
                    AND request_fingerprint = $6 AND playback_id = $7
                    AND (SELECT COUNT(*) FROM media_session_requests
                          WHERE user_id = $4 AND state = 'starting'
                            AND claim_expires_at_ms > $3) < $8
                    AND (SELECT COUNT(*) FROM media_sessions
                          WHERE user_id = $4 AND state IN ('starting', 'active')
                            AND lease_expires_at_ms > $3
                            AND incarnation_id != COALESCE((
                              SELECT current_incarnation_id FROM media_playback_pointers
                               WHERE user_id = $4 AND playback_id = $7), '')) < $9
                    AND (SELECT COUNT(*) FROM media_sessions
                          WHERE user_id = $4) < $10",
                params!(
                    claim_expires_at_ms,
                    incarnation_id,
                    now_ms,
                    user_id,
                    request_id,
                    fingerprint,
                    playback_id,
                    MAX_IN_FLIGHT_PER_USER,
                    MAX_CURRENT_PER_USER,
                    MAX_SESSION_ROWS_PER_USER
                ),
            )
            .await?
            == 1;
        if reacquired {
            return Ok(MediaSessionRequestClaim::Acquired {
                incarnation_id: incarnation_id.to_owned(),
            });
        }
        return match request_row(store, user_id, request_id).await? {
            Some(current)
                if (current.state == "failed"
                    || (current.state == "starting" && current.claim_expires_at_ms <= now_ms))
                    && current.request_fingerprint == fingerprint
                    && current.playback_id == playback_id =>
            {
                Ok(MediaSessionRequestClaim::Overloaded)
            }
            Some(current) => claim_from_row(store, current, fingerprint, playback_id).await,
            None => Ok(MediaSessionRequestClaim::Overloaded),
        };
    }
    claim_from_row(store, row, fingerprint, playback_id).await
}

#[async_trait]
impl MediaSessionStore for HiqliteAuthStore {
    async fn claim_media_session_request(
        &self,
        user_id: i64,
        request_id: &str,
        request_fingerprint: &str,
        playback_id: &str,
        incarnation_id: &str,
        now_ms: i64,
        claim_expires_at_ms: i64,
    ) -> Result<MediaSessionRequestClaim, StoreError> {
        validate_claim(
            user_id,
            request_id,
            request_fingerprint,
            playback_id,
            incarnation_id,
            now_ms,
            claim_expires_at_ms,
        )?;
        let fingerprint = request_fingerprint.to_ascii_lowercase();
        self.maintain_media_sessions(now_ms).await?;
        if let Some(row) = request_row(self, user_id, request_id).await? {
            return claim_existing_or_reacquire(
                self,
                row,
                user_id,
                request_id,
                &fingerprint,
                playback_id,
                incarnation_id,
                now_ms,
                claim_expires_at_ms,
            )
            .await;
        }
        let inserted = self
            .client()
            .execute(
                "INSERT INTO media_session_requests
                    (user_id, request_id, request_fingerprint, playback_id, state,
                     claim_expires_at_ms, incarnation_id, owner_node_id, response_json,
                     updated_at_ms)
                 SELECT $1, $2, $3, $4, 'starting', $5, $6, NULL, NULL, $7
                  WHERE (SELECT COUNT(*) FROM media_session_requests
                          WHERE user_id = $1 AND state = 'starting'
                            AND claim_expires_at_ms > $7) < $8
                    AND (SELECT COUNT(*) FROM media_sessions
                          WHERE user_id = $1 AND state IN ('starting', 'active')
                            AND lease_expires_at_ms > $7
                            AND incarnation_id != COALESCE((
                              SELECT current_incarnation_id FROM media_playback_pointers
                               WHERE user_id = $1 AND playback_id = $4), '')) < $9
                    AND (SELECT COUNT(*) FROM media_session_requests
                          WHERE user_id = $1) < $10
                    AND (SELECT COUNT(*) FROM media_sessions
                          WHERE user_id = $1) < $11
                 ON CONFLICT(user_id, request_id) DO NOTHING",
                params!(
                    user_id,
                    request_id,
                    fingerprint.as_str(),
                    playback_id,
                    claim_expires_at_ms,
                    incarnation_id,
                    now_ms,
                    MAX_IN_FLIGHT_PER_USER,
                    MAX_CURRENT_PER_USER,
                    MAX_REQUEST_ROWS_PER_USER,
                    MAX_SESSION_ROWS_PER_USER
                ),
            )
            .await?;
        if inserted == 1 {
            return Ok(MediaSessionRequestClaim::Acquired {
                incarnation_id: incarnation_id.to_owned(),
            });
        }
        if let Some(row) = request_row(self, user_id, request_id).await? {
            return claim_existing_or_reacquire(
                self,
                row,
                user_id,
                request_id,
                &fingerprint,
                playback_id,
                incarnation_id,
                now_ms,
                claim_expires_at_ms,
            )
            .await;
        }
        Ok(MediaSessionRequestClaim::Overloaded)
    }

    async fn assign_media_session_request_owner(
        &self,
        user_id: i64,
        request_id: &str,
        incarnation_id: &str,
        owner_node_id: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        if user_id <= 0
            || request_id.is_empty()
            || request_id.len() > 128
            || !valid_uuid(incarnation_id)
            || owner_node_id.is_empty()
            || owner_node_id.len() > 256
        {
            return Err(StoreError::Task(
                "invalid media-session owner assignment".to_owned(),
            ));
        }
        Ok(self
            .client()
            .execute(
                "UPDATE media_session_requests SET owner_node_id = $1, updated_at_ms = $2
                  WHERE user_id = $3 AND request_id = $4 AND incarnation_id = $5
                    AND state = 'starting' AND claim_expires_at_ms > $2
                    AND (owner_node_id IS NULL OR owner_node_id = $1)",
                params!(owner_node_id, now_ms, user_id, request_id, incarnation_id),
            )
            .await?
            == 1)
    }

    async fn activate_media_session(
        &self,
        activation: &MediaSessionActivation,
    ) -> Result<Option<MediaSessionActivationOutcome>, StoreError> {
        validate_activation(activation)?;
        let current_pointer = self
            .client()
            .query_consistent_map::<PointerRow, _>(
                "SELECT current_incarnation_id FROM media_playback_pointers
                  WHERE user_id = $1 AND playback_id = $2",
                params!(activation.user_id, activation.playback_id.as_str()),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0);
        if activation.fence_predecessor
            && current_pointer.as_deref()
                != activation.expected_predecessor_incarnation_id.as_deref()
        {
            return Ok(None);
        }
        let predecessor = match current_pointer
            .as_deref()
            .filter(|incarnation| *incarnation != activation.incarnation_id)
        {
            Some(incarnation) => route_by(self, "incarnation_id", incarnation).await?,
            None => None,
        };
        let request_id = activation.request_id.as_deref().unwrap_or("");
        let predecessor_incarnation = activation
            .expected_predecessor_incarnation_id
            .as_deref()
            .or(current_pointer.as_deref())
            .unwrap_or("");
        let lease_resource = format!("session:{}", activation.incarnation_id);
        let removed_owner_key = removed_job_owner_key(&activation.owner_node_id);
        let statements = vec![
            (
                "INSERT INTO job_leases
                    (resource, owner_node_id, fence, revision, expires_at_ms, updated_at_ms)
                 SELECT $1, $2, 1, 1, $3, $4
                  WHERE NOT EXISTS (SELECT 1 FROM settings WHERE key = $5)
                 ON CONFLICT(resource) DO UPDATE SET
                    expires_at_ms = excluded.expires_at_ms,
                    revision = job_leases.revision + 1,
                    updated_at_ms = excluded.updated_at_ms
                 WHERE job_leases.owner_node_id = excluded.owner_node_id
                   AND job_leases.fence = 1 AND job_leases.expires_at_ms > $4
                   AND job_leases.revision < 9223372036854775807
                   AND NOT EXISTS (SELECT 1 FROM settings WHERE key = $5)",
                params!(
                    lease_resource.as_str(),
                    activation.owner_node_id.as_str(),
                    activation.lease_expires_at_ms,
                    activation.now_ms,
                    removed_owner_key.as_str()
                ),
            ),
            (
                "INSERT INTO media_sessions
                    (incarnation_id, session_id, user_id, playback_id, request_fingerprint,
                     owner_node_id, owner_epoch, lease_expires_at_ms, state, recipe_json,
                     response_json, produced_playable_through_ms, fetched_through_ms,
                     media_origin_ms, media_sequence, discontinuity_sequence, updated_at_ms)
                 SELECT $1, $2, $3, $4, $5, $6, 1, $7, 'active', $8, $9,
                        0, 0, $10, 0, 0, $11
                  WHERE (SELECT COUNT(*) FROM media_sessions
                          WHERE user_id = $3 AND state IN ('starting', 'active')
                            AND lease_expires_at_ms > $11
                            AND incarnation_id != $1
                            AND incarnation_id != COALESCE((
                              SELECT current_incarnation_id FROM media_playback_pointers
                               WHERE user_id = $3 AND playback_id = $4), '')) < $12
                    AND ($13 = '' OR EXISTS (
                      SELECT 1 FROM media_session_requests
                       WHERE user_id = $3 AND request_id = $13 AND incarnation_id = $1
                         AND request_fingerprint = $5 AND playback_id = $4
                         AND owner_node_id = $6
                         AND ((state = 'starting' AND claim_expires_at_ms > $11)
                           OR (state = 'resolved' AND response_json = $9))))
                    AND EXISTS (SELECT 1 FROM job_leases
                      WHERE resource = $14 AND owner_node_id = $6 AND fence = 1
                        AND expires_at_ms = $7 AND expires_at_ms > $11
                        AND updated_at_ms = $11
                        AND revision < 9223372036854775807)
                    AND (SELECT COUNT(*) FROM media_sessions
                          WHERE user_id = $3 AND incarnation_id != $1) < $15
                    AND (SELECT COUNT(*) FROM media_sessions
                          WHERE owner_node_id = $6 AND state = 'active'
                            AND lease_expires_at_ms > $11
                            AND incarnation_id != $1
                            AND incarnation_id != COALESCE((
                              SELECT current_incarnation_id FROM media_playback_pointers
                               WHERE user_id = $3 AND playback_id = $4), '')) < $16
                    AND NOT EXISTS (SELECT 1 FROM settings WHERE key = $17)
                 ON CONFLICT(incarnation_id) DO UPDATE SET
                    lease_expires_at_ms = excluded.lease_expires_at_ms,
                    response_json = excluded.response_json,
                    updated_at_ms = excluded.updated_at_ms
                 WHERE media_sessions.session_id = excluded.session_id
                   AND media_sessions.user_id = excluded.user_id
                   AND media_sessions.playback_id = excluded.playback_id
                   AND media_sessions.request_fingerprint = excluded.request_fingerprint
                   AND media_sessions.owner_node_id = excluded.owner_node_id
                   AND media_sessions.owner_epoch = 1 AND media_sessions.state = 'active'",
                params!(
                    activation.incarnation_id.as_str(),
                    activation.session_id.as_str(),
                    activation.user_id,
                    activation.playback_id.as_str(),
                    activation.request_fingerprint.as_str(),
                    activation.owner_node_id.as_str(),
                    activation.lease_expires_at_ms,
                    activation.recipe_json.as_str(),
                    activation.response_json.as_str(),
                    activation.media_origin_ms,
                    activation.now_ms,
                    MAX_CURRENT_PER_USER,
                    request_id,
                    lease_resource.as_str(),
                    MAX_SESSION_ROWS_PER_USER,
                    MAX_OWNED,
                    removed_owner_key.as_str()
                ),
            ),
            (
                "UPDATE media_sessions SET state = 'ended', lease_expires_at_ms = $1,
                        updated_at_ms = $1
                  WHERE incarnation_id = (SELECT current_incarnation_id
                      FROM media_playback_pointers WHERE user_id = $2 AND playback_id = $3)
                    AND incarnation_id != $4 AND state != 'ended'
                    AND EXISTS (SELECT 1 FROM media_sessions
                      WHERE incarnation_id = $4 AND session_id = $5
                        AND owner_node_id = $6 AND state = 'active')
                    AND $7 != '' AND incarnation_id = $7",
                params!(
                    activation.now_ms,
                    activation.user_id,
                    activation.playback_id.as_str(),
                    activation.incarnation_id.as_str(),
                    activation.session_id.as_str(),
                    activation.owner_node_id.as_str(),
                    predecessor_incarnation
                ),
            ),
            (
                "DELETE FROM cache_consumer_pins
                  WHERE consumer_kind = 'media_session'
                    AND consumer_id IN (
                      SELECT incarnation_id FROM media_sessions
                       WHERE user_id = $1 AND playback_id = $2
                         AND incarnation_id != $3 AND state = 'ended'
                         AND updated_at_ms = $4)",
                params!(
                    activation.user_id,
                    activation.playback_id.as_str(),
                    activation.incarnation_id.as_str(),
                    activation.now_ms
                ),
            ),
            (
                "UPDATE job_leases
                    SET expires_at_ms = CASE
                          WHEN expires_at_ms < $1 THEN expires_at_ms ELSE $1 END,
                        revision = revision + 1, updated_at_ms = $1
                  WHERE revision < 9223372036854775807
                    AND EXISTS (SELECT 1 FROM media_sessions AS session
                      WHERE 'session:' || session.incarnation_id = job_leases.resource
                        AND session.user_id = $2 AND session.playback_id = $3
                        AND session.incarnation_id != $4 AND session.state = 'ended'
                        AND session.incarnation_id = $5
                        AND session.updated_at_ms = $1
                        AND session.owner_node_id = job_leases.owner_node_id
                        AND session.owner_epoch = job_leases.fence)",
                params!(
                    activation.now_ms,
                    activation.user_id,
                    activation.playback_id.as_str(),
                    activation.incarnation_id.as_str(),
                    predecessor_incarnation
                ),
            ),
            (
                "INSERT INTO media_playback_pointers
                    (user_id, playback_id, current_incarnation_id, updated_at_ms)
                 SELECT $1, $2, $3, $4 WHERE EXISTS (
                   SELECT 1 FROM media_sessions WHERE incarnation_id = $3 AND session_id = $5
                     AND owner_node_id = $6 AND state = 'active')
                   AND (($7 = '' AND NOT EXISTS (
                     SELECT 1 FROM media_playback_pointers
                      WHERE user_id = $1 AND playback_id = $2)) OR EXISTS (
                     SELECT 1 FROM media_playback_pointers
                      WHERE user_id = $1 AND playback_id = $2
                        AND current_incarnation_id IN ($7, $3)))
                 ON CONFLICT(user_id, playback_id) DO UPDATE SET
                    current_incarnation_id = excluded.current_incarnation_id,
                    updated_at_ms = excluded.updated_at_ms
                  WHERE media_playback_pointers.current_incarnation_id IN ($7, $3)",
                params!(
                    activation.user_id,
                    activation.playback_id.as_str(),
                    activation.incarnation_id.as_str(),
                    activation.now_ms,
                    activation.session_id.as_str(),
                    activation.owner_node_id.as_str(),
                    predecessor_incarnation
                ),
            ),
            (
                "UPDATE media_sessions SET state = 'ended', lease_expires_at_ms = $1,
                        updated_at_ms = $1
                  WHERE incarnation_id = $2 AND session_id = $3 AND state = 'active'
                    AND NOT EXISTS (SELECT 1 FROM media_playback_pointers
                      WHERE user_id = $4 AND playback_id = $5
                        AND current_incarnation_id = $2)",
                params!(
                    activation.now_ms,
                    activation.incarnation_id.as_str(),
                    activation.session_id.as_str(),
                    activation.user_id,
                    activation.playback_id.as_str()
                ),
            ),
            (
                "UPDATE job_leases
                    SET expires_at_ms = CASE
                          WHEN expires_at_ms < $1 THEN expires_at_ms ELSE $1 END,
                        revision = revision + 1, updated_at_ms = $1
                  WHERE resource = $2 AND owner_node_id = $3 AND fence = 1
                    AND revision < 9223372036854775807
                    AND EXISTS (SELECT 1 FROM media_sessions
                      WHERE incarnation_id = $4 AND session_id = $5 AND state = 'ended'
                        AND updated_at_ms = $1)",
                params!(
                    activation.now_ms,
                    lease_resource.as_str(),
                    activation.owner_node_id.as_str(),
                    activation.incarnation_id.as_str(),
                    activation.session_id.as_str()
                ),
            ),
            (
                "DELETE FROM job_leases WHERE resource = $1 AND owner_node_id = $2
                    AND fence = 1 AND revision = 1
                    AND NOT EXISTS (SELECT 1 FROM media_sessions
                      WHERE incarnation_id = $3)",
                params!(
                    lease_resource.as_str(),
                    activation.owner_node_id.as_str(),
                    activation.incarnation_id.as_str()
                ),
            ),
            (
                "UPDATE media_session_requests SET state = 'resolved', response_json = $1,
                        claim_expires_at_ms = $2, updated_at_ms = $3
                  WHERE $4 != '' AND user_id = $5 AND request_id = $4
                    AND incarnation_id = $6 AND request_fingerprint = $7
                    AND owner_node_id = $8 AND playback_id = $9 AND EXISTS (
                      SELECT 1 FROM media_sessions WHERE incarnation_id = $6
                        AND session_id = $10 AND state = 'active')",
                params!(
                    activation.response_json.as_str(),
                    activation.lease_expires_at_ms,
                    activation.now_ms,
                    request_id,
                    activation.user_id,
                    activation.incarnation_id.as_str(),
                    activation.request_fingerprint.as_str(),
                    activation.owner_node_id.as_str(),
                    activation.playback_id.as_str(),
                    activation.session_id.as_str()
                ),
            ),
        ];
        let changed = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        if changed.first().copied() != Some(1)
            || changed.get(1).copied() != Some(1)
            || changed.get(5).copied() != Some(1)
            || changed.get(6).copied() != Some(0)
        {
            return Ok(None);
        }
        // The pointer CAS above is the serialized activation verdict. Build
        // the initial route from the exact values committed in that same
        // transaction so a separate post-commit read cannot turn success into
        // a caller-visible failure and abort the already-owned worker.
        let route = MediaSessionRoute {
            incarnation_id: activation.incarnation_id.clone(),
            session_id: activation.session_id.clone(),
            user_id: activation.user_id,
            playback_id: activation.playback_id.clone(),
            request_fingerprint: activation.request_fingerprint.clone(),
            owner_node_id: activation.owner_node_id.clone(),
            owner_epoch: 1,
            lease_expires_at_ms: activation.lease_expires_at_ms,
            state: "active".to_owned(),
            recipe_json: activation.recipe_json.clone(),
            response_json: activation.response_json.clone(),
            produced_playable_through_ms: 0,
            fetched_through_ms: 0,
            media_origin_ms: activation.media_origin_ms,
            media_sequence: 0,
            discontinuity_sequence: 0,
            updated_at_ms: activation.now_ms,
        };
        Ok(Some(MediaSessionActivationOutcome { route, predecessor }))
    }

    async fn fail_media_session_request(
        &self,
        user_id: i64,
        request_id: &str,
        incarnation_id: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        if user_id <= 0
            || request_id.is_empty()
            || request_id.len() > 128
            || !valid_uuid(incarnation_id)
        {
            return Err(StoreError::Task("invalid media-session failure".to_owned()));
        }
        Ok(self
            .client()
            .execute(
                "UPDATE media_session_requests SET state = 'failed', claim_expires_at_ms = $1,
                        updated_at_ms = $1
                  WHERE user_id = $2 AND request_id = $3 AND incarnation_id = $4
                    AND state = 'starting'",
                params!(now_ms, user_id, request_id, incarnation_id),
            )
            .await?
            == 1)
    }

    async fn media_session_route(
        &self,
        session_id: &str,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if !valid_uuid(session_id) {
            return Ok(None);
        }
        route_by(self, "session_id", session_id).await
    }

    async fn media_session_route_by_incarnation(
        &self,
        incarnation_id: &str,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if !valid_uuid(incarnation_id) {
            return Ok(None);
        }
        route_by(self, "incarnation_id", incarnation_id).await
    }

    async fn renew_media_sessions(
        &self,
        owner_node_id: &str,
        renewals: &[MediaSessionRenewal],
        now_ms: i64,
        lease_expires_at_ms: i64,
    ) -> Result<Vec<String>, StoreError> {
        if owner_node_id.is_empty()
            || owner_node_id.len() > 256
            || renewals.len() > MAX_RENEWALS
            || renewals.iter().any(|renewal| !valid_renewal(renewal))
            || lease_expires_at_ms <= now_ms
        {
            return Err(StoreError::Task(
                "invalid media-session renewal batch".to_owned(),
            ));
        }
        if renewals.is_empty() {
            return Ok(Vec::new());
        }
        let owner_fence_key = removed_job_owner_key(owner_node_id);
        let mut statements = Vec::with_capacity(renewals.len().saturating_mul(3));
        for renewal in renewals {
            let lease_resource = format!("session:{}", renewal.incarnation_id);
            statements.push((
                "UPDATE job_leases SET expires_at_ms = $1, revision = revision + 1,
                        updated_at_ms = $2
                  WHERE resource = $3 AND owner_node_id = $4 AND fence = $5
                    AND expires_at_ms > $2 AND revision < 9223372036854775807
                    AND NOT EXISTS (SELECT 1 FROM settings WHERE key = $6)
                    AND expires_at_ms = (SELECT lease_expires_at_ms FROM media_sessions
                      WHERE incarnation_id = $7 AND owner_node_id = $4 AND owner_epoch = $5
                        AND state = 'active' AND lease_expires_at_ms > $2)",
                params!(
                    lease_expires_at_ms,
                    now_ms,
                    lease_resource.as_str(),
                    owner_node_id,
                    renewal.owner_epoch,
                    owner_fence_key.as_str(),
                    renewal.incarnation_id.as_str()
                ),
            ));
            statements.push((
                "UPDATE media_sessions SET lease_expires_at_ms = $1, updated_at_ms = $2,
                        produced_playable_through_ms = MAX(
                          produced_playable_through_ms, $7),
                        fetched_through_ms = MAX(fetched_through_ms, $8),
                        media_sequence = MAX(media_sequence, $9)
                      WHERE incarnation_id = $3 AND owner_node_id = $4 AND owner_epoch = $5
                        AND state = 'active' AND lease_expires_at_ms > $2
                        AND EXISTS (SELECT 1 FROM job_leases
                          WHERE resource = $6 AND owner_node_id = $4 AND fence = $5
                            AND expires_at_ms = $1)",
                params!(
                    lease_expires_at_ms,
                    now_ms,
                    renewal.incarnation_id.as_str(),
                    owner_node_id,
                    renewal.owner_epoch,
                    lease_resource.as_str(),
                    renewal.produced_playable_through_ms,
                    renewal.fetched_through_ms,
                    renewal.media_sequence
                ),
            ));
            statements.push((
                "UPDATE cache_consumer_pins
                    SET expires_at_ms = CASE
                          WHEN expires_at_ms < $1 THEN $1 ELSE expires_at_ms END
                  WHERE consumer_kind = 'media_session' AND consumer_id = $2
                    AND consumer_epoch = $3 AND expires_at_ms > $4
                    AND EXISTS (
                        SELECT 1 FROM transcode_cache_locations location
                         WHERE location.storage_id = cache_consumer_pins.storage_id
                           AND location.recipe_hash = cache_consumer_pins.recipe_hash
                           AND location.generation_id = cache_consumer_pins.generation_id
                           AND location.storage_class = 'shared'
                           AND location.complete = 1)
                    AND EXISTS (
                        SELECT 1 FROM media_sessions session
                         WHERE session.incarnation_id = $2
                           AND session.owner_node_id = $5
                           AND session.owner_epoch = $3
                           AND session.state = 'active'
                           AND session.lease_expires_at_ms = $1)",
                params!(
                    lease_expires_at_ms,
                    renewal.incarnation_id.as_str(),
                    renewal.owner_epoch,
                    now_ms,
                    owner_node_id
                ),
            ));
        }
        let changed = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(renewals
            .iter()
            .zip(changed.chunks_exact(3))
            .filter(|(_, rows)| rows[0..2] == [1, 1])
            .map(|(renewal, _)| renewal.incarnation_id.clone())
            .collect())
    }

    async fn expired_media_sessions(
        &self,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<MediaSessionRoute>, StoreError> {
        if now_ms < 0 {
            return Err(StoreError::Task(
                "invalid media-session takeover inventory time".to_owned(),
            ));
        }
        let limit = i64::try_from(limit.min(MAX_TAKEOVER_CANDIDATES)).unwrap_or(0);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {ROUTE_COLS} FROM media_sessions
              WHERE state = 'active' AND lease_expires_at_ms <= $1
              ORDER BY lease_expires_at_ms, incarnation_id LIMIT $2"
        );
        validate_sql(&sql)?;
        Ok(self
            .client()
            .query_consistent_map::<RouteRow, _>(sql, params!(now_ms, limit))
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn claim_media_session_takeover(
        &self,
        takeover: &MediaSessionTakeover,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        validate_takeover(takeover)?;
        let lease_resource = format!("session:{}", takeover.incarnation_id);
        let next_epoch = takeover.expected_owner_epoch.saturating_add(1);
        let removed_owner_key = removed_job_owner_key(&takeover.next_owner_node_id);
        let changed = self
            .client()
            .txn([
                (
                    "UPDATE job_leases
                        SET owner_node_id = $1, fence = $2, revision = revision + 1,
                            expires_at_ms = $3, updated_at_ms = $4
                      WHERE resource = $5 AND owner_node_id = $6 AND fence = $7
                        AND expires_at_ms <= $4
                        AND fence < 9223372036854775807
                        AND revision < 9223372036854775807
                        AND NOT EXISTS (SELECT 1 FROM settings WHERE key = $8)
                        AND expires_at_ms = (SELECT lease_expires_at_ms
                          FROM media_sessions WHERE incarnation_id = $9
                            AND owner_node_id = $6 AND owner_epoch = $7
                            AND state = 'active' AND lease_expires_at_ms <= $4
                            AND discontinuity_sequence < 9223372036854775807)",
                    params!(
                        takeover.next_owner_node_id.as_str(),
                        next_epoch,
                        takeover.lease_expires_at_ms,
                        takeover.now_ms,
                        lease_resource.as_str(),
                        takeover.expected_owner_node_id.as_str(),
                        takeover.expected_owner_epoch,
                        removed_owner_key.as_str(),
                        takeover.incarnation_id.as_str()
                    ),
                ),
                (
                    "UPDATE media_sessions
                        SET owner_node_id = $1, owner_epoch = $2,
                            lease_expires_at_ms = $3,
                            discontinuity_sequence = discontinuity_sequence + 1,
                            updated_at_ms = $4
                      WHERE incarnation_id = $5 AND owner_node_id = $6 AND owner_epoch = $7
                        AND state = 'active' AND lease_expires_at_ms <= $4
                        AND discontinuity_sequence < 9223372036854775807
                        AND EXISTS (SELECT 1 FROM job_leases
                          WHERE resource = $8 AND owner_node_id = $1 AND fence = $2
                            AND expires_at_ms = $3 AND updated_at_ms = $4)",
                    params!(
                        takeover.next_owner_node_id.as_str(),
                        next_epoch,
                        takeover.lease_expires_at_ms,
                        takeover.now_ms,
                        takeover.incarnation_id.as_str(),
                        takeover.expected_owner_node_id.as_str(),
                        takeover.expected_owner_epoch,
                        lease_resource.as_str()
                    ),
                ),
                (
                    "DELETE FROM cache_consumer_pins
                      WHERE consumer_kind = 'media_session' AND consumer_id = $1
                        AND consumer_epoch = $2
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = $1 AND owner_node_id = $3
                            AND owner_epoch = $4 AND state = 'active')",
                    params!(
                        takeover.incarnation_id.as_str(),
                        takeover.expected_owner_epoch,
                        takeover.next_owner_node_id.as_str(),
                        next_epoch
                    ),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        if changed.first().copied() != Some(1) || changed.get(1).copied() != Some(1) {
            return Ok(None);
        }
        let route = route_by(self, "incarnation_id", &takeover.incarnation_id).await?;
        Ok(route.filter(|route| {
            route.owner_node_id == takeover.next_owner_node_id
                && route.owner_epoch == next_epoch
                && route.lease_expires_at_ms == takeover.lease_expires_at_ms
                && route.updated_at_ms == takeover.now_ms
        }))
    }

    async fn end_media_session(
        &self,
        session_id: &str,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if !valid_uuid(session_id) {
            return Ok(None);
        }
        let route = route_by(self, "session_id", session_id).await?;
        let Some(route) = route else {
            return Ok(None);
        };
        let lease_resource = format!("session:{}", route.incarnation_id);
        self.client()
            .txn([
                (
                    "UPDATE media_sessions SET state = 'ended', lease_expires_at_ms = $1,
                            updated_at_ms = $1 WHERE incarnation_id = $2 AND state != 'ended'",
                    params!(now_ms, route.incarnation_id.as_str()),
                ),
                (
                    "UPDATE job_leases
                        SET expires_at_ms = CASE
                              WHEN expires_at_ms < $1 THEN expires_at_ms ELSE $1 END,
                            revision = revision + 1, updated_at_ms = $1
                      WHERE resource = $2 AND owner_node_id = $3 AND fence = $4
                        AND revision < 9223372036854775807
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = $5 AND state = 'ended')",
                    params!(
                        now_ms,
                        lease_resource.as_str(),
                        route.owner_node_id.as_str(),
                        route.owner_epoch,
                        route.incarnation_id.as_str()
                    ),
                ),
                (
                    "DELETE FROM media_playback_pointers
                      WHERE user_id = $1 AND playback_id = $2 AND current_incarnation_id = $3",
                    params!(
                        route.user_id,
                        route.playback_id.as_str(),
                        route.incarnation_id.as_str()
                    ),
                ),
                (
                    "DELETE FROM cache_consumer_pins
                      WHERE consumer_kind = 'media_session' AND consumer_id = $1
                        AND consumer_epoch = $2
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = $1 AND state = 'ended')",
                    params!(route.incarnation_id.as_str(), route.owner_epoch),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(Some(route))
    }

    async fn maintain_media_sessions(&self, now_ms: i64) -> Result<(), StoreError> {
        let failed_cutoff = now_ms.saturating_sub(FAILED_RETENTION_MS);
        let retained_cutoff = now_ms.saturating_sub(RESOLVED_RETENTION_MS);
        let retire_before = now_ms.saturating_sub(TAKEOVER_RECOVERY_MS);
        // A Hiqlite transaction is a replicated Raft proposal even when all
        // statements affect zero rows. Keep idle clusters out of the write
        // log by proving that at least one bounded cleanup has work first.
        let pending = self
            .client()
            .query_consistent_map::<PendingMaintenanceRow, _>(
                "SELECT 1 AS pending WHERE
                    EXISTS (SELECT 1 FROM media_sessions
                      WHERE state = 'active' AND lease_expires_at_ms <= $4)
                    OR EXISTS (SELECT 1 FROM job_leases lease
                      JOIN media_sessions session
                        ON lease.resource = 'session:' || session.incarnation_id
                      WHERE session.state = 'ended' AND session.lease_expires_at_ms <= $1
                        AND lease.expires_at_ms > $1
                        AND lease.revision < 9223372036854775807)
                    OR EXISTS (SELECT 1 FROM media_playback_pointers pointer
                      LEFT JOIN media_sessions session
                        ON session.incarnation_id = pointer.current_incarnation_id
                      WHERE session.incarnation_id IS NULL OR session.state != 'active'
                        OR session.lease_expires_at_ms <= $4)
                    OR EXISTS (SELECT 1 FROM media_session_requests
                      WHERE state = 'starting' AND claim_expires_at_ms <= $1)
                    OR EXISTS (SELECT 1 FROM media_session_requests
                      WHERE state = 'failed' AND updated_at_ms < $2)
                    OR EXISTS (SELECT 1 FROM media_session_requests request
                      WHERE request.state = 'resolved' AND request.updated_at_ms < $3
                        AND NOT EXISTS (SELECT 1 FROM media_sessions session
                          WHERE session.incarnation_id = request.incarnation_id
                            AND session.state = 'active'
                            AND session.lease_expires_at_ms > $1))
                    OR EXISTS (SELECT 1 FROM media_sessions
                      WHERE state = 'ended' AND updated_at_ms < $3)
                    OR EXISTS (SELECT 1 FROM job_leases lease
                      WHERE lease.resource LIKE 'session:%' AND lease.updated_at_ms < $3
                        AND NOT EXISTS (SELECT 1 FROM media_sessions session
                          WHERE lease.resource = 'session:' || session.incarnation_id))",
                params!(now_ms, failed_cutoff, retained_cutoff, retire_before),
            )
            .await?
            .into_iter()
            .next()
            .is_some_and(|row| row.0 == 1);
        if !pending {
            return Ok(());
        }
        let statements = vec![
            (
                "UPDATE media_sessions SET state = 'ended', lease_expires_at_ms = $1,
                        updated_at_ms = $1
                  WHERE incarnation_id IN (
                    SELECT incarnation_id FROM media_sessions
                     WHERE state = 'active' AND lease_expires_at_ms <= $3
                     ORDER BY lease_expires_at_ms, incarnation_id LIMIT $2)",
                params!(now_ms, MAINTENANCE_BATCH, retire_before),
            ),
            (
                "DELETE FROM cache_consumer_pins
                  WHERE consumer_kind = 'media_session'
                    AND consumer_id IN (
                      SELECT incarnation_id FROM media_sessions
                       WHERE state = 'ended' AND lease_expires_at_ms <= $1
                       ORDER BY updated_at_ms, incarnation_id LIMIT $2)",
                params!(now_ms, MAINTENANCE_BATCH),
            ),
            (
                "UPDATE job_leases
                    SET expires_at_ms = CASE
                          WHEN expires_at_ms < $1 THEN expires_at_ms ELSE $1 END,
                        revision = revision + 1, updated_at_ms = $1
                  WHERE revision < 9223372036854775807
                    AND expires_at_ms > $1
                    AND resource IN (
                      SELECT 'session:' || session.incarnation_id
                        FROM media_sessions session
                        JOIN job_leases lease
                          ON lease.resource = 'session:' || session.incarnation_id
                       WHERE session.state = 'ended'
                         AND session.lease_expires_at_ms <= $1
                         AND lease.expires_at_ms > $1
                       ORDER BY session.updated_at_ms, session.incarnation_id LIMIT $2)",
                params!(now_ms, MAINTENANCE_BATCH),
            ),
            (
                "DELETE FROM media_playback_pointers WHERE rowid IN (
                   SELECT pointer.rowid FROM media_playback_pointers pointer
                   LEFT JOIN media_sessions session
                     ON session.incarnation_id = pointer.current_incarnation_id
                  WHERE session.incarnation_id IS NULL OR session.state != 'active'
                     OR session.lease_expires_at_ms <= $3
                  ORDER BY pointer.updated_at_ms, pointer.rowid LIMIT $2)",
                params!(now_ms, MAINTENANCE_BATCH, retire_before),
            ),
            (
                "DELETE FROM media_session_requests WHERE rowid IN (
                   SELECT rowid FROM media_session_requests
                    WHERE state = 'starting' AND claim_expires_at_ms <= $1
                    ORDER BY claim_expires_at_ms, rowid LIMIT $2)",
                params!(now_ms, MAINTENANCE_BATCH),
            ),
            (
                "DELETE FROM media_session_requests WHERE rowid IN (
                   SELECT rowid FROM media_session_requests
                    WHERE state = 'failed' AND updated_at_ms < $1
                    ORDER BY updated_at_ms, rowid LIMIT $2)",
                params!(failed_cutoff, MAINTENANCE_BATCH),
            ),
            (
                "DELETE FROM media_session_requests WHERE rowid IN (
                   SELECT request.rowid FROM media_session_requests request
                    WHERE request.state = 'resolved' AND request.updated_at_ms < $1
                      AND NOT EXISTS (SELECT 1 FROM media_sessions session
                        WHERE session.incarnation_id = request.incarnation_id
                          AND session.state = 'active' AND session.lease_expires_at_ms > $2)
                    ORDER BY request.updated_at_ms, request.rowid LIMIT $3)",
                params!(retained_cutoff, now_ms, MAINTENANCE_BATCH),
            ),
            (
                "DELETE FROM job_leases WHERE rowid IN (
                   SELECT lease.rowid FROM job_leases lease
                   JOIN media_sessions session
                     ON lease.resource = 'session:' || session.incarnation_id
                    WHERE session.state = 'ended' AND session.updated_at_ms < $1
                    ORDER BY session.updated_at_ms, lease.rowid LIMIT $2)",
                params!(retained_cutoff, MAINTENANCE_BATCH),
            ),
            (
                "DELETE FROM media_sessions WHERE rowid IN (
                   SELECT rowid FROM media_sessions
                    WHERE state = 'ended' AND updated_at_ms < $1
                    ORDER BY updated_at_ms, rowid LIMIT $2)",
                params!(retained_cutoff, MAINTENANCE_BATCH),
            ),
            (
                "DELETE FROM job_leases WHERE rowid IN (
                   SELECT lease.rowid FROM job_leases lease
                    WHERE lease.resource LIKE 'session:%' AND lease.updated_at_ms < $1
                      AND NOT EXISTS (SELECT 1 FROM media_sessions session
                        WHERE lease.resource = 'session:' || session.incarnation_id)
                    ORDER BY lease.updated_at_ms, lease.rowid LIMIT $2)",
                params!(retained_cutoff, MAINTENANCE_BATCH),
            ),
        ];
        self.client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(())
    }

    async fn owned_media_sessions(
        &self,
        owner_node_id: &str,
        now_ms: i64,
    ) -> Result<Vec<OwnedMediaSessionLease>, StoreError> {
        if owner_node_id.is_empty() || owner_node_id.len() > 256 {
            return Err(StoreError::Task("invalid media-session owner".to_owned()));
        }
        Ok(self
            .client()
            .query_consistent_map::<OwnedLeaseRow, _>(
                "SELECT incarnation_id, session_id, owner_epoch, lease_expires_at_ms
                   FROM media_sessions
                  WHERE owner_node_id = $1 AND state = 'active'
                    AND lease_expires_at_ms > $2
                  ORDER BY updated_at_ms, incarnation_id LIMIT $3",
                params!(owner_node_id, now_ms, MAX_OWNED),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }
}
