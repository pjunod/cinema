//! Replicated idempotency, ownership, and routing for live HLS sessions.

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::Row;

use super::hiqlite::{database_error, timeout_store, validate_sql, HiqliteAuthStore};
use super::MediaSessionStore;
use crate::cluster::coordination::removed_job_owner_key;
use crate::domain::{
    MediaSessionActivation, MediaSessionActivationOutcome, MediaSessionActivationSettlement,
    MediaSessionEnd, MediaSessionProjectionCompletion, MediaSessionRenewal,
    MediaSessionRequestClaim, MediaSessionRoute, MediaSessionTakeover, MediaSessionTakeoverCursor,
    MediaSessionTerminalAck, OwnedMediaSessionLease, MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS,
    MEDIA_SESSION_PUBLICATION_BLOCKED,
};
use crate::error::StoreError;

#[cfg(feature = "hiqlite-contract-tests")]
type ActivationPointerReadPause = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);

/// Contract-only seam which freezes one activation after its optimistic
/// pointer read and before its replicated transaction is submitted. It lets
/// the three-voter contract deterministically order activation+renewal inside
/// that otherwise unobservable TOCTOU window.
#[cfg(feature = "hiqlite-contract-tests")]
static ACTIVATION_POINTER_READ_PAUSE: std::sync::LazyLock<
    std::sync::Mutex<Option<ActivationPointerReadPause>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

#[cfg(feature = "hiqlite-contract-tests")]
impl HiqliteAuthStore {
    pub fn validation_pause_next_activation_after_pointer_read() -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (reached_sender, reached_receiver) = tokio::sync::oneshot::channel();
        let (release_sender, release_receiver) = tokio::sync::oneshot::channel();
        let mut pause = ACTIVATION_POINTER_READ_PAUSE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            pause.is_none(),
            "activation pointer-read pause already armed"
        );
        *pause = Some((reached_sender, release_receiver));
        (reached_receiver, release_sender)
    }
}

const MAX_IN_FLIGHT_PER_USER: i64 = 32;
const MAX_CURRENT_PER_USER: i64 = 64;
const MAX_REQUEST_ROWS_PER_USER: i64 = 4_096;
const MAX_SESSION_ROWS_PER_USER: i64 = 4_096;
const MAX_RENEWALS: usize = 256;
const MAX_TAKEOVER_CANDIDATES: usize = 64;
const MAX_OWNED: i64 = 4_096;
const MAINTENANCE_BATCH: i64 = 256;
const MAX_MEDIA_MILLIS: i64 = 366 * 24 * 60 * 60 * 1_000;
const MAX_TERMINAL_ACK_BYTES: usize = 64 * 1024;
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
    terminal_reason               TEXT CHECK (terminal_reason IN
                                      ('deleted', 'superseded', 'admin_stop', 'revoked', 'replaced')),
    publication_ready_at_ms       INTEGER NOT NULL DEFAULT 0 CHECK (publication_ready_at_ms >= 0),
    recipe_json                   TEXT NOT NULL,
    response_json                 TEXT NOT NULL,
    produced_playable_through_ms  INTEGER NOT NULL DEFAULT 0,
    fetched_through_ms            INTEGER NOT NULL DEFAULT 0,
    media_origin_ms               INTEGER NOT NULL DEFAULT 0,
    media_sequence                INTEGER NOT NULL DEFAULT 0,
    discontinuity_sequence        INTEGER NOT NULL DEFAULT 0,
    updated_at_ms                 INTEGER NOT NULL
) STRICT";

/// Exact v10 shape used only by the v9 -> v10 migration. Later additive
/// migrations own the terminal-cause and publication-fence columns; using the
/// fresh schema here would make those later ALTERs fail on direct v9 upgrades.
pub(super) const MEDIA_SESSIONS_V10_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS media_sessions (
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

pub(super) const MEDIA_SESSION_TERMINAL_ACKS_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS media_session_terminal_acks (
    incarnation_id     TEXT NOT NULL UNIQUE,
    session_id         TEXT PRIMARY KEY,
    owner_node_id      TEXT NOT NULL,
    owner_epoch        INTEGER NOT NULL CHECK (owner_epoch > 0),
    client_instance_id TEXT NOT NULL,
    sequence           INTEGER NOT NULL CHECK (sequence > 0),
    request_fingerprint TEXT NOT NULL,
    response_json      TEXT NOT NULL,
    expires_at_ms      INTEGER NOT NULL,
    updated_at_ms      INTEGER NOT NULL
) STRICT";

pub(super) const MEDIA_SESSION_TERMINAL_ACKS_EXPIRY_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS media_session_terminal_acks_expiry
        ON media_session_terminal_acks(expires_at_ms, session_id)";

pub(super) const MEDIA_SESSION_TERMINAL_REASON_MIGRATION: &str =
    "ALTER TABLE media_sessions ADD COLUMN terminal_reason TEXT
        CHECK (terminal_reason IN
          ('deleted', 'superseded', 'admin_stop', 'revoked', 'replaced'))";

pub(super) const MEDIA_SESSION_PUBLICATION_FENCE_MIGRATION: &str =
    "ALTER TABLE media_sessions ADD COLUMN publication_ready_at_ms INTEGER NOT NULL DEFAULT 0
        CHECK (publication_ready_at_ms >= 0)";

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
        super::MEDIA_SESSION_PUBLICATION_CLAIM_TRIGGER_SCHEMA,
        MEDIA_SESSION_TERMINAL_ACKS_SCHEMA,
        MEDIA_SESSION_TERMINAL_ACKS_EXPIRY_INDEX,
        super::MEDIA_SESSION_PREPARATIONS_SCHEMA,
    ] {
        validate_sql(sql)?;
        for result in timeout_store(client.batch(sql)).await? {
            result.map_err(database_error)?;
        }
    }
    Ok(())
}

const ROUTE_COLS: &str = "incarnation_id, session_id, user_id, playback_id,
    request_fingerprint, owner_node_id, owner_epoch, lease_expires_at_ms, state, terminal_reason,
    publication_ready_at_ms, recipe_json, response_json, produced_playable_through_ms, fetched_through_ms,
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
            terminal_reason: row.get("terminal_reason"),
            publication_ready_at_ms: row.get("publication_ready_at_ms"),
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

struct TerminalAckRow(MediaSessionTerminalAck);

impl From<&mut Row<'_>> for TerminalAckRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(MediaSessionTerminalAck {
            incarnation_id: row.get("incarnation_id"),
            session_id: row.get("session_id"),
            owner_node_id: row.get("owner_node_id"),
            owner_epoch: row.get("owner_epoch"),
            client_instance_id: row.get("client_instance_id"),
            sequence: row.get("sequence"),
            request_fingerprint: row.get("request_fingerprint"),
            response_json: row.get("response_json"),
            expires_at_ms: row.get("expires_at_ms"),
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

struct StagedRow(crate::domain::MediaSessionStagedGeneration);

impl From<&mut Row<'_>> for StagedRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(crate::domain::MediaSessionStagedGeneration {
            user_id: row.get("user_id"),
            playback_id: row.get("playback_id"),
            staged_incarnation_id: row.get("staged_incarnation_id"),
            expected_predecessor_incarnation_id: row.get("expected_predecessor_incarnation_id"),
            deadline_ms: row.get("deadline_ms"),
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
        })
    }
}

const STAGED_COLS: &str = "user_id, playback_id, staged_incarnation_id,
    expected_predecessor_incarnation_id, deadline_ms, created_at_ms, updated_at_ms";

/// Is this commit an exact replay?
///
/// The pointer is the discriminator: if it names the incarnation the caller
/// asked about, this commit already happened and its outcome is the truthful
/// answer. `predecessor` is `None` because the retirement it reports belongs
/// to the transaction that actually ran.
async fn commit_replay(
    store: &HiqliteAuthStore,
    user_id: i64,
    playback_id: &str,
    staged_incarnation_id: &str,
) -> Result<Option<crate::domain::MediaSessionPreparationCommit>, StoreError> {
    let pointer = store
        .client()
        .query_consistent_map::<PointerRow, _>(
            "SELECT current_incarnation_id FROM media_playback_pointers
              WHERE user_id = $1 AND playback_id = $2",
            params!(user_id, playback_id),
        )
        .await?
        .into_iter()
        .next()
        .map(|row| row.0);
    if pointer.as_deref() != Some(staged_incarnation_id) {
        return Ok(None);
    }
    Ok(route_by(store, "incarnation_id", staged_incarnation_id)
        .await?
        .map(|route| crate::domain::MediaSessionPreparationCommit {
            route,
            predecessor: None,
        }))
}

async fn staged_row(
    store: &HiqliteAuthStore,
    user_id: i64,
    playback_id: &str,
) -> Result<Option<crate::domain::MediaSessionStagedGeneration>, StoreError> {
    let sql = format!(
        "SELECT {STAGED_COLS} FROM media_session_preparations
          WHERE user_id = $1 AND playback_id = $2"
    );
    validate_sql(&sql)?;
    Ok(store
        .client()
        .query_consistent_map::<StagedRow, _>(sql, params!(user_id, playback_id))
        .await?
        .into_iter()
        .next()
        .map(|row| row.0))
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

fn valid_terminal_ack(ack: &MediaSessionTerminalAck) -> bool {
    valid_uuid(&ack.incarnation_id)
        && valid_uuid(&ack.session_id)
        && !ack.owner_node_id.is_empty()
        && ack.owner_node_id.len() <= 256
        && ack.owner_epoch > 0
        && valid_uuid(&ack.client_instance_id)
        && ack.sequence > 0
        && valid_fingerprint(&ack.request_fingerprint)
        && !ack.response_json.is_empty()
        && ack.response_json.len() <= MAX_TERMINAL_ACK_BYTES
        && ack.updated_at_ms > 0
        && ack.expires_at_ms > ack.updated_at_ms
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
        && activation.publication_ready_at_ms == MEDIA_SESSION_PUBLICATION_BLOCKED
        && (0..=MAX_MEDIA_MILLIS).contains(&activation.media_origin_ms)
        && activation.lease_expires_at_ms > activation.now_ms;
    valid
        .then_some(())
        .ok_or_else(|| StoreError::Task("invalid media-session activation".to_owned()))
}

/// Exact immutable identity for an activation replay. Lease, progress,
/// publication, and update coordinates are deliberately absent: those are
/// monotone durable state which a replay must return, never restore from its
/// original input.
fn validate_preparation(
    preparation: &crate::domain::MediaSessionPreparation,
) -> Result<(), StoreError> {
    let valid = valid_uuid(&preparation.incarnation_id)
        && valid_uuid(&preparation.session_id)
        && valid_uuid(&preparation.expected_predecessor_incarnation_id)
        // A successor staged against itself is not a successor, and the
        // pointer guard would pass for it because the pointer would name it.
        && preparation.expected_predecessor_incarnation_id != preparation.incarnation_id
        && preparation.user_id > 0
        && !preparation.playback_id.is_empty()
        && preparation.playback_id.len() <= 128
        && valid_fingerprint(&preparation.request_fingerprint)
        && !preparation.owner_node_id.is_empty()
        && preparation.owner_node_id.len() <= 256
        && preparation.recipe_json.len() <= 32 * 1024
        && preparation.response_json.len() <= 64 * 1024
        && (0..=MAX_MEDIA_MILLIS).contains(&preparation.media_origin_ms)
        && preparation.now_ms > 0
        && preparation.deadline_ms > preparation.now_ms;
    if valid {
        Ok(())
    } else {
        Err(StoreError::Task(
            "invalid media-session preparation".to_owned(),
        ))
    }
}

/// Exact immutable identity for a preparation replay.
///
/// Progress and lease coordinates are deliberately absent for the same reason
/// they are absent from [`activation_route_matches`]: they are monotone
/// durable state a replay must read back, never restore from its input.
fn preparation_route_matches(
    route: &MediaSessionRoute,
    preparation: &crate::domain::MediaSessionPreparation,
) -> bool {
    route.session_id == preparation.session_id
        && route.user_id == preparation.user_id
        && route.playback_id == preparation.playback_id
        && route.request_fingerprint == preparation.request_fingerprint
        && route.owner_node_id == preparation.owner_node_id
        && route.state == "active"
        // The sentinel is what keeps a staged row out of takeover inventory,
        // so a replay that finds it armed is not looking at a staged row.
        && route.publication_ready_at_ms == MEDIA_SESSION_PUBLICATION_BLOCKED
}

fn activation_route_matches(
    route: &MediaSessionRoute,
    activation: &MediaSessionActivation,
) -> bool {
    route.incarnation_id == activation.incarnation_id
        && route.session_id == activation.session_id
        && route.user_id == activation.user_id
        && route.playback_id == activation.playback_id
        && route.request_fingerprint == activation.request_fingerprint
        && route.owner_node_id == activation.owner_node_id
        && route.owner_epoch == 1
        && route.state == "active"
        && route.recipe_json == activation.recipe_json
        && route.response_json == activation.response_json
        && route.media_origin_ms == activation.media_origin_ms
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
        #[cfg(feature = "hiqlite-contract-tests")]
        {
            let pointer_read_pause = {
                let mut pause = ACTIVATION_POINTER_READ_PAUSE
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                pause.take()
            };
            if let Some((reached, release)) = pointer_read_pause {
                let _ = reached.send(());
                let _ = release.await;
            }
        }
        if current_pointer.as_deref() == Some(activation.incarnation_id.as_str()) {
            let route = route_by(self, "incarnation_id", &activation.incarnation_id)
                .await?
                .filter(|route| activation_route_matches(route, activation));
            let Some(route) = route else {
                return Ok(None);
            };
            let predecessor = match activation.expected_predecessor_incarnation_id.as_deref() {
                Some(incarnation_id) => route_by(self, "incarnation_id", incarnation_id).await?,
                None => None,
            };
            return Ok(Some(MediaSessionActivationOutcome { route, predecessor }));
        }
        if activation.fence_predecessor
            && current_pointer.as_deref()
                != activation.expected_predecessor_incarnation_id.as_deref()
        {
            return Ok(None);
        }
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
                    AND NOT EXISTS (
                      SELECT 1 FROM media_playback_pointers
                       WHERE user_id = $6 AND playback_id = $7
                         AND current_incarnation_id = $8)
                 ON CONFLICT(resource) DO UPDATE SET
                    expires_at_ms = excluded.expires_at_ms,
                    revision = job_leases.revision + 1,
                    updated_at_ms = excluded.updated_at_ms
                 WHERE job_leases.owner_node_id = excluded.owner_node_id
                   AND job_leases.fence = 1 AND job_leases.expires_at_ms > $4
                   AND job_leases.revision < 9223372036854775807
                   AND NOT EXISTS (SELECT 1 FROM settings WHERE key = $5)
                   AND NOT EXISTS (
                     SELECT 1 FROM media_playback_pointers
                      WHERE user_id = $6 AND playback_id = $7
                        AND current_incarnation_id = $8)",
                params!(
                    lease_resource.as_str(),
                    activation.owner_node_id.as_str(),
                    activation.lease_expires_at_ms,
                    activation.now_ms,
                    removed_owner_key.as_str(),
                    activation.user_id,
                    activation.playback_id.as_str(),
                    activation.incarnation_id.as_str()
                ),
            ),
            (
                "INSERT INTO media_sessions
                    (incarnation_id, session_id, user_id, playback_id, request_fingerprint,
                     owner_node_id, owner_epoch, lease_expires_at_ms, state, recipe_json,
                     response_json, produced_playable_through_ms, fetched_through_ms,
                     media_origin_ms, media_sequence, discontinuity_sequence,
                     updated_at_ms, publication_ready_at_ms)
                 SELECT $1, $2, $3, $4, $5, $6, 1, $7, 'active', $8, $9,
                        0, 0, $10, 0, 0, $11, $12
                  WHERE (SELECT COUNT(*) FROM media_sessions
                          WHERE user_id = $3 AND state IN ('starting', 'active')
                            AND lease_expires_at_ms > $11
                            AND incarnation_id != $1
                            AND incarnation_id != COALESCE((
                              SELECT current_incarnation_id FROM media_playback_pointers
                               WHERE user_id = $3 AND playback_id = $4), '')) < $13
                    AND ($14 = '' OR EXISTS (
                      SELECT 1 FROM media_session_requests
                       WHERE user_id = $3 AND request_id = $14 AND incarnation_id = $1
                         AND request_fingerprint = $5 AND playback_id = $4
                         AND owner_node_id = $6
                         AND ((state = 'starting' AND claim_expires_at_ms > $11)
                           OR (state = 'resolved' AND response_json = $9))))
                    AND EXISTS (SELECT 1 FROM job_leases
                      WHERE resource = $15 AND owner_node_id = $6 AND fence = 1
                        AND expires_at_ms = $7 AND expires_at_ms > $11
                        AND updated_at_ms = $11
                        AND revision < 9223372036854775807)
                    AND (SELECT COUNT(*) FROM media_sessions
                          WHERE user_id = $3 AND incarnation_id != $1) < $16
                    AND (SELECT COUNT(*) FROM media_sessions
                          WHERE owner_node_id = $6 AND state = 'active'
                            AND lease_expires_at_ms > $11
                            AND incarnation_id != $1
                            AND incarnation_id != COALESCE((
                              SELECT current_incarnation_id FROM media_playback_pointers
                               WHERE user_id = $3 AND playback_id = $4), '')) < $17
                    AND NOT EXISTS (SELECT 1 FROM settings WHERE key = $18)
                    AND ($19 = '' OR NOT EXISTS (SELECT 1 FROM media_sessions
                      WHERE incarnation_id = $19 AND state = 'active'
                        AND publication_ready_at_ms != 0))
                    AND NOT EXISTS (
                      SELECT 1 FROM media_playback_pointers
                       WHERE user_id = $3 AND playback_id = $4
                         AND current_incarnation_id = $1)
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
                    activation.publication_ready_at_ms,
                    MAX_CURRENT_PER_USER,
                    request_id,
                    lease_resource.as_str(),
                    MAX_SESSION_ROWS_PER_USER,
                    MAX_OWNED,
                    removed_owner_key.as_str(),
                    predecessor_incarnation
                ),
            ),
            (
                "UPDATE media_sessions SET state = 'ended', terminal_reason = 'superseded', lease_expires_at_ms = $1,
                        publication_ready_at_ms = $2, updated_at_ms = $1
                  WHERE incarnation_id = (SELECT current_incarnation_id
                      FROM media_playback_pointers WHERE user_id = $3 AND playback_id = $4)
                    AND incarnation_id != $5 AND state != 'ended'
                    AND EXISTS (SELECT 1 FROM media_sessions
                      WHERE incarnation_id = $5 AND session_id = $6
                        AND owner_node_id = $7 AND state = 'active')
                    AND $8 != '' AND incarnation_id = $8",
                params!(
                    activation.now_ms,
                    MEDIA_SESSION_PUBLICATION_BLOCKED,
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
                  WHERE media_playback_pointers.current_incarnation_id IN ($7, $3)
                    AND media_playback_pointers.current_incarnation_id
                        != excluded.current_incarnation_id",
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
                "UPDATE media_sessions SET state = 'ended', terminal_reason = 'replaced', lease_expires_at_ms = $1,
                        publication_ready_at_ms = $2, updated_at_ms = $1
                  WHERE incarnation_id = $3 AND session_id = $4 AND state = 'active'
                    AND NOT EXISTS (SELECT 1 FROM media_playback_pointers
                      WHERE user_id = $5 AND playback_id = $6
                        AND current_incarnation_id = $3)",
                params!(
                    activation.now_ms,
                    MEDIA_SESSION_PUBLICATION_BLOCKED,
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
        ];
        let statement_count = statements.len();
        let changed = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        let fresh_activation = changed.first().copied() == Some(1)
            && changed.get(1).copied() == Some(1)
            && changed.get(5).copied() == Some(1)
            && changed.get(6).copied() == Some(0);
        // The optimistic pointer read above may lose a race to this exact
        // activation and a later renewal. Every write in that replay path is
        // guarded inside the transaction, so the only valid alternative to a
        // fresh activation is a wholly read-only transaction. The exact
        // post-transaction projection below distinguishes that replay from a
        // transaction which merely failed all of its preconditions.
        let transaction_replay =
            changed.len() == statement_count && changed.iter().all(|affected| *affected == 0);
        if !fresh_activation && !transaction_replay {
            return Ok(None);
        }
        // INSERT conflict is an idempotent replay, not proof that every field
        // still equals the activation input. Handoff settlement, renewal, or
        // takeover may already have advanced the durable row. Return an exact
        // post-commit projection so replay cannot regress a finite/ready fence
        // or overwrite progress in the route cache. Read failure remains
        // commit-unknown to the caller's exact activation reconciler.
        let route = route_by(self, "incarnation_id", &activation.incarnation_id)
            .await?
            .filter(|route| activation_route_matches(route, activation));
        let Some(route) = route else {
            return Ok(None);
        };
        let committed_pointer = self
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
        if committed_pointer.as_deref() != Some(activation.incarnation_id.as_str()) {
            return Ok(None);
        }
        // Takeover may have advanced the predecessor between the pointer read
        // and this transaction. Once the CAS above ends that incarnation it
        // can no longer advance, so this post-commit row is the authoritative
        // owner/epoch the terminal projection must acknowledge.
        let predecessor =
            match (!predecessor_incarnation.is_empty()).then_some(predecessor_incarnation) {
                Some(incarnation) => route_by(self, "incarnation_id", incarnation).await?,
                None => None,
            };
        Ok(Some(MediaSessionActivationOutcome { route, predecessor }))
    }

    async fn prepare_media_session(
        &self,
        preparation: &crate::domain::MediaSessionPreparation,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        validate_preparation(preparation)?;
        // Replay by exact identity, before anything is attempted. The ledger's
        // primary key would otherwise turn an owner's retry into "you already
        // have one".
        if let Some(existing) =
            staged_row(self, preparation.user_id, &preparation.playback_id).await?
        {
            if existing.staged_incarnation_id != preparation.incarnation_id
                || existing.expected_predecessor_incarnation_id
                    != preparation.expected_predecessor_incarnation_id
            {
                return Ok(None);
            }
            return Ok(
                route_by(self, "incarnation_id", &preparation.incarnation_id)
                    .await?
                    .filter(|route| preparation_route_matches(route, preparation)),
            );
        }
        // Every precondition is inlined into each statement's WHERE, because
        // `txn` takes a fixed statement list and cannot read, branch or
        // RETURNING. The pointer identity, the ledger's emptiness and all
        // three admission bounds therefore appear inside the INSERT itself.
        let prepare_lease_resource = format!("session:{}", preparation.incarnation_id);
        let removed_owner_key = removed_job_owner_key(&preparation.owner_node_id);
        let statements: Vec<(&str, hiqlite::Params)> = vec![
            (
                // The staged row's own session lease, taken before the row as
                // activation does. Without it the successor can never be
                // renewed or taken over: renewal's first statement is an
                // UPDATE on this exact resource, and takeover requires it.
                //
                // It carries the WHOLE precondition set, not just its own.
                // There is no rollback here — a replicated transaction that
                // commits has committed — so a lease taken on a prepare that
                // the later statements then refuse is a durable orphan, and
                // the orphan satisfies the next attempt's lease guard, which
                // makes that attempt insert its rows and still report a loss.
                // Every statement in this transaction therefore fires on
                // exactly the same conditions or none of them do.
                //
                // An upsert rather than an insert-if-absent, for the same
                // reason activation uses one: a recycled resource left by a
                // prior life must be refreshable by its own owner instead of
                // wedging the playback until retention expiry.
                "INSERT INTO job_leases
                    (resource, owner_node_id, fence, revision, expires_at_ms, updated_at_ms)
                 SELECT $1, $2, 1, 1, $3, $4
                  WHERE EXISTS (SELECT 1 FROM media_playback_pointers
                      WHERE user_id = $5 AND playback_id = $6
                        AND current_incarnation_id = $7)
                    AND NOT EXISTS (SELECT 1 FROM media_session_preparations
                      WHERE user_id = $5 AND playback_id = $6)
                    AND NOT EXISTS (SELECT 1 FROM media_session_preparations
                      WHERE staged_incarnation_id = $8)
                    AND NOT EXISTS (SELECT 1 FROM media_sessions WHERE incarnation_id = $8)
                    AND (SELECT COUNT(*) FROM media_sessions
                          WHERE user_id = $5 AND state IN ('starting', 'active')
                            AND lease_expires_at_ms > $4 AND incarnation_id != $8) < $9
                    AND (SELECT COUNT(*) FROM media_sessions
                          WHERE user_id = $5 AND incarnation_id != $8) < $10
                    AND (SELECT COUNT(*) FROM media_sessions
                          WHERE owner_node_id = $2 AND state = 'active'
                            AND lease_expires_at_ms > $4 AND incarnation_id != $8) < $11
                    AND NOT EXISTS (SELECT 1 FROM settings WHERE key = $12)
                 ON CONFLICT(resource) DO UPDATE SET
                    expires_at_ms = excluded.expires_at_ms,
                    revision = job_leases.revision + 1,
                    updated_at_ms = excluded.updated_at_ms
                  WHERE job_leases.owner_node_id = excluded.owner_node_id
                    AND job_leases.fence = 1 AND job_leases.expires_at_ms > $4
                    AND job_leases.revision < 9223372036854775807",
                params!(
                    prepare_lease_resource.as_str(),
                    preparation.owner_node_id.as_str(),
                    preparation.deadline_ms,
                    preparation.now_ms,
                    preparation.user_id,
                    preparation.playback_id.as_str(),
                    preparation.expected_predecessor_incarnation_id.as_str(),
                    preparation.incarnation_id.as_str(),
                    MAX_CURRENT_PER_USER,
                    MAX_SESSION_ROWS_PER_USER,
                    MAX_OWNED,
                    removed_owner_key.as_str()
                ),
            ),
            (
                "INSERT INTO media_sessions
                    (incarnation_id, session_id, user_id, playback_id, request_fingerprint,
                     owner_node_id, owner_epoch, lease_expires_at_ms, state, recipe_json,
                     response_json, produced_playable_through_ms, fetched_through_ms,
                     media_origin_ms, media_sequence, discontinuity_sequence,
                     publication_ready_at_ms, updated_at_ms)
                 SELECT $1, $2, $3, $4, $5, $6, 1, $7, 'active', $8, $9, 0, 0, $10, 0, 0, $11, $12
                  WHERE EXISTS (SELECT 1 FROM job_leases
                      WHERE resource = 'session:' || $1 AND owner_node_id = $6
                        AND fence = 1 AND expires_at_ms = $7 AND expires_at_ms > $12)
                    AND EXISTS (SELECT 1 FROM media_playback_pointers
                      WHERE user_id = $3 AND playback_id = $4
                        AND current_incarnation_id = $13)
                    AND NOT EXISTS (SELECT 1 FROM media_session_preparations
                      WHERE user_id = $3 AND playback_id = $4)
                    AND NOT EXISTS (SELECT 1 FROM media_session_preparations
                      WHERE staged_incarnation_id = $1)
                    AND (SELECT COUNT(*) FROM media_sessions
                          WHERE user_id = $3 AND state IN ('starting', 'active')
                            AND lease_expires_at_ms > $12 AND incarnation_id != $1) < $14
                    AND (SELECT COUNT(*) FROM media_sessions
                          WHERE user_id = $3 AND incarnation_id != $1) < $15
                    AND (SELECT COUNT(*) FROM media_sessions
                          WHERE owner_node_id = $6 AND state = 'active'
                            AND lease_expires_at_ms > $12 AND incarnation_id != $1) < $16
                    AND NOT EXISTS (SELECT 1 FROM settings WHERE key = $17)",
                params!(
                    preparation.incarnation_id.as_str(),
                    preparation.session_id.as_str(),
                    preparation.user_id,
                    preparation.playback_id.as_str(),
                    preparation.request_fingerprint.as_str(),
                    preparation.owner_node_id.as_str(),
                    preparation.deadline_ms,
                    preparation.recipe_json.as_str(),
                    preparation.response_json.as_str(),
                    preparation.media_origin_ms,
                    MEDIA_SESSION_PUBLICATION_BLOCKED,
                    preparation.now_ms,
                    preparation.expected_predecessor_incarnation_id.as_str(),
                    MAX_CURRENT_PER_USER,
                    MAX_SESSION_ROWS_PER_USER,
                    MAX_OWNED,
                    removed_owner_key.as_str()
                ),
            ),
            (
                // Guarded on the row above having landed, so the ledger can
                // never name an incarnation that does not exist.
                "INSERT INTO media_session_preparations
                    (user_id, playback_id, staged_incarnation_id,
                     expected_predecessor_incarnation_id, deadline_ms,
                     created_at_ms, updated_at_ms)
                 SELECT $1, $2, $3, $4, $5, $6, $6 WHERE EXISTS (
                   SELECT 1 FROM media_sessions
                    WHERE incarnation_id = $3 AND session_id = $7
                      AND user_id = $1 AND playback_id = $2
                      AND owner_node_id = $8 AND state = 'active'
                      AND publication_ready_at_ms = $9)",
                params!(
                    preparation.user_id,
                    preparation.playback_id.as_str(),
                    preparation.incarnation_id.as_str(),
                    preparation.expected_predecessor_incarnation_id.as_str(),
                    preparation.deadline_ms,
                    preparation.now_ms,
                    preparation.session_id.as_str(),
                    preparation.owner_node_id.as_str(),
                    MEDIA_SESSION_PUBLICATION_BLOCKED
                ),
            ),
        ];
        for (sql, _) in &statements {
            validate_sql(sql)?;
        }
        let changed = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        if changed.first().copied() != Some(1)
            || changed.get(1).copied() != Some(1)
            || changed.get(2).copied() != Some(1)
        {
            return Ok(None);
        }
        // The exact post-commit projection. `route_by` alone would accept a
        // row an unrelated writer produced, so the ledger is re-read too and
        // both have to agree on the same successor.
        let route = route_by(self, "incarnation_id", &preparation.incarnation_id)
            .await?
            .filter(|route| preparation_route_matches(route, preparation));
        let staged = staged_row(self, preparation.user_id, &preparation.playback_id).await?;
        let staged_is_ours = staged.map(|staged| staged.staged_incarnation_id).as_deref()
            == Some(preparation.incarnation_id.as_str());
        let Some(route) = route.filter(|_| staged_is_ours) else {
            // There is nothing to roll back — a replicated transaction that
            // commits has committed — so the cleanup is explicit. Without it a
            // caller told it lost would leave a live staged row holding the
            // one-per-playback slot and a slot of the user's admission cap,
            // with nobody left who would ever commit or abort it. The SQLite
            // twin reaches the same durable state by rolling back.
            if staged_is_ours {
                self.abort_media_session_preparation(
                    preparation.user_id,
                    &preparation.playback_id,
                    &preparation.incarnation_id,
                    preparation.now_ms,
                )
                .await?;
            }
            return Ok(None);
        };
        Ok(Some(route))
    }

    async fn staged_media_session_for_playback(
        &self,
        user_id: i64,
        playback_id: &str,
    ) -> Result<Option<crate::domain::MediaSessionStagedGeneration>, StoreError> {
        if user_id <= 0 || playback_id.is_empty() || playback_id.len() > 128 {
            return Err(StoreError::Task(
                "invalid staged media-session lookup".to_owned(),
            ));
        }
        staged_row(self, user_id, playback_id).await
    }

    async fn commit_media_session_preparation(
        &self,
        user_id: i64,
        playback_id: &str,
        staged_incarnation_id: &str,
        now_ms: i64,
        lease_expires_at_ms: i64,
    ) -> Result<Option<crate::domain::MediaSessionPreparationCommit>, StoreError> {
        if user_id <= 0
            || playback_id.is_empty()
            || playback_id.len() > 128
            || !valid_uuid(staged_incarnation_id)
            || now_ms <= 0
            || lease_expires_at_ms <= now_ms
        {
            return Err(StoreError::Task(
                "invalid media-session preparation commit".to_owned(),
            ));
        }
        let Some(staged) = staged_row(self, user_id, playback_id).await? else {
            // No ledger row. Either it was never staged, or an earlier commit
            // already consumed it — and the pointer is the discriminator.
            return commit_replay(self, user_id, playback_id, staged_incarnation_id).await;
        };
        if staged.staged_incarnation_id != staged_incarnation_id {
            // A later preparation holds the slot, which does not make this
            // commit a loss: if the pointer names the incarnation the caller
            // asked about, this exact commit already happened and the replay
            // must read back. Returning `None` here made a retry of a lost
            // response read as "you lost" on this backend and "here is your
            // route" on SQLite.
            return commit_replay(self, user_id, playback_id, staged_incarnation_id).await;
        }
        let predecessor_incarnation = staged.expected_predecessor_incarnation_id.clone();
        let lease_resource = format!("session:{predecessor_incarnation}");
        let staged_lease_resource = format!("session:{}", staged.staged_incarnation_id);
        // The pointer advance and the predecessor's retirement in one
        // transaction, both fenced on the exact recorded predecessor. Nothing
        // here reads the pointer to decide what to reap; a pointer that no
        // longer names the predecessor simply fails every guard, and the
        // abort branch below is what turns that into the right outcome.
        let statements: Vec<(&str, hiqlite::Params)> = vec![
            (
                "UPDATE media_playback_pointers
                    SET current_incarnation_id = $1, updated_at_ms = $2
                  WHERE user_id = $3 AND playback_id = $4
                    AND current_incarnation_id = $5
                    AND EXISTS (SELECT 1 FROM media_sessions
                      WHERE incarnation_id = $1 AND user_id = $3 AND playback_id = $4
                        AND state = 'active')",
                params!(
                    staged.staged_incarnation_id.as_str(),
                    now_ms,
                    user_id,
                    playback_id,
                    predecessor_incarnation.as_str()
                ),
            ),
            (
                "UPDATE media_sessions SET state = 'ended', terminal_reason = 'superseded',
                        lease_expires_at_ms = $1, publication_ready_at_ms = $2, updated_at_ms = $1
                  WHERE incarnation_id = $3 AND state != 'ended'
                    AND EXISTS (SELECT 1 FROM media_playback_pointers
                      WHERE user_id = $4 AND playback_id = $5
                        AND current_incarnation_id = $6)",
                params!(
                    now_ms,
                    MEDIA_SESSION_PUBLICATION_BLOCKED,
                    predecessor_incarnation.as_str(),
                    user_id,
                    playback_id,
                    staged.staged_incarnation_id.as_str()
                ),
            ),
            (
                "DELETE FROM cache_consumer_pins
                  WHERE consumer_kind = 'media_session' AND consumer_id = $1
                    AND EXISTS (SELECT 1 FROM media_sessions
                      WHERE incarnation_id = $1 AND state = 'ended' AND updated_at_ms = $2)",
                params!(predecessor_incarnation.as_str(), now_ms),
            ),
            (
                "UPDATE job_leases
                    SET expires_at_ms = CASE
                          WHEN expires_at_ms < $1 THEN expires_at_ms ELSE $1 END,
                        revision = revision + 1, updated_at_ms = $1
                  WHERE resource = $2 AND revision < 9223372036854775807
                    AND EXISTS (SELECT 1 FROM media_sessions
                      WHERE incarnation_id = $3 AND state = 'ended' AND updated_at_ms = $1)",
                params!(
                    now_ms,
                    lease_resource.as_str(),
                    predecessor_incarnation.as_str()
                ),
            ),
            (
                // The successor stops being a candidate and starts being a
                // stream, so it stops carrying the candidate's clock. Both
                // halves move: the row, and the `job_leases` fence renewal
                // reads. Without this it would be ended by maintenance at the
                // moment the preparation would have expired, taking the
                // playback's pointer with it.
                "UPDATE media_sessions
                    SET lease_expires_at_ms = $1, updated_at_ms = $2
                  WHERE incarnation_id = $3 AND state = 'active'
                    AND EXISTS (SELECT 1 FROM media_playback_pointers
                      WHERE user_id = $4 AND playback_id = $5
                        AND current_incarnation_id = $3)",
                params!(
                    lease_expires_at_ms,
                    now_ms,
                    staged.staged_incarnation_id.as_str(),
                    user_id,
                    playback_id
                ),
            ),
            (
                "UPDATE job_leases
                    SET expires_at_ms = $1, revision = revision + 1, updated_at_ms = $2
                  WHERE resource = $3 AND fence = 1
                    AND revision < 9223372036854775807
                    AND EXISTS (SELECT 1 FROM media_playback_pointers
                      WHERE user_id = $4 AND playback_id = $5
                        AND current_incarnation_id = $6)",
                params!(
                    lease_expires_at_ms,
                    now_ms,
                    staged_lease_resource.as_str(),
                    user_id,
                    playback_id,
                    staged.staged_incarnation_id.as_str()
                ),
            ),
            (
                "DELETE FROM media_session_preparations
                  WHERE user_id = $1 AND playback_id = $2 AND staged_incarnation_id = $3
                    AND EXISTS (SELECT 1 FROM media_playback_pointers
                      WHERE user_id = $1 AND playback_id = $2
                        AND current_incarnation_id = $3)",
                params!(user_id, playback_id, staged.staged_incarnation_id.as_str()),
            ),
        ];
        for (sql, _) in &statements {
            validate_sql(sql)?;
        }
        let statement_count = statements.len();
        let changed = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        let fresh_commit = changed.first().copied() == Some(1);
        // An all-zero replicated transaction is ambiguous between "I lost
        // every precondition" and "I am an exact replay", so the exact
        // post-commit projection below is the discriminator — never
        // rows_affected on its own.
        let transaction_replay =
            changed.len() == statement_count && changed.iter().all(|affected| *affected == 0);
        if !fresh_commit && !transaction_replay {
            return Ok(None);
        }
        let pointer = self
            .client()
            .query_consistent_map::<PointerRow, _>(
                "SELECT current_incarnation_id FROM media_playback_pointers
                  WHERE user_id = $1 AND playback_id = $2",
                params!(user_id, playback_id),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0);
        if pointer.as_deref() != Some(staged.staged_incarnation_id.as_str()) {
            // The pointer moved: a newer player generation is current. Abort
            // the staged successor rather than reap that generation — this is
            // the clause the whole predecessor-recording design exists for.
            self.abort_media_session_preparation(
                user_id,
                playback_id,
                &staged.staged_incarnation_id,
                now_ms,
            )
            .await?;
            return Ok(None);
        }
        let route = route_by(self, "incarnation_id", &staged.staged_incarnation_id)
            .await?
            .filter(|route| route.state == "active");
        let Some(route) = route else {
            return Ok(None);
        };
        let predecessor = route_by(self, "incarnation_id", &predecessor_incarnation).await?;
        Ok(Some(crate::domain::MediaSessionPreparationCommit {
            route,
            predecessor,
        }))
    }

    async fn abort_media_session_preparation(
        &self,
        user_id: i64,
        playback_id: &str,
        staged_incarnation_id: &str,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if user_id <= 0
            || playback_id.is_empty()
            || playback_id.len() > 128
            || !valid_uuid(staged_incarnation_id)
            || now_ms <= 0
        {
            return Err(StoreError::Task(
                "invalid media-session preparation abort".to_owned(),
            ));
        }
        let lease_resource = format!("session:{staged_incarnation_id}");
        // Ledger-scoped in every statement, which is the safety property:
        // without a ledger row naming it, this cannot end an incarnation. An
        // abort aimed at a successor that already committed ends nothing.
        let statements: Vec<(&str, hiqlite::Params)> = vec![
            (
                "UPDATE media_sessions SET state = 'ended', terminal_reason = 'replaced',
                        lease_expires_at_ms = $1, updated_at_ms = $1
                  WHERE incarnation_id = $2 AND state != 'ended'
                    AND EXISTS (SELECT 1 FROM media_session_preparations
                      WHERE user_id = $3 AND playback_id = $4
                        AND staged_incarnation_id = $2)",
                params!(now_ms, staged_incarnation_id, user_id, playback_id),
            ),
            (
                "DELETE FROM cache_consumer_pins
                  WHERE consumer_kind = 'media_session' AND consumer_id = $1
                    AND EXISTS (SELECT 1 FROM media_sessions
                      WHERE incarnation_id = $1 AND state = 'ended' AND updated_at_ms = $2)
                    AND EXISTS (SELECT 1 FROM media_session_preparations
                      WHERE user_id = $3 AND playback_id = $4
                        AND staged_incarnation_id = $1)",
                params!(staged_incarnation_id, now_ms, user_id, playback_id),
            ),
            (
                "UPDATE job_leases
                    SET expires_at_ms = CASE
                          WHEN expires_at_ms < $1 THEN expires_at_ms ELSE $1 END,
                        revision = revision + 1, updated_at_ms = $1
                  WHERE resource = $2 AND revision < 9223372036854775807
                    AND EXISTS (SELECT 1 FROM media_sessions
                      WHERE incarnation_id = $3 AND state = 'ended' AND updated_at_ms = $1)
                    AND EXISTS (SELECT 1 FROM media_session_preparations
                      WHERE user_id = $4 AND playback_id = $5
                        AND staged_incarnation_id = $3)",
                params!(
                    now_ms,
                    lease_resource.as_str(),
                    staged_incarnation_id,
                    user_id,
                    playback_id
                ),
            ),
            (
                "DELETE FROM media_session_preparations
                  WHERE user_id = $1 AND playback_id = $2 AND staged_incarnation_id = $3",
                params!(user_id, playback_id, staged_incarnation_id),
            ),
        ];
        for (sql, _) in &statements {
            validate_sql(sql)?;
        }
        self.client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        // The projection, not the row counts: an abort that finds the staged
        // row already ended and its ledger row already gone is an idempotent
        // replay and must read the same as the first one.
        if staged_row(self, user_id, playback_id)
            .await?
            .is_some_and(|staged| staged.staged_incarnation_id == staged_incarnation_id)
        {
            return Ok(None);
        }
        // The same predicate the SQLite twin uses, and it has to be: this
        // reports success only for a row that is ended, carries the abort's
        // own terminal cause, and belongs to the playback the caller named.
        Ok(route_by(self, "incarnation_id", staged_incarnation_id)
            .await?
            .filter(|route| {
                route.state == "ended"
                    && route.terminal_reason.as_deref() == Some("replaced")
                    && route.user_id == user_id
                    && route.playback_id == playback_id
            }))
    }

    async fn settle_media_session_activation(
        &self,
        activation: &MediaSessionActivation,
        settlement: MediaSessionActivationSettlement,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        validate_activation(activation)?;
        if now_ms <= 0 {
            return Err(StoreError::Task(
                "invalid media-session activation settlement time".to_owned(),
            ));
        }
        let request_id = activation.request_id.as_deref().unwrap_or("");
        let lease_resource = format!("session:{}", activation.incarnation_id);
        match settlement {
            MediaSessionActivationSettlement::Confirm {
                publication_ready_at_ms,
            } => {
                let valid_publication = if activation.expected_predecessor_incarnation_id.is_some()
                {
                    publication_ready_at_ms
                        >= now_ms.saturating_add(MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS)
                        && publication_ready_at_ms != MEDIA_SESSION_PUBLICATION_BLOCKED
                } else {
                    publication_ready_at_ms == 0
                };
                if !valid_publication {
                    return Err(StoreError::Task(
                        "invalid media-session activation confirmation".to_owned(),
                    ));
                }
                let changed = self
                    .client()
                    .txn([(
                        "UPDATE media_sessions SET publication_ready_at_ms = $1,
                                    updated_at_ms = $2
                              WHERE incarnation_id = $3 AND session_id = $4 AND user_id = $5
                                AND playback_id = $6 AND request_fingerprint = $7
                                AND owner_node_id = $8 AND owner_epoch = 1 AND state = 'active'
                                AND recipe_json = $9 AND response_json = $10
                                AND media_origin_ms = $11
                                AND publication_ready_at_ms = $12
                                AND lease_expires_at_ms = $13 AND lease_expires_at_ms > $2
                                AND EXISTS (SELECT 1 FROM media_playback_pointers
                                  WHERE user_id = $5 AND playback_id = $6
                                    AND current_incarnation_id = $3)
                                AND ($14 = '' OR EXISTS (SELECT 1
                                  FROM media_session_requests
                                  WHERE user_id = $5 AND request_id = $14
                                    AND incarnation_id = $3 AND request_fingerprint = $7
                                    AND playback_id = $6 AND owner_node_id = $8
                                    AND state = 'starting'))",
                        params!(
                            publication_ready_at_ms,
                            now_ms,
                            activation.incarnation_id.as_str(),
                            activation.session_id.as_str(),
                            activation.user_id,
                            activation.playback_id.as_str(),
                            activation.request_fingerprint.as_str(),
                            activation.owner_node_id.as_str(),
                            activation.recipe_json.as_str(),
                            activation.response_json.as_str(),
                            activation.media_origin_ms,
                            MEDIA_SESSION_PUBLICATION_BLOCKED,
                            activation.lease_expires_at_ms,
                            request_id
                        ),
                    )])
                    .await?
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(database_error)?;
                let route = route_by(self, "incarnation_id", &activation.incarnation_id)
                    .await?
                    .filter(|route| {
                        activation_route_matches(route, activation)
                            && route.publication_ready_at_ms != MEDIA_SESSION_PUBLICATION_BLOCKED
                    });
                let committed_pointer = self
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
                if matches!(changed.as_slice(), [1] | [0])
                    && committed_pointer.as_deref() == Some(activation.incarnation_id.as_str())
                {
                    Ok(route)
                } else {
                    Ok(None)
                }
            }
            MediaSessionActivationSettlement::Abandon => {
                self.client()
                    .txn([
                        (
                            "UPDATE media_session_requests SET state = 'failed',
                                    response_json = NULL, claim_expires_at_ms = $1,
                                    updated_at_ms = $1
                              WHERE $2 != '' AND user_id = $3 AND request_id = $2
                                AND incarnation_id = $4 AND request_fingerprint = $5
                                AND playback_id = $6 AND owner_node_id = $7
                                AND state = 'starting'",
                            params!(
                                now_ms,
                                request_id,
                                activation.user_id,
                                activation.incarnation_id.as_str(),
                                activation.request_fingerprint.as_str(),
                                activation.playback_id.as_str(),
                                activation.owner_node_id.as_str()
                            ),
                        ),
                        (
                            "UPDATE media_sessions SET state = 'ended',
                                    terminal_reason = 'replaced', lease_expires_at_ms = $1,
                                    publication_ready_at_ms = $2, updated_at_ms = $1
                              WHERE incarnation_id = $3 AND session_id = $4 AND user_id = $5
                                AND playback_id = $6 AND request_fingerprint = $7
                                AND owner_node_id = $8 AND owner_epoch = 1
                                AND state = 'active' AND recipe_json = $9
                                AND response_json = $10 AND media_origin_ms = $11
                                AND ($12 = '' AND publication_ready_at_ms = $2
                                  OR $12 != '' AND EXISTS (SELECT 1
                                  FROM media_session_requests WHERE user_id = $5
                                    AND request_id = $12 AND incarnation_id = $3
                                    AND request_fingerprint = $7 AND playback_id = $6
                                    AND owner_node_id = $8 AND state IN ('failed', 'starting')))",
                            params!(
                                now_ms,
                                MEDIA_SESSION_PUBLICATION_BLOCKED,
                                activation.incarnation_id.as_str(),
                                activation.session_id.as_str(),
                                activation.user_id,
                                activation.playback_id.as_str(),
                                activation.request_fingerprint.as_str(),
                                activation.owner_node_id.as_str(),
                                activation.recipe_json.as_str(),
                                activation.response_json.as_str(),
                                activation.media_origin_ms,
                                request_id
                            ),
                        ),
                        (
                            "DELETE FROM media_playback_pointers
                              WHERE user_id = $1 AND playback_id = $2
                                AND current_incarnation_id = $3
                                AND EXISTS (SELECT 1 FROM media_sessions
                                  WHERE incarnation_id = $3 AND state = 'ended'
                                    AND terminal_reason = 'replaced' AND updated_at_ms = $4)",
                            params!(
                                activation.user_id,
                                activation.playback_id.as_str(),
                                activation.incarnation_id.as_str(),
                                now_ms
                            ),
                        ),
                        (
                            "UPDATE job_leases SET expires_at_ms = $1,
                                    revision = revision + 1, updated_at_ms = $1
                              WHERE resource = $2 AND owner_node_id = $3 AND fence = 1
                                AND revision < 9223372036854775807
                                AND EXISTS (SELECT 1 FROM media_sessions
                                  WHERE incarnation_id = $4 AND state = 'ended'
                                    AND terminal_reason = 'replaced' AND updated_at_ms = $1)",
                            params!(
                                now_ms,
                                lease_resource.as_str(),
                                activation.owner_node_id.as_str(),
                                activation.incarnation_id.as_str()
                            ),
                        ),
                        (
                            "DELETE FROM cache_consumer_pins
                              WHERE consumer_kind = 'media_session' AND consumer_id = $1
                                AND EXISTS (SELECT 1 FROM media_sessions
                                  WHERE incarnation_id = $1 AND state = 'ended'
                                    AND terminal_reason = 'replaced' AND updated_at_ms = $2)",
                            params!(activation.incarnation_id.as_str(), now_ms),
                        ),
                    ])
                    .await?
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(database_error)?;
                let route = route_by(self, "incarnation_id", &activation.incarnation_id)
                    .await?
                    .filter(|route| activation_route_matches(route, activation));
                let Some(route) = route else {
                    return Ok(None);
                };
                if activation.request_id.is_none() {
                    return Ok((route.publication_ready_at_ms
                        != MEDIA_SESSION_PUBLICATION_BLOCKED)
                        .then_some(route));
                }
                let request = request_row(
                    self,
                    activation.user_id,
                    activation.request_id.as_deref().unwrap_or_default(),
                )
                .await?;
                let resolved = request.is_some_and(|request| {
                    request.state == "resolved"
                        && request.incarnation_id == activation.incarnation_id
                        && request.request_fingerprint == activation.request_fingerprint
                        && request.playback_id == activation.playback_id
                        && request.owner_node_id.as_deref()
                            == Some(activation.owner_node_id.as_str())
                });
                Ok(
                    (resolved
                        && route.publication_ready_at_ms != MEDIA_SESSION_PUBLICATION_BLOCKED)
                        .then_some(route),
                )
            }
        }
    }

    async fn publish_media_session_activation(
        &self,
        user_id: i64,
        request_id: &str,
        incarnation_id: &str,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if user_id <= 0
            || request_id.is_empty()
            || request_id.len() > 128
            || !valid_uuid(incarnation_id)
            || now_ms <= 0
        {
            return Err(StoreError::Task(
                "invalid media-session activation publication".to_owned(),
            ));
        }
        self.client()
            .txn([(
                "UPDATE media_session_requests SET state = 'resolved',
                        response_json = (SELECT route.response_json FROM media_sessions route
                          WHERE route.user_id = $1 AND $2 != '' AND route.incarnation_id = $3
                            AND route.state = 'active' AND route.publication_ready_at_ms = 0
                            AND route.lease_expires_at_ms > $4
                            AND route.request_fingerprint = media_session_requests.request_fingerprint
                            AND route.playback_id = media_session_requests.playback_id
                            AND route.owner_node_id = media_session_requests.owner_node_id),
                        claim_expires_at_ms = (SELECT route.lease_expires_at_ms FROM media_sessions route
                          WHERE route.user_id = $1 AND route.incarnation_id = $3
                            AND route.state = 'active' AND route.publication_ready_at_ms = 0
                            AND route.lease_expires_at_ms > $4
                            AND route.request_fingerprint = media_session_requests.request_fingerprint
                            AND route.playback_id = media_session_requests.playback_id
                            AND route.owner_node_id = media_session_requests.owner_node_id),
                        updated_at_ms = $4
                  WHERE user_id = $1 AND request_id = $2 AND incarnation_id = $3
                    AND state = 'starting'
                    AND EXISTS (SELECT 1 FROM media_sessions route
                      WHERE route.user_id = $1 AND route.incarnation_id = $3
                        AND route.state = 'active' AND route.publication_ready_at_ms = 0
                        AND route.lease_expires_at_ms > $4
                        AND route.request_fingerprint = media_session_requests.request_fingerprint
                        AND route.playback_id = media_session_requests.playback_id
                        AND route.owner_node_id = media_session_requests.owner_node_id)",
                params!(user_id, request_id, incarnation_id, now_ms),
            )])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        let route = route_by(self, "incarnation_id", incarnation_id)
            .await?
            .filter(|route| {
                route.user_id == user_id
                    && route.state == "active"
                    && route.publication_ready_at_ms == 0
            });
        let Some(route) = route else {
            return Ok(None);
        };
        let resolved = request_row(self, user_id, request_id)
            .await?
            .is_some_and(|request| {
                request.state == "resolved"
                    && request.incarnation_id == incarnation_id
                    && request.request_fingerprint == route.request_fingerprint
                    && request.playback_id == route.playback_id
            });
        Ok(resolved.then_some(route))
    }

    async fn complete_media_session_handoff(
        &self,
        incarnation_id: &str,
        owner_node_id: &str,
        owner_epoch: i64,
        proof: MediaSessionProjectionCompletion,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if !valid_uuid(incarnation_id)
            || owner_node_id.is_empty()
            || owner_node_id.len() > 256
            || owner_epoch <= 0
            || now_ms <= 0
        {
            return Err(StoreError::Task(
                "invalid media-session handoff completion".to_owned(),
            ));
        }
        let (statement, expected_not_before_ms) = match proof {
            MediaSessionProjectionCompletion::PredecessorAcknowledged => (
                "UPDATE media_sessions SET publication_ready_at_ms = 0, updated_at_ms = $1
                  WHERE incarnation_id = $2 AND owner_node_id = $3 AND owner_epoch = $4
                    AND state = 'active' AND publication_ready_at_ms != 0",
                0,
            ),
            MediaSessionProjectionCompletion::SafetyBoundaryElapsed {
                expected_not_before_ms,
            } if expected_not_before_ms > 0
                && expected_not_before_ms != MEDIA_SESSION_PUBLICATION_BLOCKED
                && expected_not_before_ms <= now_ms =>
            {
                (
                    "UPDATE media_sessions SET publication_ready_at_ms = 0, updated_at_ms = $1
                      WHERE incarnation_id = $2 AND owner_node_id = $3 AND owner_epoch = $4
                        AND state = 'active' AND publication_ready_at_ms = $5",
                    expected_not_before_ms,
                )
            }
            MediaSessionProjectionCompletion::SafetyBoundaryElapsed { .. } => {
                return Err(StoreError::Task(
                    "invalid media-session handoff safety proof".to_owned(),
                ));
            }
        };
        if expected_not_before_ms == 0 {
            self.client()
                .execute(
                    statement,
                    params!(now_ms, incarnation_id, owner_node_id, owner_epoch),
                )
                .await?;
        } else {
            self.client()
                .execute(
                    statement,
                    params!(
                        now_ms,
                        incarnation_id,
                        owner_node_id,
                        owner_epoch,
                        expected_not_before_ms
                    ),
                )
                .await?;
        }
        Ok(route_by(self, "incarnation_id", incarnation_id)
            .await?
            .filter(|route| {
                route.owner_node_id == owner_node_id
                    && route.owner_epoch == owner_epoch
                    && route.state == "active"
                    && route.publication_ready_at_ms == 0
            }))
    }

    async fn arm_media_session_handoff(
        &self,
        incarnation_id: &str,
        owner_node_id: &str,
        owner_epoch: i64,
        publication_ready_at_ms: i64,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if !valid_uuid(incarnation_id)
            || owner_node_id.is_empty()
            || owner_node_id.len() > 256
            || owner_epoch <= 0
            || publication_ready_at_ms
                < now_ms.saturating_add(MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS)
            || publication_ready_at_ms == MEDIA_SESSION_PUBLICATION_BLOCKED
            || now_ms <= 0
        {
            return Err(StoreError::Task(
                "invalid media-session handoff arming".to_owned(),
            ));
        }
        self.client()
            .execute(
                "UPDATE media_sessions SET publication_ready_at_ms = $1, updated_at_ms = $2
                  WHERE incarnation_id = $3 AND owner_node_id = $4 AND owner_epoch = $5
                    AND state = 'active' AND publication_ready_at_ms = $6",
                params!(
                    publication_ready_at_ms,
                    now_ms,
                    incarnation_id,
                    owner_node_id,
                    owner_epoch,
                    MEDIA_SESSION_PUBLICATION_BLOCKED
                ),
            )
            .await?;
        Ok(route_by(self, "incarnation_id", incarnation_id)
            .await?
            .filter(|route| {
                route.owner_node_id == owner_node_id
                    && route.owner_epoch == owner_epoch
                    && route.state == "active"
                    && route.publication_ready_at_ms != MEDIA_SESSION_PUBLICATION_BLOCKED
            }))
    }

    async fn arm_media_session_terminal_projection(
        &self,
        incarnation_id: &str,
        owner_node_id: &str,
        owner_epoch: i64,
        projection_safe_at_ms: i64,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if !valid_uuid(incarnation_id)
            || owner_node_id.is_empty()
            || owner_node_id.len() > 256
            || owner_epoch <= 0
            || projection_safe_at_ms < now_ms.saturating_add(MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS)
            || projection_safe_at_ms == MEDIA_SESSION_PUBLICATION_BLOCKED
            || now_ms <= 0
        {
            return Err(StoreError::Task(
                "invalid media-session terminal projection arming".to_owned(),
            ));
        }
        self.client()
            .execute(
                "UPDATE media_sessions SET publication_ready_at_ms = $1, updated_at_ms = $2
                  WHERE incarnation_id = $3 AND owner_node_id = $4 AND owner_epoch = $5
                    AND state = 'ended' AND publication_ready_at_ms = $6",
                params!(
                    projection_safe_at_ms,
                    now_ms,
                    incarnation_id,
                    owner_node_id,
                    owner_epoch,
                    MEDIA_SESSION_PUBLICATION_BLOCKED
                ),
            )
            .await?;
        Ok(route_by(self, "incarnation_id", incarnation_id)
            .await?
            .filter(|route| {
                route.owner_node_id == owner_node_id
                    && route.owner_epoch == owner_epoch
                    && route.state == "ended"
                    && route.publication_ready_at_ms != MEDIA_SESSION_PUBLICATION_BLOCKED
            }))
    }

    async fn complete_media_session_terminal_projection(
        &self,
        incarnation_id: &str,
        owner_node_id: &str,
        owner_epoch: i64,
        proof: MediaSessionProjectionCompletion,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if !valid_uuid(incarnation_id)
            || owner_node_id.is_empty()
            || owner_node_id.len() > 256
            || owner_epoch <= 0
            || now_ms <= 0
        {
            return Err(StoreError::Task(
                "invalid media-session terminal projection completion".to_owned(),
            ));
        }
        let (statement, expected_not_before_ms) = match proof {
            MediaSessionProjectionCompletion::PredecessorAcknowledged => (
                "UPDATE media_sessions SET publication_ready_at_ms = 0, updated_at_ms = $1
                  WHERE incarnation_id = $2 AND owner_node_id = $3 AND owner_epoch = $4
                    AND state = 'ended' AND publication_ready_at_ms != 0",
                0,
            ),
            MediaSessionProjectionCompletion::SafetyBoundaryElapsed {
                expected_not_before_ms,
            } if expected_not_before_ms > 0
                && expected_not_before_ms != MEDIA_SESSION_PUBLICATION_BLOCKED
                && expected_not_before_ms <= now_ms =>
            {
                (
                    "UPDATE media_sessions SET publication_ready_at_ms = 0, updated_at_ms = $1
                      WHERE incarnation_id = $2 AND owner_node_id = $3 AND owner_epoch = $4
                        AND state = 'ended' AND publication_ready_at_ms = $5",
                    expected_not_before_ms,
                )
            }
            MediaSessionProjectionCompletion::SafetyBoundaryElapsed { .. } => {
                return Err(StoreError::Task(
                    "invalid media-session terminal projection safety proof".to_owned(),
                ));
            }
        };
        if expected_not_before_ms == 0 {
            self.client()
                .execute(
                    statement,
                    params!(now_ms, incarnation_id, owner_node_id, owner_epoch),
                )
                .await?;
        } else {
            self.client()
                .execute(
                    statement,
                    params!(
                        now_ms,
                        incarnation_id,
                        owner_node_id,
                        owner_epoch,
                        expected_not_before_ms
                    ),
                )
                .await?;
        }
        Ok(route_by(self, "incarnation_id", incarnation_id)
            .await?
            .filter(|route| {
                route.owner_node_id == owner_node_id
                    && route.owner_epoch == owner_epoch
                    && route.state == "ended"
                    && route.publication_ready_at_ms == 0
            }))
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

    async fn media_session_route_for_playback(
        &self,
        user_id: i64,
        playback_id: &str,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if user_id <= 0
            || playback_id.is_empty()
            || playback_id.len() > 128
            || playback_id
                .bytes()
                .any(|byte| matches!(byte, b'\r' | b'\n' | 0))
        {
            return Err(StoreError::Task(
                "invalid media-session playback route".to_owned(),
            ));
        }
        let incarnation = self
            .client()
            .query_consistent_map::<PointerRow, _>(
                "SELECT current_incarnation_id FROM media_playback_pointers
                  WHERE user_id = $1 AND playback_id = $2",
                params!(user_id, playback_id),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0);
        match incarnation {
            Some(incarnation) => route_by(self, "incarnation_id", &incarnation).await,
            None => Ok(None),
        }
    }

    async fn record_media_session_terminal_ack(
        &self,
        acknowledgement: &MediaSessionTerminalAck,
    ) -> Result<bool, StoreError> {
        if !valid_terminal_ack(acknowledgement) {
            return Err(StoreError::Task(
                "invalid media-session terminal acknowledgement".to_owned(),
            ));
        }
        let lease_resource = format!("session:{}", acknowledgement.incarnation_id);
        self.client()
            .txn([
                (
                    "INSERT INTO media_session_terminal_acks
                    (incarnation_id, session_id, owner_node_id, owner_epoch,
                     client_instance_id, sequence, request_fingerprint, response_json,
                     expires_at_ms, updated_at_ms)
                 SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10
                   WHERE EXISTS (SELECT 1 FROM media_sessions
                     WHERE incarnation_id = $1 AND session_id = $2
                       AND owner_node_id = $3 AND owner_epoch = $4
                       AND state IN ('active', 'ended'))
                 ON CONFLICT(session_id) DO NOTHING",
                    params!(
                        acknowledgement.incarnation_id.as_str(),
                        acknowledgement.session_id.as_str(),
                        acknowledgement.owner_node_id.as_str(),
                        acknowledgement.owner_epoch,
                        acknowledgement.client_instance_id.as_str(),
                        acknowledgement.sequence,
                        acknowledgement.request_fingerprint.as_str(),
                        acknowledgement.response_json.as_str(),
                        acknowledgement.expires_at_ms,
                        acknowledgement.updated_at_ms
                    ),
                ),
                (
                    "UPDATE media_sessions SET state = 'ended',
                            terminal_reason = COALESCE(terminal_reason, 'deleted'), lease_expires_at_ms = $1,
                            publication_ready_at_ms = 0, updated_at_ms = $1
                      WHERE incarnation_id = $2 AND session_id = $3
                        AND owner_node_id = $4 AND owner_epoch = $5
                        AND state IN ('active', 'ended')
                        AND EXISTS (SELECT 1 FROM media_session_terminal_acks
                          WHERE session_id = $3 AND incarnation_id = $2
                            AND owner_node_id = $4 AND owner_epoch = $5
                            AND client_instance_id = $6 AND sequence = $7
                            AND request_fingerprint = $8 AND response_json = $9
                            AND expires_at_ms = $10 AND updated_at_ms = $1)",
                    params!(
                        acknowledgement.updated_at_ms,
                        acknowledgement.incarnation_id.as_str(),
                        acknowledgement.session_id.as_str(),
                        acknowledgement.owner_node_id.as_str(),
                        acknowledgement.owner_epoch,
                        acknowledgement.client_instance_id.as_str(),
                        acknowledgement.sequence,
                        acknowledgement.request_fingerprint.as_str(),
                        acknowledgement.response_json.as_str(),
                        acknowledgement.expires_at_ms
                    ),
                ),
                (
                    "UPDATE job_leases
                        SET expires_at_ms = CASE
                              WHEN expires_at_ms < $1 THEN expires_at_ms ELSE $1 END,
                            revision = revision + 1, updated_at_ms = $1
                      WHERE resource = $2 AND owner_node_id = $3 AND fence = $4
                        AND revision < 9223372036854775807
                        AND expires_at_ms > $1
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = $5 AND session_id = $6
                            AND owner_node_id = $3 AND owner_epoch = $4
                            AND state = 'ended' AND updated_at_ms = $1)",
                    params!(
                        acknowledgement.updated_at_ms,
                        lease_resource.as_str(),
                        acknowledgement.owner_node_id.as_str(),
                        acknowledgement.owner_epoch,
                        acknowledgement.incarnation_id.as_str(),
                        acknowledgement.session_id.as_str()
                    ),
                ),
                (
                    "DELETE FROM media_playback_pointers
                      WHERE current_incarnation_id = $1
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = $1 AND session_id = $2
                            AND owner_node_id = $3 AND owner_epoch = $4
                            AND state = 'ended' AND updated_at_ms = $5)",
                    params!(
                        acknowledgement.incarnation_id.as_str(),
                        acknowledgement.session_id.as_str(),
                        acknowledgement.owner_node_id.as_str(),
                        acknowledgement.owner_epoch,
                        acknowledgement.updated_at_ms
                    ),
                ),
                (
                    "DELETE FROM cache_consumer_pins
                      WHERE consumer_kind = 'media_session' AND consumer_id = $1
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = $1 AND session_id = $2
                            AND owner_node_id = $3 AND owner_epoch = $4
                            AND state = 'ended' AND updated_at_ms = $5)",
                    params!(
                        acknowledgement.incarnation_id.as_str(),
                        acknowledgement.session_id.as_str(),
                        acknowledgement.owner_node_id.as_str(),
                        acknowledgement.owner_epoch,
                        acknowledgement.updated_at_ms
                    ),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        let stored = self
            .client()
            .query_consistent_map::<TerminalAckRow, _>(
                "SELECT incarnation_id, session_id, owner_node_id, owner_epoch,
                        client_instance_id, sequence, request_fingerprint, response_json,
                        expires_at_ms, updated_at_ms
                   FROM media_session_terminal_acks WHERE session_id = $1",
                params!(acknowledgement.session_id.as_str()),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0);
        let ended = route_by(self, "session_id", &acknowledgement.session_id)
            .await?
            .is_some_and(|route| {
                route.incarnation_id == acknowledgement.incarnation_id
                    && route.owner_node_id == acknowledgement.owner_node_id
                    && route.owner_epoch == acknowledgement.owner_epoch
                    && route.state == "ended"
                    && route.updated_at_ms == acknowledgement.updated_at_ms
            });
        Ok(stored.as_ref() == Some(acknowledgement) && ended)
    }

    async fn media_session_terminal_ack(
        &self,
        session_id: &str,
        now_ms: i64,
    ) -> Result<Option<MediaSessionTerminalAck>, StoreError> {
        if !valid_uuid(session_id) || now_ms <= 0 {
            return Ok(None);
        }
        Ok(self
            .client()
            .query_consistent_map::<TerminalAckRow, _>(
                "SELECT incarnation_id, session_id, owner_node_id, owner_epoch,
                        client_instance_id, sequence, request_fingerprint, response_json,
                        expires_at_ms, updated_at_ms
                   FROM media_session_terminal_acks
                  WHERE session_id = $1 AND expires_at_ms > $2",
                params!(session_id, now_ms),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0))
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
                        AND state = 'active' AND lease_expires_at_ms > $2
                        AND publication_ready_at_ms != $8
                        AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                          WHERE request.user_id = media_sessions.user_id
                            AND request.incarnation_id = media_sessions.incarnation_id
                            AND request.state = 'starting'
                            AND media_sessions.publication_ready_at_ms = 0
                            AND request.claim_expires_at_ms <= media_sessions.lease_expires_at_ms))",
                params!(
                    lease_expires_at_ms,
                    now_ms,
                    lease_resource.as_str(),
                    owner_node_id,
                    renewal.owner_epoch,
                    owner_fence_key.as_str(),
                    renewal.incarnation_id.as_str(),
                    MEDIA_SESSION_PUBLICATION_BLOCKED
                ),
            ));
            statements.push((
                // SQLite resolves `$N` by first appearance, not by the
                // number, so the frontier columns must be numbered where they
                // are written or every value in this statement shifts.
                "UPDATE media_sessions SET lease_expires_at_ms = $1, updated_at_ms = $2,
                        produced_playable_through_ms = MAX(
                          produced_playable_through_ms, $3),
                        fetched_through_ms = MAX(fetched_through_ms, $4),
                        media_sequence = MAX(media_sequence, $5)
                      WHERE incarnation_id = $6 AND owner_node_id = $7 AND owner_epoch = $8
                        AND state = 'active' AND lease_expires_at_ms > $2
                        AND publication_ready_at_ms != $9
                        AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                          WHERE request.user_id = media_sessions.user_id
                            AND request.incarnation_id = media_sessions.incarnation_id
                            AND request.state = 'starting'
                            AND media_sessions.publication_ready_at_ms = 0
                            AND request.claim_expires_at_ms <= media_sessions.lease_expires_at_ms)
                        AND EXISTS (SELECT 1 FROM job_leases
                          WHERE resource = $10 AND owner_node_id = $7 AND fence = $8
                            AND expires_at_ms = $1)",
                params!(
                    lease_expires_at_ms,
                    now_ms,
                    renewal.produced_playable_through_ms,
                    renewal.fetched_through_ms,
                    renewal.media_sequence,
                    renewal.incarnation_id.as_str(),
                    owner_node_id,
                    renewal.owner_epoch,
                    MEDIA_SESSION_PUBLICATION_BLOCKED,
                    lease_resource.as_str()
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
                           AND session.lease_expires_at_ms = $1
                           AND session.publication_ready_at_ms != $6
                           AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                             WHERE request.user_id = session.user_id
                               AND request.incarnation_id = session.incarnation_id
                               AND request.state = 'starting'
                               AND session.publication_ready_at_ms = 0
                               AND request.claim_expires_at_ms <= session.lease_expires_at_ms))",
                params!(
                    lease_expires_at_ms,
                    renewal.incarnation_id.as_str(),
                    renewal.owner_epoch,
                    now_ms,
                    owner_node_id,
                    MEDIA_SESSION_PUBLICATION_BLOCKED
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
        after: Option<MediaSessionTakeoverCursor>,
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
        let (has_cursor, after_lease_expires_at_ms, after_incarnation_id) = match after {
            Some(after) => {
                if after.lease_expires_at_ms < 0 || after.incarnation_id.is_empty() {
                    return Err(StoreError::Task(
                        "invalid media-session takeover inventory cursor".to_owned(),
                    ));
                }
                (1_i64, after.lease_expires_at_ms, after.incarnation_id)
            }
            None => (0_i64, 0_i64, String::new()),
        };
        let sql = format!(
            "SELECT {ROUTE_COLS} FROM media_sessions
              WHERE state = 'active' AND lease_expires_at_ms <= $1
                AND publication_ready_at_ms != $2
                AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                  WHERE request.user_id = media_sessions.user_id
                    AND request.incarnation_id = media_sessions.incarnation_id
                    AND request.state = 'starting'
                    AND media_sessions.publication_ready_at_ms = 0
                    AND request.claim_expires_at_ms <= media_sessions.lease_expires_at_ms)
                AND ($3 = 0 OR lease_expires_at_ms > $4
                  OR (lease_expires_at_ms = $4 AND incarnation_id > $5))
              ORDER BY lease_expires_at_ms, incarnation_id LIMIT $6"
        );
        validate_sql(&sql)?;
        Ok(self
            .client()
            .query_consistent_map::<RouteRow, _>(
                sql,
                params!(
                    now_ms,
                    MEDIA_SESSION_PUBLICATION_BLOCKED,
                    has_cursor,
                    after_lease_expires_at_ms,
                    after_incarnation_id,
                    limit
                ),
            )
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
                            AND publication_ready_at_ms != $10
                            AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                              WHERE request.user_id = media_sessions.user_id
                                AND request.incarnation_id = media_sessions.incarnation_id
                                AND request.state = 'starting'
                                AND media_sessions.publication_ready_at_ms = 0
                                AND request.claim_expires_at_ms <= media_sessions.lease_expires_at_ms)
                            AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                              WHERE request.user_id = media_sessions.user_id
                                AND request.incarnation_id = media_sessions.incarnation_id
                                AND request.state = 'starting'
                                AND (request.owner_node_id IS NULL
                                  OR request.owner_node_id != $6))
                            AND (SELECT COUNT(*) FROM media_session_requests request
                              WHERE request.user_id = media_sessions.user_id
                                AND request.incarnation_id = media_sessions.incarnation_id
                                AND request.state = 'starting') <= 1
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
                        takeover.incarnation_id.as_str(),
                        MEDIA_SESSION_PUBLICATION_BLOCKED
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
                        AND publication_ready_at_ms != $8
                        AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                          WHERE request.user_id = media_sessions.user_id
                            AND request.incarnation_id = media_sessions.incarnation_id
                            AND request.state = 'starting'
                            AND media_sessions.publication_ready_at_ms = 0
                            AND request.claim_expires_at_ms <= media_sessions.lease_expires_at_ms)
                        AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                          WHERE request.user_id = media_sessions.user_id
                            AND request.incarnation_id = media_sessions.incarnation_id
                            AND request.state = 'starting'
                            AND (request.owner_node_id IS NULL
                              OR request.owner_node_id != $6))
                        AND (SELECT COUNT(*) FROM media_session_requests request
                          WHERE request.user_id = media_sessions.user_id
                            AND request.incarnation_id = media_sessions.incarnation_id
                            AND request.state = 'starting') <= 1
                        AND discontinuity_sequence < 9223372036854775807
                        AND EXISTS (SELECT 1 FROM job_leases
                          WHERE resource = $9 AND owner_node_id = $1 AND fence = $2
                            AND expires_at_ms = $3 AND updated_at_ms = $4)",
                    params!(
                        takeover.next_owner_node_id.as_str(),
                        next_epoch,
                        takeover.lease_expires_at_ms,
                        takeover.now_ms,
                        takeover.incarnation_id.as_str(),
                        takeover.expected_owner_node_id.as_str(),
                        takeover.expected_owner_epoch,
                        MEDIA_SESSION_PUBLICATION_BLOCKED,
                        lease_resource.as_str()
                    ),
                ),
                (
                    "UPDATE media_session_requests
                        SET owner_node_id = $1, updated_at_ms = $2
                      WHERE incarnation_id = $3 AND owner_node_id = $4
                        AND state = 'starting'
                        AND EXISTS (SELECT 1 FROM media_sessions route
                          WHERE route.incarnation_id = $3
                            AND route.user_id = media_session_requests.user_id
                            AND route.request_fingerprint = media_session_requests.request_fingerprint
                            AND route.playback_id = media_session_requests.playback_id
                            AND route.owner_node_id = $1 AND route.owner_epoch = $5
                            AND route.lease_expires_at_ms = $6 AND route.updated_at_ms = $2)",
                    params!(
                        takeover.next_owner_node_id.as_str(),
                        takeover.now_ms,
                        takeover.incarnation_id.as_str(),
                        takeover.expected_owner_node_id.as_str(),
                        next_epoch,
                        takeover.lease_expires_at_ms
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
        if changed.len() != 4
            || changed.first().copied() != Some(1)
            || changed.get(1).copied() != Some(1)
            || changed.get(2).copied().is_some_and(|count| count > 1)
        {
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

    async fn end_media_session_if_owner(
        &self,
        end: &MediaSessionEnd,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if !valid_uuid(&end.incarnation_id)
            || !valid_uuid(&end.session_id)
            || end.expected_owner_node_id.is_empty()
            || end.expected_owner_node_id.len() > 256
            || end.expected_owner_epoch <= 0
            || end.expected_lease_expires_at_ms <= 0
            || !crate::domain::valid_media_session_terminal_reason(&end.terminal_reason)
            || end.now_ms < 0
        {
            return Err(StoreError::Task(
                "invalid exact media-session end".to_owned(),
            ));
        }
        let route = route_by(self, "incarnation_id", &end.incarnation_id).await?;
        let Some(route) = route else {
            return Ok(None);
        };
        if route.session_id != end.session_id
            || route.owner_node_id != end.expected_owner_node_id
            || route.owner_epoch != end.expected_owner_epoch
        {
            return Ok(None);
        }
        if route.state != "active" && route.state != "ended" {
            return Ok(None);
        }
        let lease_resource = format!("session:{}", end.incarnation_id);
        let changed = self
            .client()
            .txn([
                (
                    "UPDATE media_sessions SET state = 'ended', terminal_reason = $1, lease_expires_at_ms = $2,
                            publication_ready_at_ms = $3, updated_at_ms = $2
                      WHERE incarnation_id = $4 AND session_id = $5
                        AND owner_node_id = $6 AND owner_epoch = $7
                        AND lease_expires_at_ms = $8 AND state = 'active'",
                    params!(
                        end.terminal_reason.as_str(),
                        end.now_ms,
                        MEDIA_SESSION_PUBLICATION_BLOCKED,
                        end.incarnation_id.as_str(),
                        end.session_id.as_str(),
                        end.expected_owner_node_id.as_str(),
                        end.expected_owner_epoch,
                        end.expected_lease_expires_at_ms
                    ),
                ),
                (
                    "UPDATE job_leases
                        SET expires_at_ms = CASE
                              WHEN expires_at_ms < $1 THEN expires_at_ms ELSE $1 END,
                            revision = revision + 1, updated_at_ms = $1
                      WHERE resource = $2 AND owner_node_id = $3 AND fence = $4
                        AND revision < 9223372036854775807
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = $5 AND session_id = $6
                            AND owner_node_id = $3 AND owner_epoch = $4
                            AND state = 'ended')",
                    params!(
                        end.now_ms,
                        lease_resource.as_str(),
                        end.expected_owner_node_id.as_str(),
                        end.expected_owner_epoch,
                        end.incarnation_id.as_str(),
                        end.session_id.as_str()
                    ),
                ),
                (
                    "DELETE FROM media_playback_pointers
                      WHERE user_id = $1 AND playback_id = $2 AND current_incarnation_id = $3
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = $3 AND session_id = $4
                            AND owner_node_id = $5 AND owner_epoch = $6
                            AND state = 'ended')",
                    params!(
                        route.user_id,
                        route.playback_id.as_str(),
                        end.incarnation_id.as_str(),
                        end.session_id.as_str(),
                        end.expected_owner_node_id.as_str(),
                        end.expected_owner_epoch
                    ),
                ),
                (
                    "DELETE FROM cache_consumer_pins
                      WHERE consumer_kind = 'media_session' AND consumer_id = $1
                        AND consumer_epoch = $2
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = $1 AND session_id = $3
                            AND owner_node_id = $4 AND owner_epoch = $2
                            AND state = 'ended')",
                    params!(
                        end.incarnation_id.as_str(),
                        end.expected_owner_epoch,
                        end.session_id.as_str(),
                        end.expected_owner_node_id.as_str()
                    ),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        // `0` is the idempotent already-ended case (or a lost ownership
        // race). The exact post-transaction read below distinguishes them;
        // cleanup statements intentionally run in both backend implementations.
        let _ended_now = changed.first().copied() == Some(1);
        Ok(route_by(self, "incarnation_id", &end.incarnation_id)
            .await?
            .filter(|settled| {
                settled.session_id == end.session_id
                    && settled.owner_node_id == end.expected_owner_node_id
                    && settled.owner_epoch == end.expected_owner_epoch
                    && settled.state == "ended"
            }))
    }

    async fn end_media_session(
        &self,
        session_id: &str,
        terminal_reason: &str,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if !valid_uuid(session_id) {
            return Ok(None);
        }
        if !crate::domain::valid_media_session_terminal_reason(terminal_reason) {
            return Err(StoreError::Task(
                "invalid media-session terminal reason".to_owned(),
            ));
        }
        let route = route_by(self, "session_id", session_id).await?;
        let Some(route) = route else {
            return Ok(None);
        };
        let lease_resource = format!("session:{}", route.incarnation_id);
        // The route read above is outside this transaction, so a takeover may
        // commit between the two and move ownership to a different node at a
        // higher fence. Every dependent statement therefore reads the owner
        // and epoch back out of `media_sessions` inside the transaction
        // instead of trusting that snapshot: otherwise ending a session that
        // has just been taken over clamps a lease nobody holds, leaves the
        // successor's cache pin behind, and hands the caller a route naming
        // the dead node — so the abort is sent to a host that is gone while
        // the replacement encoder keeps running.
        self.client()
            .txn([
                (
                    "UPDATE media_sessions SET state = 'ended', terminal_reason = $1,
                            lease_expires_at_ms = $2, publication_ready_at_ms = $3,
                            updated_at_ms = $2
                      WHERE incarnation_id = $4 AND state != 'ended'",
                    params!(
                        terminal_reason,
                        now_ms,
                        MEDIA_SESSION_PUBLICATION_BLOCKED,
                        route.incarnation_id.as_str()
                    ),
                ),
                (
                    "UPDATE job_leases
                        SET expires_at_ms = CASE
                              WHEN expires_at_ms < $1 THEN expires_at_ms ELSE $1 END,
                            revision = revision + 1, updated_at_ms = $1
                      WHERE resource = $2
                        AND revision < 9223372036854775807
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = $3 AND state = 'ended'
                            AND media_sessions.owner_node_id = job_leases.owner_node_id
                            AND media_sessions.owner_epoch = job_leases.fence)",
                    params!(
                        now_ms,
                        lease_resource.as_str(),
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
                    // Every epoch's pin, not just the epoch the pre-read saw:
                    // the incarnation is over, so no generation of it may keep
                    // a shared-cache root alive.
                    "DELETE FROM cache_consumer_pins
                      WHERE consumer_kind = 'media_session' AND consumer_id = $1
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = $1 AND state = 'ended')",
                    params!(route.incarnation_id.as_str()),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        // Report what the caller must act on — the owner as of the end, not as
        // of the pre-read. `stop_owned_session` picks local teardown or a peer
        // abort from this field.
        Ok(route_by(self, "incarnation_id", &route.incarnation_id)
            .await?
            .or(Some(route)))
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
                      WHERE state = 'active' AND lease_expires_at_ms <= $1)
                    OR EXISTS (SELECT 1 FROM job_leases lease
                      JOIN media_sessions session
                        ON lease.resource = 'session:' || session.incarnation_id
                      WHERE session.state = 'ended' AND session.lease_expires_at_ms <= $2
                        AND lease.expires_at_ms > $2
                        AND lease.revision < 9223372036854775807)
                    OR EXISTS (SELECT 1 FROM media_playback_pointers pointer
                      LEFT JOIN media_sessions session
                        ON session.incarnation_id = pointer.current_incarnation_id
                      WHERE session.incarnation_id IS NULL OR session.state != 'active'
                        OR session.lease_expires_at_ms <= $1)
                    OR EXISTS (SELECT 1 FROM media_session_preparations preparation
                      LEFT JOIN media_sessions session
                        ON session.incarnation_id = preparation.staged_incarnation_id
                      WHERE session.incarnation_id IS NULL OR session.state != 'active')
                    OR EXISTS (SELECT 1 FROM media_session_requests
                      WHERE state = 'starting' AND claim_expires_at_ms <= $2)
                    OR EXISTS (SELECT 1 FROM media_session_requests
                      WHERE state = 'failed' AND updated_at_ms < $3)
                    OR EXISTS (SELECT 1 FROM media_session_requests request
                      WHERE request.state = 'resolved' AND request.updated_at_ms < $4
                        AND NOT EXISTS (SELECT 1 FROM media_sessions session
                          WHERE session.incarnation_id = request.incarnation_id
                            AND session.state = 'active'
                            AND session.lease_expires_at_ms > $2))
                    OR EXISTS (SELECT 1 FROM media_sessions
                      WHERE state = 'ended' AND updated_at_ms < $4)
                    OR EXISTS (SELECT 1 FROM media_session_terminal_acks acknowledgement
                      WHERE acknowledgement.expires_at_ms <= $2
                         OR NOT EXISTS (SELECT 1 FROM media_sessions session
                              WHERE session.session_id = acknowledgement.session_id
                                AND session.incarnation_id = acknowledgement.incarnation_id))
                    OR EXISTS (SELECT 1 FROM job_leases lease
                      WHERE lease.resource LIKE 'session:%' AND lease.updated_at_ms < $4
                        AND NOT EXISTS (SELECT 1 FROM media_sessions session
                          WHERE lease.resource = 'session:' || session.incarnation_id))",
                params!(retire_before, now_ms, failed_cutoff, retained_cutoff),
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
                "UPDATE media_sessions SET state = 'ended', terminal_reason = 'replaced', lease_expires_at_ms = $1,
                        publication_ready_at_ms = $2, updated_at_ms = $1
                  WHERE incarnation_id IN (
                    SELECT incarnation_id FROM media_sessions
                     WHERE state = 'active' AND lease_expires_at_ms <= $3
                     ORDER BY lease_expires_at_ms, incarnation_id LIMIT $4)",
                params!(
                    now_ms,
                    MEDIA_SESSION_PUBLICATION_BLOCKED,
                    retire_before,
                    MAINTENANCE_BATCH
                ),
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
                     OR session.lease_expires_at_ms <= $1
                  ORDER BY pointer.updated_at_ms, pointer.rowid LIMIT $2)",
                params!(retire_before, MAINTENANCE_BATCH),
            ),
            (
                // The ledger's own reaper, and the reason the preparation
                // deadline can be a single clock. The retirement sweep above
                // already ended any staged row past its deadline — a staged
                // row is `active`, so it is in that sweep like any other — and
                // this clears the ledger entry it left behind. Without it a
                // dead owner's preparation holds the one-per-playback slot
                // forever and refuses every future prepare for that player.
                //
                // Keyed on the successor's state, never on the deadline, so it
                // can never race a live preparation whose owner is renewing.
                "DELETE FROM media_session_preparations WHERE rowid IN (
                   SELECT preparation.rowid FROM media_session_preparations preparation
                   LEFT JOIN media_sessions session
                     ON session.incarnation_id = preparation.staged_incarnation_id
                  WHERE session.incarnation_id IS NULL OR session.state != 'active'
                  ORDER BY preparation.updated_at_ms, preparation.rowid LIMIT $1)",
                params!(MAINTENANCE_BATCH),
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
                "DELETE FROM media_session_terminal_acks WHERE rowid IN (
                   SELECT acknowledgement.rowid FROM media_session_terminal_acks acknowledgement
                    WHERE acknowledgement.expires_at_ms <= $1
                       OR NOT EXISTS (SELECT 1 FROM media_sessions session
                            WHERE session.session_id = acknowledgement.session_id
                              AND session.incarnation_id = acknowledgement.incarnation_id)
                    ORDER BY acknowledgement.expires_at_ms, acknowledgement.rowid LIMIT $2)",
                params!(now_ms, MAINTENANCE_BATCH),
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
                    AND publication_ready_at_ms != $3
                    AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                      WHERE request.user_id = media_sessions.user_id
                        AND request.incarnation_id = media_sessions.incarnation_id
                        AND request.state = 'starting'
                        AND media_sessions.publication_ready_at_ms = 0
                        AND request.claim_expires_at_ms <= media_sessions.lease_expires_at_ms)
                  ORDER BY updated_at_ms, incarnation_id LIMIT $4",
                params!(
                    owner_node_id,
                    now_ms,
                    MEDIA_SESSION_PUBLICATION_BLOCKED,
                    MAX_OWNED
                ),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MEDIA_SESSIONS_SCHEMA, MEDIA_SESSIONS_V10_SCHEMA,
        MEDIA_SESSION_PUBLICATION_FENCE_MIGRATION, MEDIA_SESSION_TERMINAL_REASON_MIGRATION,
    };

    const SOURCE: &str = include_str!("hiqlite_sessions.rs");

    fn method_source(method: &str) -> &'static str {
        let declaration = format!("    async fn {method}(");
        let start = SOURCE
            .find(&declaration)
            .unwrap_or_else(|| panic!("missing {declaration} in hiqlite_sessions.rs"));
        let rest = &SOURCE[start..];
        let end = rest[1..]
            .find("\n    async fn ")
            .map_or(rest.len(), |offset| offset + 1);
        &rest[..end]
    }

    /// Every `$N` placeholder in a replicated statement must be introduced in
    /// numeric order.
    ///
    /// SQLite resolves `$N` as a *named* parameter and assigns its index by
    /// first appearance, not by the number after the sigil, while `params!`
    /// binds positionally. A statement that introduces `$7` before `$3`
    /// therefore compiles, runs, affects rows, and writes every value into
    /// the wrong column — silently. This is a whole-file scan rather than a
    /// per-statement one because the failure mode is invisible at the call
    /// site and cost this module four statements at once.
    #[test]
    fn every_replicated_placeholder_is_introduced_in_order() {
        let production_source = SOURCE
            .split_once("\n#[cfg(test)]")
            .map_or(SOURCE, |(source, _)| source);
        let mut statement = String::new();
        let mut in_statement = false;
        let mut offenders = Vec::new();
        for (number, line) in production_source.lines().enumerate() {
            let quotes = line.matches('"').count();
            if !in_statement && quotes == 1 {
                in_statement = true;
                statement.clear();
                statement.push_str(line);
                statement.push(' ');
                continue;
            }
            if in_statement {
                statement.push_str(line);
                statement.push(' ');
                if quotes >= 1 {
                    in_statement = false;
                    if let Some(first) = first_out_of_order(&statement) {
                        offenders.push(format!("line {}: introduces {first} early", number + 1));
                    }
                }
                continue;
            }
            if quotes >= 2 {
                if let Some(first) = first_out_of_order(line) {
                    offenders.push(format!("line {}: introduces {first} early", number + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "replicated statements bind by first appearance: {offenders:?}"
        );
    }

    fn first_out_of_order(text: &str) -> Option<String> {
        let mut seen: Vec<u32> = Vec::new();
        let bytes = text.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != b'$' {
                index += 1;
                continue;
            }
            let mut end = index + 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end == index + 1 {
                index += 1;
                continue;
            }
            let ordinal: u32 = text[index + 1..end].parse().unwrap_or(0);
            if !seen.contains(&ordinal) {
                if ordinal as usize != seen.len() + 1 {
                    return Some(format!("${ordinal}"));
                }
                seen.push(ordinal);
            }
            index = end;
        }
        None
    }

    /// Ending a session must act on the owner the row has at commit time, not
    /// on the snapshot the pre-transaction read returned.
    ///
    /// The read that drives `end_media_session` is outside its transaction, so
    /// a takeover can commit between the two. If the dependent statements are
    /// driven by Rust-side `route.owner_node_id` / `route.owner_epoch`, the
    /// end clamps a lease the dead node no longer holds and leaves the
    /// successor's pin behind. That interleaving is not reproducible through
    /// the public store API, so the property is asserted where it lives: the
    /// statements read owner and fence back out of `media_sessions`.
    #[test]
    fn ending_a_session_reads_its_owner_inside_the_transaction() {
        let source = method_source("end_media_session");
        assert!(
            source.contains("media_sessions.owner_node_id = job_leases.owner_node_id")
                && source.contains("media_sessions.owner_epoch = job_leases.fence"),
            "the lease clamp must correlate against the row, not a pre-read snapshot"
        );
        assert!(
            !source.contains("route.owner_node_id.as_str()")
                && !source.contains("route.owner_epoch"),
            "no statement in this transaction may bind the pre-read owner or fence"
        );
        assert!(
            !source.contains("consumer_epoch = $"),
            "the pin delete covers every epoch of the ended incarnation"
        );
        assert!(
            source.contains("Ok(route_by(self, \"incarnation_id\", &route.incarnation_id)"),
            "the returned route must be re-read after the transaction commits"
        );
    }

    /// Reproduce the ordering which the backend-neutral sequential contract
    /// cannot force: caller A reads the predecessor pointer, caller B commits
    /// the exact activation and renews it, then caller A finally enters its
    /// replicated transaction with the stale activation lease.
    ///
    /// The transaction must observe B's current pointer and affect zero rows.
    /// In particular it may not rewrite the renewed session/job lease, the
    /// pointer timestamp, or the resolved request. The all-zero result is only
    /// accepted after exact route and current-pointer reads outside the
    /// transaction, so an ordinary precondition failure cannot masquerade as
    /// a replay.
    /// §4.2's guards, written for the commit path rather than inherited.
    ///
    /// The all-zero replicated transaction is ambiguous between "I lost every
    /// precondition" and "I am an exact replay", and only the post-commit
    /// projection tells them apart. These assertions are what stop that
    /// discriminator being quietly deleted by somebody who reads the
    /// `rows_affected` check above it and concludes it is redundant.
    #[test]
    fn preparation_commit_disambiguates_replay_from_total_loss() {
        let source = method_source("commit_media_session_preparation");
        assert!(
            source.contains("changed.iter().all(|affected| *affected == 0)"),
            "an all-zero transaction must be treated as a candidate replay \
             rather than a loss"
        );
        assert!(
            source.contains("pointer.as_deref() != Some(staged.staged_incarnation_id.as_str())"),
            "and the exact post-commit pointer projection is what settles it"
        );
        assert_eq!(
            source.matches("current_incarnation_id = $5").count(),
            1,
            "the pointer advance fences on the recorded predecessor, never on \
             whatever the pointer happens to name"
        );
        assert!(
            !source.contains("SELECT current_incarnation_id FROM media_playback_pointers\n                      WHERE user_id = $4 AND playback_id = $5)"),
            "the predecessor's retirement must name an exact incarnation, not \
             a pointer subquery — that is the §2.4 divergence"
        );
        assert!(
            source.contains("abort_media_session_preparation("),
            "losing the pointer CAS aborts the staged successor rather than \
             leaving the ledger row to lock the playback out of preparation"
        );
    }

    /// A preparation must not run the supersession reap or move the pointer.
    ///
    /// Acceptance 3, as a source guard, because the runnable form of it needs
    /// a three-voter cluster and does not run in the fast loop.
    #[test]
    fn preparation_neither_reaps_nor_repoints() {
        let source = method_source("prepare_media_session");
        assert!(
            !source.contains("terminal_reason = 'superseded'"),
            "prepare must not run the supersession reap"
        );
        assert!(
            !source.contains("UPDATE media_playback_pointers")
                && !source.contains("INSERT INTO media_playback_pointers"),
            "prepare must not write the pointer — a staged successor is \
             invisible to the current-session lookup precisely because it \
             holds none"
        );
        assert!(
            source.contains("NOT EXISTS (SELECT 1 FROM media_session_preparations\n                      WHERE user_id = $3 AND playback_id = $4)"),
            "one staged successor per playback, inlined because a replicated \
             transaction cannot read and branch"
        );
        assert!(
            source.contains("INSERT INTO job_leases"),
            "a staged successor takes its own session lease, or it can never \
             be renewed or taken over once it commits"
        );
    }

    /// Every write an abort performs is gated on the ledger row.
    ///
    /// The incarnation id alone names any session in the database, so an
    /// ungated `WHERE incarnation_id = ?` ends a stranger's live stream.
    #[test]
    fn preparation_abort_is_ledger_gated_in_every_statement() {
        let source = method_source("abort_media_session_preparation");
        assert!(
            source.contains("EXISTS (SELECT 1 FROM media_session_preparations"),
            "the retirement is gated on the ledger row — the incarnation id \
             alone names any session in the database"
        );
        assert_eq!(
            source
                .matches("state = 'ended' AND updated_at_ms = $")
                .count(),
            2,
            "the pin delete and the lease clamp also chain on that retirement \
             having fired at this instant — `now_ms` is the caller's, so the \
             timestamp alone is not proof, and the ledger EXISTS alone does \
             not say the work is this abort's"
        );
        assert_eq!(
            source
                .matches("EXISTS (SELECT 1 FROM media_session_preparations")
                .count(),
            3,
            "all three mutations are ledger-gated; only the ledger DELETE \
             itself is not, and it comes last"
        );
        let ledger_delete = source
            .find("DELETE FROM media_session_preparations")
            .expect("the ledger row is cleared");
        assert!(
            ledger_delete
                > source
                    .find("UPDATE media_sessions")
                    .expect("the retirement exists"),
            "the ledger DELETE comes last, because the retirement reads it"
        );
    }

    #[test]
    fn activation_losing_pointer_read_race_is_a_read_only_replay() {
        let source = method_source("activate_media_session");
        assert_eq!(
            source.matches("current_incarnation_id = $8)").count(),
            2,
            "both the job-lease insert and conflict update must be suppressed by the transaction-time pointer"
        );
        assert!(
            source.contains(
                "AND NOT EXISTS (\n                      SELECT 1 FROM media_playback_pointers\n                       WHERE user_id = $3 AND playback_id = $4\n                         AND current_incarnation_id = $1)"
            ),
            "the session insert/upsert must be suppressed after the exact activation wins"
        );
        assert!(
            source.contains(
                "media_playback_pointers.current_incarnation_id\n                        != excluded.current_incarnation_id"
            ),
            "an already-current pointer must not rewrite its update timestamp"
        );
        assert!(
            source.contains("OR (state = 'resolved' AND response_json = $9)))"),
            "an exact resolved request must remain eligible for read-only replay"
        );
        assert!(
            !source.contains("UPDATE media_session_requests")
                && !source.contains("INSERT INTO media_session_requests")
                && !source.contains("DELETE FROM media_session_requests"),
            "activation may validate an existing request but must not mutate it"
        );
        assert!(
            source.contains("changed.iter().all(|affected| *affected == 0)"),
            "the replicated transaction must recognize the read-only race result"
        );
        assert!(
            source.contains(
                "committed_pointer.as_deref() != Some(activation.incarnation_id.as_str())"
            ),
            "all-zero transactions must prove the exact pointer after commit"
        );
        assert!(
            source.contains(".filter(|route| activation_route_matches(route, activation))"),
            "all-zero transactions must also prove the immutable activation identity"
        );
    }

    #[test]
    fn direct_v9_upgrade_uses_the_frozen_v10_session_shape() {
        assert!(!MEDIA_SESSIONS_V10_SCHEMA.contains("terminal_reason"));
        assert!(!MEDIA_SESSIONS_V10_SCHEMA.contains("publication_ready_at_ms"));
        assert!(MEDIA_SESSIONS_SCHEMA.contains("terminal_reason"));
        assert!(MEDIA_SESSIONS_SCHEMA.contains("publication_ready_at_ms"));
        assert!(MEDIA_SESSION_TERMINAL_REASON_MIGRATION.contains("terminal_reason"));
        assert!(MEDIA_SESSION_PUBLICATION_FENCE_MIGRATION.contains("publication_ready_at_ms"));
    }
}
