use async_trait::async_trait;
use rusqlite::{params, OptionalExtension, Row};

use super::SqliteStore;
use crate::cluster::coordination::removed_job_owner_key;
use crate::domain::{
    MediaSessionActivation, MediaSessionActivationOutcome, MediaSessionActivationSettlement,
    MediaSessionEnd, MediaSessionPreparation, MediaSessionPreparationAbortRequest,
    MediaSessionPreparationCommit, MediaSessionPreparationCommitRequest,
    MediaSessionProjectionCompletion, MediaSessionRenewal, MediaSessionRequestClaim,
    MediaSessionRoute, MediaSessionStagedGeneration, MediaSessionTakeover,
    MediaSessionTakeoverCursor, MediaSessionTerminalAck, OwnedMediaSessionLease,
    MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS, MEDIA_SESSION_PUBLICATION_BLOCKED,
};
use crate::error::StoreError;
use crate::store::MediaSessionStore;

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
const ROUTE_COLS: &str = "incarnation_id, session_id, user_id, playback_id, \
    request_fingerprint, owner_node_id, owner_epoch, lease_expires_at_ms, state, terminal_reason, \
    publication_ready_at_ms, recipe_json, response_json, produced_playable_through_ms, fetched_through_ms, \
    media_origin_ms, media_sequence, discontinuity_sequence, updated_at_ms";

fn route_from_row(row: &Row<'_>) -> rusqlite::Result<MediaSessionRoute> {
    Ok(MediaSessionRoute {
        incarnation_id: row.get(0)?,
        session_id: row.get(1)?,
        user_id: row.get(2)?,
        playback_id: row.get(3)?,
        request_fingerprint: row.get(4)?,
        owner_node_id: row.get(5)?,
        owner_epoch: row.get(6)?,
        lease_expires_at_ms: row.get(7)?,
        state: row.get(8)?,
        terminal_reason: row.get(9)?,
        publication_ready_at_ms: row.get(10)?,
        recipe_json: row.get(11)?,
        response_json: row.get(12)?,
        produced_playable_through_ms: row.get(13)?,
        fetched_through_ms: row.get(14)?,
        media_origin_ms: row.get(15)?,
        media_sequence: row.get(16)?,
        discontinuity_sequence: row.get(17)?,
        updated_at_ms: row.get(18)?,
    })
}

fn terminal_ack_from_row(row: &Row<'_>) -> rusqlite::Result<MediaSessionTerminalAck> {
    Ok(MediaSessionTerminalAck {
        incarnation_id: row.get(0)?,
        session_id: row.get(1)?,
        owner_node_id: row.get(2)?,
        owner_epoch: row.get(3)?,
        client_instance_id: row.get(4)?,
        sequence: row.get(5)?,
        request_fingerprint: row.get(6)?,
        response_json: row.get(7)?,
        expires_at_ms: row.get(8)?,
        updated_at_ms: row.get(9)?,
    })
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
    if valid {
        Ok(())
    } else {
        Err(StoreError::Task(
            "invalid media-session activation".to_owned(),
        ))
    }
}

fn validate_preparation(preparation: &MediaSessionPreparation) -> Result<(), StoreError> {
    let valid = valid_uuid(&preparation.incarnation_id)
        && valid_uuid(&preparation.session_id)
        && valid_uuid(&preparation.expected_predecessor_incarnation_id)
        // A successor staged against itself is not a successor. The pointer
        // CAS below would pass for it, because the pointer would name it.
        && preparation.expected_predecessor_incarnation_id != preparation.incarnation_id
        && !preparation.expected_predecessor_owner_node_id.is_empty()
        && preparation.expected_predecessor_owner_node_id.len() <= 256
        && preparation.expected_predecessor_owner_epoch > 0
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

/// End one staged successor, every write gated on its ledger row.
///
/// The gating is the safety property and it has to be in the SQL, not in a
/// predicate applied to the result: the incarnation id alone names any session
/// in the database, so an `UPDATE … WHERE incarnation_id = ?` would end a
/// stranger's live stream and then report that it had lost. `playback_id` is
/// caller-supplied and `user_id` is not otherwise consulted, so there is no
/// boundary here except this one.
///
/// The ledger row is deleted LAST, because the three writes before it read it.
///
/// Deliberately says nothing about whether it did anything. The caller derives
/// that from a re-read plus a predicate, never from `rows_affected`, so an
/// owner retrying an abort after a crash reads the same answer as the first
/// attempt rather than a spurious loss.
fn abort_staged_generation(
    tx: &rusqlite::Transaction<'_>,
    user_id: i64,
    playback_id: &str,
    staged_incarnation_id: &str,
    now_ms: i64,
) -> rusqlite::Result<usize> {
    let retired = tx.execute(
        "UPDATE media_sessions
            SET state = 'ended', terminal_reason = 'replaced',
                lease_expires_at_ms = ?1, updated_at_ms = ?1
          WHERE incarnation_id = ?2 AND state != 'ended'
            AND EXISTS (SELECT 1 FROM media_session_preparations
              WHERE user_id = ?3 AND playback_id = ?4
                AND staged_incarnation_id = ?2)",
        params![now_ms, staged_incarnation_id, user_id, playback_id],
    )?;
    // The next two carry BOTH guards. "Ended at this instant" alone is not
    // proof the retirement above fired: `now_ms` is the caller's, not the
    // retirement's, so a row that happened to be ended at exactly that
    // millisecond by anything else would satisfy it — and an incarnation id
    // names any session in the database. The ledger EXISTS is what makes this
    // a staged successor of this playback; the timestamp is what makes it
    // this abort's own work.
    tx.execute(
        "DELETE FROM cache_consumer_pins
          WHERE consumer_kind = 'media_session' AND consumer_id = ?1
            AND EXISTS (SELECT 1 FROM media_sessions
              WHERE incarnation_id = ?1 AND state = 'ended' AND updated_at_ms = ?2)
            AND EXISTS (SELECT 1 FROM media_session_preparations
              WHERE user_id = ?3 AND playback_id = ?4 AND staged_incarnation_id = ?1)",
        params![staged_incarnation_id, now_ms, user_id, playback_id],
    )?;
    tx.execute(
        "UPDATE job_leases
            SET expires_at_ms = CASE
                  WHEN expires_at_ms < ?1 THEN expires_at_ms ELSE ?1 END,
                revision = revision + 1, updated_at_ms = ?1
          WHERE resource = ?2 AND revision < 9223372036854775807
            AND EXISTS (SELECT 1 FROM media_sessions
              WHERE incarnation_id = ?3 AND state = 'ended' AND updated_at_ms = ?1)
            AND EXISTS (SELECT 1 FROM media_session_preparations
              WHERE user_id = ?4 AND playback_id = ?5 AND staged_incarnation_id = ?3)",
        params![
            now_ms,
            format!("session:{staged_incarnation_id}"),
            staged_incarnation_id,
            user_id,
            playback_id,
        ],
    )?;
    tx.execute(
        "DELETE FROM media_session_preparations
          WHERE user_id = ?1 AND playback_id = ?2 AND staged_incarnation_id = ?3",
        params![user_id, playback_id, staged_incarnation_id],
    )?;
    Ok(retired)
}

/// Stage one preparation inside the caller's transaction.
///
/// The caller owns commit versus rollback. Returning `None` therefore leaves
/// no partial lease/session/ledger writes behind, which lets the ordinary
/// prepare and the occupied-slot rejoin share exactly the same admission and
/// replay rules.
fn prepare_within(
    tx: &rusqlite::Transaction<'_>,
    preparation: &MediaSessionPreparation,
) -> rusqlite::Result<Option<MediaSessionRoute>> {
    let predecessor_is_authoritative = tx
        .query_row(
            "SELECT 1 FROM media_playback_pointers pointer
              JOIN media_sessions predecessor
                ON predecessor.incarnation_id = pointer.current_incarnation_id
             WHERE pointer.user_id = ?1 AND pointer.playback_id = ?2
               AND pointer.current_incarnation_id = ?3
               AND predecessor.owner_node_id = ?4
               AND predecessor.owner_epoch = ?5
               AND predecessor.state = 'active'",
            params![
                preparation.user_id,
                preparation.playback_id,
                preparation.expected_predecessor_incarnation_id,
                preparation.expected_predecessor_owner_node_id,
                preparation.expected_predecessor_owner_epoch,
            ],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !predecessor_is_authoritative {
        return Ok(None);
    }
    let existing = tx
        .query_row(
            &format!(
                "SELECT {STAGED_COLS} FROM media_session_preparations
                  WHERE user_id = ?1 AND playback_id = ?2"
            ),
            params![preparation.user_id, preparation.playback_id],
            staged_from_row,
        )
        .optional()?;
    if let Some(existing) = existing {
        let replay = existing.staged_incarnation_id == preparation.incarnation_id
            && existing.expected_predecessor_incarnation_id
                == preparation.expected_predecessor_incarnation_id;
        return if replay {
            Ok(tx
                .query_row(
                    &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"),
                    [preparation.incarnation_id.as_str()],
                    route_from_row,
                )
                .optional()?
                .filter(|route| preparation_route_matches(route, preparation)))
        } else {
            Ok(None)
        };
    }

    let current_pointer = tx
        .query_row(
            "SELECT current_incarnation_id FROM media_playback_pointers
              WHERE user_id = ?1 AND playback_id = ?2",
            params![preparation.user_id, preparation.playback_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if current_pointer.as_deref() != Some(preparation.expected_predecessor_incarnation_id.as_str())
    {
        return Ok(None);
    }

    // A preparation reaps nothing, so the pointed-at predecessor remains in
    // every admission count. During a rejoin the caller has already ended the
    // old staged row in this transaction, which naturally frees its live slot
    // before these reads run.
    let current: i64 = tx.query_row(
        "SELECT COUNT(*) FROM media_sessions
          WHERE user_id = ?1 AND state IN ('starting', 'active')
            AND lease_expires_at_ms > ?2 AND incarnation_id != ?3",
        params![
            preparation.user_id,
            preparation.now_ms,
            preparation.incarnation_id,
        ],
        |row| row.get(0),
    )?;
    let session_rows: i64 = tx.query_row(
        "SELECT COUNT(*) FROM media_sessions
          WHERE user_id = ?1 AND incarnation_id != ?2",
        params![preparation.user_id, preparation.incarnation_id],
        |row| row.get(0),
    )?;
    let owner_current: i64 = tx.query_row(
        "SELECT COUNT(*) FROM media_sessions
          WHERE owner_node_id = ?1 AND state = 'active'
            AND lease_expires_at_ms > ?2 AND incarnation_id != ?3",
        params![
            preparation.owner_node_id,
            preparation.now_ms,
            preparation.incarnation_id,
        ],
        |row| row.get(0),
    )?;
    if current >= MAX_CURRENT_PER_USER
        || session_rows >= MAX_SESSION_ROWS_PER_USER
        || owner_current >= MAX_OWNED
    {
        return Ok(None);
    }

    let removed_owner: i64 = tx.query_row(
        "SELECT COUNT(*) FROM settings WHERE key = ?1",
        params![removed_job_owner_key(&preparation.owner_node_id)],
        |row| row.get(0),
    )?;
    if removed_owner > 0 {
        return Ok(None);
    }
    let staged_elsewhere: i64 = tx.query_row(
        "SELECT COUNT(*) FROM media_session_preparations
          WHERE staged_incarnation_id = ?1",
        params![preparation.incarnation_id],
        |row| row.get(0),
    )?;
    if staged_elsewhere > 0 {
        return Ok(None);
    }

    if tx.execute(
        "INSERT INTO job_leases
            (resource, owner_node_id, fence, revision, expires_at_ms, updated_at_ms)
         VALUES (?1, ?2, 1, 1, ?3, ?4)
         ON CONFLICT(resource) DO UPDATE SET
            expires_at_ms = excluded.expires_at_ms,
            revision = job_leases.revision + 1,
            updated_at_ms = excluded.updated_at_ms
         WHERE job_leases.owner_node_id = excluded.owner_node_id
           AND job_leases.fence = 1 AND job_leases.expires_at_ms > ?4
           AND job_leases.revision < 9223372036854775807",
        params![
            format!("session:{}", preparation.incarnation_id),
            preparation.owner_node_id,
            preparation.deadline_ms,
            preparation.now_ms,
        ],
    )? != 1
    {
        return Ok(None);
    }
    tx.execute(
        "INSERT INTO media_sessions
            (incarnation_id, session_id, user_id, playback_id, request_fingerprint,
             owner_node_id, owner_epoch, lease_expires_at_ms, state, recipe_json,
             response_json, produced_playable_through_ms, fetched_through_ms,
             media_origin_ms, media_sequence, discontinuity_sequence,
             publication_ready_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, 'active', ?8, ?9,
                 0, 0, ?10, 0, 0, ?11, ?12)",
        params![
            preparation.incarnation_id,
            preparation.session_id,
            preparation.user_id,
            preparation.playback_id,
            preparation.request_fingerprint,
            preparation.owner_node_id,
            preparation.deadline_ms,
            preparation.recipe_json,
            preparation.response_json,
            preparation.media_origin_ms,
            MEDIA_SESSION_PUBLICATION_BLOCKED,
            preparation.now_ms,
        ],
    )?;
    tx.execute(
        "INSERT INTO media_session_preparations
            (user_id, playback_id, staged_incarnation_id,
             expected_predecessor_incarnation_id, deadline_ms,
             created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![
            preparation.user_id,
            preparation.playback_id,
            preparation.incarnation_id,
            preparation.expected_predecessor_incarnation_id,
            preparation.deadline_ms,
            preparation.now_ms,
        ],
    )?;
    Ok(tx
        .query_row(
            &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"),
            [preparation.incarnation_id.as_str()],
            route_from_row,
        )
        .optional()?
        .filter(|route| preparation_route_matches(route, preparation)))
}

/// Exact immutable identity for a preparation replay.
///
/// Kept identical to the replicated twin's, because a replay that reads back
/// on one backend and loses on the other is the §2.4 failure mode with a
/// different name. Progress and lease coordinates are deliberately absent:
/// they are monotone durable state a replay must read back, never restore.
fn preparation_route_matches(
    route: &MediaSessionRoute,
    preparation: &MediaSessionPreparation,
) -> bool {
    route.incarnation_id == preparation.incarnation_id
        && route.session_id == preparation.session_id
        && route.user_id == preparation.user_id
        && route.playback_id == preparation.playback_id
        && route.request_fingerprint == preparation.request_fingerprint
        && route.owner_node_id == preparation.owner_node_id
        && route.owner_epoch == 1
        && route.recipe_json == preparation.recipe_json
        && route.response_json == preparation.response_json
        && route.media_origin_ms == preparation.media_origin_ms
        && route.state == "active"
        && route.publication_ready_at_ms == MEDIA_SESSION_PUBLICATION_BLOCKED
}

fn staged_from_row(row: &Row<'_>) -> rusqlite::Result<MediaSessionStagedGeneration> {
    Ok(MediaSessionStagedGeneration {
        user_id: row.get(0)?,
        playback_id: row.get(1)?,
        staged_incarnation_id: row.get(2)?,
        expected_predecessor_incarnation_id: row.get(3)?,
        deadline_ms: row.get(4)?,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
    })
}

const STAGED_COLS: &str = "user_id, playback_id, staged_incarnation_id, \
    expected_predecessor_incarnation_id, deadline_ms, created_at_ms, updated_at_ms";

/// Exact immutable identity for an activation replay. Lease, progress,
/// publication, and update coordinates are deliberately absent: those are
/// monotone durable state which a replay must return, never restore from its
/// original input.
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

#[async_trait]
impl MediaSessionStore for SqliteStore {
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
        self.maintain_media_sessions(now_ms).await?;
        let request_id = request_id.to_owned();
        let request_fingerprint = request_fingerprint.to_ascii_lowercase();
        let playback_id = playback_id.to_owned();
        let incarnation_id = incarnation_id.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let existing = tx
                .query_row(
                    "SELECT request_fingerprint, playback_id, state, incarnation_id, owner_node_id,
                            claim_expires_at_ms
                       FROM media_session_requests WHERE user_id = ?1 AND request_id = ?2",
                    params![user_id, request_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, i64>(5)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((
                fingerprint,
                existing_playback,
                state,
                existing_incarnation,
                owner,
                expires,
            )) = existing
            {
                let outcome =
                    if fingerprint != request_fingerprint || existing_playback != playback_id {
                        MediaSessionRequestClaim::Conflict
                    } else if state == "failed" || (state == "starting" && expires <= now_ms) {
                        let reacquired = tx.execute(
                            "UPDATE media_session_requests
                            SET state = 'starting', claim_expires_at_ms = ?1,
                                incarnation_id = ?2, owner_node_id = NULL,
                                response_json = NULL, updated_at_ms = ?3
                          WHERE user_id = ?4 AND request_id = ?5
                            AND (state = 'failed'
                              OR (state = 'starting' AND claim_expires_at_ms <= ?3))
                            AND request_fingerprint = ?6 AND playback_id = ?7
                            AND (SELECT COUNT(*) FROM media_session_requests
                                  WHERE user_id = ?4 AND state = 'starting'
                                    AND claim_expires_at_ms > ?3) < ?8
                            AND (SELECT COUNT(*) FROM media_sessions
                                  WHERE user_id = ?4 AND state IN ('starting', 'active')
                                    AND lease_expires_at_ms > ?3
                                    AND incarnation_id != COALESCE((
                                      SELECT current_incarnation_id FROM media_playback_pointers
                                       WHERE user_id = ?4 AND playback_id = ?7), '')) < ?9
                            AND (SELECT COUNT(*) FROM media_sessions
                                  WHERE user_id = ?4) < ?10",
                            params![
                                claim_expires_at_ms,
                                incarnation_id,
                                now_ms,
                                user_id,
                                request_id,
                                request_fingerprint,
                                playback_id,
                                MAX_IN_FLIGHT_PER_USER,
                                MAX_CURRENT_PER_USER,
                                MAX_SESSION_ROWS_PER_USER,
                            ],
                        )? == 1;
                        if reacquired {
                            MediaSessionRequestClaim::Acquired { incarnation_id }
                        } else {
                            MediaSessionRequestClaim::Overloaded
                        }
                    } else if state == "resolved" {
                        tx.query_row(
                            &format!(
                                "SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"
                            ),
                            [existing_incarnation.as_str()],
                            route_from_row,
                        )
                        .optional()?
                        .map(|route| MediaSessionRequestClaim::Resolved(Box::new(route)))
                        .unwrap_or(MediaSessionRequestClaim::InFlight {
                            incarnation_id: existing_incarnation,
                            owner_node_id: owner,
                            claim_expires_at_ms: expires,
                        })
                    } else if state == "starting" {
                        MediaSessionRequestClaim::InFlight {
                            incarnation_id: existing_incarnation,
                            owner_node_id: owner,
                            claim_expires_at_ms: expires,
                        }
                    } else {
                        MediaSessionRequestClaim::Conflict
                    };
                tx.commit()?;
                return Ok(outcome);
            }
            let in_flight: i64 = tx.query_row(
                "SELECT COUNT(*) FROM media_session_requests
                  WHERE user_id = ?1 AND state = 'starting' AND claim_expires_at_ms > ?2",
                params![user_id, now_ms],
                |row| row.get(0),
            )?;
            let current: i64 = tx.query_row(
                "SELECT COUNT(*) FROM media_sessions
                  WHERE user_id = ?1 AND state IN ('starting', 'active')
                    AND lease_expires_at_ms > ?2
                    AND incarnation_id != COALESCE((
                      SELECT current_incarnation_id FROM media_playback_pointers
                       WHERE user_id = ?1 AND playback_id = ?3), '')",
                params![user_id, now_ms, playback_id],
                |row| row.get(0),
            )?;
            let request_rows: i64 = tx.query_row(
                "SELECT COUNT(*) FROM media_session_requests WHERE user_id = ?1",
                [user_id],
                |row| row.get(0),
            )?;
            let session_rows: i64 = tx.query_row(
                "SELECT COUNT(*) FROM media_sessions WHERE user_id = ?1",
                [user_id],
                |row| row.get(0),
            )?;
            if in_flight >= MAX_IN_FLIGHT_PER_USER
                || current >= MAX_CURRENT_PER_USER
                || request_rows >= MAX_REQUEST_ROWS_PER_USER
                || session_rows >= MAX_SESSION_ROWS_PER_USER
            {
                tx.commit()?;
                return Ok(MediaSessionRequestClaim::Overloaded);
            }
            tx.execute(
                "INSERT INTO media_session_requests
                    (user_id, request_id, request_fingerprint, playback_id, state,
                     claim_expires_at_ms, incarnation_id, owner_node_id, response_json,
                     updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, 'starting', ?5, ?6, NULL, NULL, ?7)",
                params![
                    user_id,
                    request_id,
                    request_fingerprint,
                    playback_id,
                    claim_expires_at_ms,
                    incarnation_id,
                    now_ms,
                ],
            )?;
            tx.commit()?;
            Ok(MediaSessionRequestClaim::Acquired { incarnation_id })
        })
        .await
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
        let request_id = request_id.to_owned();
        let incarnation_id = incarnation_id.to_owned();
        let owner_node_id = owner_node_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE media_session_requests SET owner_node_id = ?1, updated_at_ms = ?2
                  WHERE user_id = ?3 AND request_id = ?4 AND incarnation_id = ?5
                    AND state = 'starting' AND claim_expires_at_ms > ?2
                    AND (owner_node_id IS NULL OR owner_node_id = ?1)",
                params![owner_node_id, now_ms, user_id, request_id, incarnation_id],
            )? == 1)
        })
        .await
    }

    async fn activate_media_session(
        &self,
        activation: &MediaSessionActivation,
    ) -> Result<Option<MediaSessionActivationOutcome>, StoreError> {
        validate_activation(activation)?;
        let activation = activation.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let lease_resource = format!("session:{}", activation.incarnation_id);
            let current_pointer = tx
                .query_row(
                    "SELECT current_incarnation_id FROM media_playback_pointers
                      WHERE user_id = ?1 AND playback_id = ?2",
                    params![activation.user_id, activation.playback_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if current_pointer.as_deref() == Some(activation.incarnation_id.as_str()) {
                let route = tx
                    .query_row(
                        &format!(
                            "SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"
                        ),
                        [activation.incarnation_id.as_str()],
                        route_from_row,
                    )
                    .optional()?
                    .filter(|route| activation_route_matches(route, &activation));
                let Some(route) = route else {
                    tx.commit()?;
                    return Ok(None);
                };
                let predecessor = match activation
                    .expected_predecessor_incarnation_id
                    .as_deref()
                {
                    Some(incarnation_id) => tx
                        .query_row(
                            &format!(
                                "SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"
                            ),
                            [incarnation_id],
                            route_from_row,
                        )
                        .optional()?,
                    None => None,
                };
                tx.commit()?;
                return Ok(Some(MediaSessionActivationOutcome { route, predecessor }));
            }
            if let Some(request_id) = activation.request_id.as_deref() {
                let matches: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM media_session_requests
                      WHERE user_id = ?1 AND request_id = ?2 AND incarnation_id = ?3
                        AND request_fingerprint = ?4 AND playback_id = ?5
                        AND owner_node_id = ?6
                        AND ((state = 'starting' AND claim_expires_at_ms > ?7)
                          OR (state = 'resolved' AND response_json = ?8))",
                    params![
                        activation.user_id,
                        request_id,
                        activation.incarnation_id,
                        activation.request_fingerprint,
                        activation.playback_id,
                        activation.owner_node_id,
                        activation.now_ms,
                        activation.response_json,
                    ],
                    |row| row.get(0),
                )?;
                if matches != 1 {
                    tx.commit()?;
                    return Ok(None);
                }
            }
            if activation.fence_predecessor {
                if current_pointer.as_deref()
                    != activation.expected_predecessor_incarnation_id.as_deref()
                {
                    tx.commit()?;
                    return Ok(None);
                }
                if let Some(predecessor) = activation.expected_predecessor_incarnation_id.as_deref()
                {
                    let predecessor_pending: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM media_sessions
                          WHERE incarnation_id = ?1 AND state = 'active'
                            AND publication_ready_at_ms != 0",
                        [predecessor],
                        |row| row.get(0),
                    )?;
                    if predecessor_pending != 0 {
                        tx.commit()?;
                        return Ok(None);
                    }
                }
            }
            let current: i64 = tx.query_row(
                "SELECT COUNT(*) FROM media_sessions
                  WHERE user_id = ?1 AND state IN ('starting', 'active')
                    AND lease_expires_at_ms > ?4
                    AND incarnation_id != ?2
                    AND incarnation_id != COALESCE((
                      SELECT current_incarnation_id FROM media_playback_pointers
                       WHERE user_id = ?1 AND playback_id = ?3), '')",
                params![
                    activation.user_id,
                    activation.incarnation_id,
                    activation.playback_id,
                    activation.now_ms,
                ],
                |row| row.get(0),
            )?;
            if current >= MAX_CURRENT_PER_USER {
                tx.commit()?;
                return Ok(None);
            }
            let owner_current: i64 = tx.query_row(
                "SELECT COUNT(*) FROM media_sessions
                  WHERE owner_node_id = ?1 AND state = 'active'
                    AND lease_expires_at_ms > ?2 AND incarnation_id != ?3
                    AND incarnation_id != COALESCE((
                      SELECT current_incarnation_id FROM media_playback_pointers
                       WHERE user_id = ?4 AND playback_id = ?5), '')",
                params![
                    activation.owner_node_id,
                    activation.now_ms,
                    activation.incarnation_id,
                    activation.user_id,
                    activation.playback_id,
                ],
                |row| row.get(0),
            )?;
            let session_rows: i64 = tx.query_row(
                "SELECT COUNT(*) FROM media_sessions
                  WHERE user_id = ?1 AND incarnation_id != ?2",
                params![activation.user_id, activation.incarnation_id],
                |row| row.get(0),
            )?;
            if owner_current >= MAX_OWNED || session_rows >= MAX_SESSION_ROWS_PER_USER {
                tx.commit()?;
                return Ok(None);
            }
            let predecessor = tx
                .query_row(
                    &format!(
                        "SELECT {ROUTE_COLS} FROM media_sessions
                          WHERE incarnation_id = (
                            SELECT current_incarnation_id FROM media_playback_pointers
                             WHERE user_id = ?1 AND playback_id = ?2)
                            AND incarnation_id != ?3"
                    ),
                    params![
                        activation.user_id,
                        activation.playback_id,
                        activation.incarnation_id,
                    ],
                    route_from_row,
                )
                .optional()?;
            if tx.execute(
                "INSERT INTO job_leases
                    (resource, owner_node_id, fence, revision, expires_at_ms, updated_at_ms)
                 VALUES (?1, ?2, 1, 1, ?3, ?4)
                 ON CONFLICT(resource) DO UPDATE SET
                    expires_at_ms = excluded.expires_at_ms,
                    revision = job_leases.revision + 1,
                    updated_at_ms = excluded.updated_at_ms
                 WHERE job_leases.owner_node_id = excluded.owner_node_id
                   AND job_leases.fence = 1 AND job_leases.expires_at_ms > ?4
                   AND job_leases.revision < 9223372036854775807",
                params![
                    lease_resource,
                    activation.owner_node_id,
                    activation.lease_expires_at_ms,
                    activation.now_ms,
                ],
            )? != 1
            {
                tx.rollback()?;
                return Ok(None);
            }
            tx.execute(
                "UPDATE media_sessions SET state = 'ended', terminal_reason = 'superseded', lease_expires_at_ms = ?1,
                        publication_ready_at_ms = ?5, updated_at_ms = ?1
                  WHERE incarnation_id = (SELECT current_incarnation_id
                    FROM media_playback_pointers WHERE user_id = ?2 AND playback_id = ?3)
                    AND incarnation_id != ?4 AND state != 'ended'",
                params![
                    activation.now_ms,
                    activation.user_id,
                    activation.playback_id,
                    activation.incarnation_id,
                    MEDIA_SESSION_PUBLICATION_BLOCKED,
                ],
            )?;
            tx.execute(
                "DELETE FROM cache_consumer_pins
                  WHERE consumer_kind = 'media_session'
                    AND consumer_id IN (
                      SELECT incarnation_id FROM media_sessions
                       WHERE user_id = ?1 AND playback_id = ?2
                         AND incarnation_id != ?3 AND state = 'ended'
                         AND updated_at_ms = ?4)",
                params![
                    activation.user_id,
                    activation.playback_id,
                    activation.incarnation_id,
                    activation.now_ms,
                ],
            )?;
            tx.execute(
                "UPDATE job_leases
                    SET expires_at_ms = CASE
                          WHEN expires_at_ms < ?1 THEN expires_at_ms ELSE ?1 END,
                        revision = revision + 1, updated_at_ms = ?1
                  WHERE revision < 9223372036854775807
                    AND EXISTS (SELECT 1 FROM media_sessions AS session
                      WHERE 'session:' || session.incarnation_id = job_leases.resource
                        AND session.user_id = ?2 AND session.playback_id = ?3
                        AND session.incarnation_id != ?4 AND session.state = 'ended'
                        AND session.updated_at_ms = ?1
                        AND session.owner_node_id = job_leases.owner_node_id
                        AND session.owner_epoch = job_leases.fence)",
                params![
                    activation.now_ms,
                    activation.user_id,
                    activation.playback_id,
                    activation.incarnation_id,
                ],
            )?;
            tx.execute(
                "INSERT INTO media_sessions
                    (incarnation_id, session_id, user_id, playback_id, request_fingerprint,
                     owner_node_id, owner_epoch, lease_expires_at_ms, state, recipe_json,
                     response_json, produced_playable_through_ms, fetched_through_ms,
                     media_origin_ms, media_sequence, discontinuity_sequence,
                     publication_ready_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, 'active', ?8, ?9,
                         0, 0, ?10, 0, 0, ?13, ?11)
                 ON CONFLICT(incarnation_id) DO UPDATE SET
                    lease_expires_at_ms = excluded.lease_expires_at_ms,
                    response_json = excluded.response_json,
                    updated_at_ms = excluded.updated_at_ms
                 WHERE media_sessions.session_id = excluded.session_id
                   AND media_sessions.user_id = excluded.user_id
                   AND media_sessions.playback_id = excluded.playback_id
                   AND media_sessions.request_fingerprint = excluded.request_fingerprint
                   AND media_sessions.owner_node_id = excluded.owner_node_id
                   AND media_sessions.owner_epoch = 1 AND media_sessions.state = 'active'
                   AND EXISTS (SELECT 1 FROM job_leases
                     WHERE resource = ?12 AND owner_node_id = ?6 AND fence = 1
                       AND expires_at_ms = ?7 AND expires_at_ms > ?11)",
                params![
                    activation.incarnation_id,
                    activation.session_id,
                    activation.user_id,
                    activation.playback_id,
                    activation.request_fingerprint,
                    activation.owner_node_id,
                    activation.lease_expires_at_ms,
                    activation.recipe_json,
                    activation.response_json,
                    activation.media_origin_ms,
                    activation.now_ms,
                    lease_resource,
                    activation.publication_ready_at_ms,
                ],
            )?;
            let route = tx
                .query_row(
                    &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"),
                    [activation.incarnation_id.as_str()],
                    route_from_row,
                )
                .optional()?;
            let Some(route) = route.filter(|route| activation_route_matches(route, &activation))
            else {
                tx.rollback()?;
                return Ok(None);
            };
            tx.execute(
                "INSERT INTO media_playback_pointers
                    (user_id, playback_id, current_incarnation_id, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(user_id, playback_id) DO UPDATE SET
                    current_incarnation_id = excluded.current_incarnation_id,
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    activation.user_id,
                    activation.playback_id,
                    activation.incarnation_id,
                    activation.now_ms,
                ],
            )?;
            // Re-read inside the transaction instead of fabricating a
            // superseded result. Another first-writer terminal cause may have
            // won before activation; callers must project that durable cause
            // exactly, as the replicated implementation already does.
            let predecessor = match predecessor {
                Some(predecessor) => tx
                    .query_row(
                        &format!(
                            "SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"
                        ),
                        [predecessor.incarnation_id.as_str()],
                        route_from_row,
                    )
                    .optional()?,
                None => None,
            };
            tx.commit()?;
            Ok(Some(MediaSessionActivationOutcome { route, predecessor }))
        })
        .await
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
        let activation = activation.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let route = tx
                .query_row(
                    &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"),
                    [activation.incarnation_id.as_str()],
                    route_from_row,
                )
                .optional()?;
            match settlement {
                MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms,
                } => {
                    let valid_publication = if activation
                        .expected_predecessor_incarnation_id
                        .is_some()
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
                    let current_pointer: Option<String> = tx
                        .query_row(
                            "SELECT current_incarnation_id FROM media_playback_pointers
                              WHERE user_id = ?1 AND playback_id = ?2",
                            params![activation.user_id, activation.playback_id],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if current_pointer.as_deref() != Some(activation.incarnation_id.as_str()) {
                        tx.commit()?;
                        return Ok(None);
                    }
                    if let Some(existing) = route.as_ref().filter(|route| {
                        activation_route_matches(route, &activation)
                            && route.publication_ready_at_ms
                                != MEDIA_SESSION_PUBLICATION_BLOCKED
                            // Same reasoning as the replicated twin: identity
                            // alone would return a boundary another
                            // confirmation of this incarnation committed.
                            && route.publication_ready_at_ms == publication_ready_at_ms
                    }) {
                        tx.commit()?;
                        return Ok(Some(existing.clone()));
                    }
                    let Some(route) = route.filter(|route| {
                        activation_route_matches(route, &activation)
                            && route.publication_ready_at_ms
                                == MEDIA_SESSION_PUBLICATION_BLOCKED
                            && route.lease_expires_at_ms == activation.lease_expires_at_ms
                            && route.lease_expires_at_ms > now_ms
                    }) else {
                        tx.commit()?;
                        return Ok(None);
                    };
                    if let Some(request_id) = activation.request_id.as_deref() {
                        let pending: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM media_session_requests
                              WHERE user_id = ?1 AND request_id = ?2 AND incarnation_id = ?3
                                AND request_fingerprint = ?4 AND playback_id = ?5
                                AND owner_node_id = ?6 AND state = 'starting'",
                            params![
                                activation.user_id,
                                request_id,
                                activation.incarnation_id,
                                activation.request_fingerprint,
                                activation.playback_id,
                                activation.owner_node_id,
                            ],
                            |row| row.get(0),
                        )?;
                        if pending != 1 {
                            tx.rollback()?;
                            return Ok(None);
                        }
                    }
                    if tx.execute(
                        "UPDATE media_sessions SET publication_ready_at_ms = ?1, updated_at_ms = ?2
                          WHERE incarnation_id = ?3 AND owner_node_id = ?4 AND owner_epoch = 1
                            AND state = 'active' AND publication_ready_at_ms = ?5
                            AND lease_expires_at_ms = ?6 AND lease_expires_at_ms > ?2
                            AND EXISTS (SELECT 1 FROM media_playback_pointers
                              WHERE user_id = ?7 AND playback_id = ?8
                                AND current_incarnation_id = ?3)",
                        params![
                            publication_ready_at_ms,
                            now_ms,
                            activation.incarnation_id,
                            activation.owner_node_id,
                            MEDIA_SESSION_PUBLICATION_BLOCKED,
                            activation.lease_expires_at_ms,
                            activation.user_id,
                            activation.playback_id,
                        ],
                    )? != 1
                    {
                        tx.rollback()?;
                        return Ok(None);
                    }
                    let mut confirmed = route;
                    confirmed.publication_ready_at_ms = publication_ready_at_ms;
                    confirmed.updated_at_ms = now_ms;
                    tx.commit()?;
                    Ok(Some(confirmed))
                }
                MediaSessionActivationSettlement::Abandon => {
                    let exact_route = route
                        .as_ref()
                        .filter(|route| activation_route_matches(route, &activation));
                    if let Some(route) = exact_route {
                        let preserve = if activation.request_id.is_none() {
                            route.publication_ready_at_ms != MEDIA_SESSION_PUBLICATION_BLOCKED
                        } else {
                            let request_state: Option<String> = tx
                                .query_row(
                                    "SELECT state FROM media_session_requests
                                      WHERE user_id = ?1 AND request_id = ?2
                                        AND incarnation_id = ?3 AND request_fingerprint = ?4
                                        AND playback_id = ?5 AND owner_node_id = ?6",
                                    params![
                                        activation.user_id,
                                        activation.request_id.as_deref().unwrap_or_default(),
                                        activation.incarnation_id,
                                        activation.request_fingerprint,
                                        activation.playback_id,
                                        activation.owner_node_id,
                                    ],
                                    |row| row.get(0),
                                )
                                .optional()?;
                            matches!(request_state.as_deref(), Some("resolved"))
                        };
                        if preserve {
                            tx.commit()?;
                            return Ok(Some(route.clone()));
                        }
                        if activation.request_id.is_some()
                            && !matches!(
                                tx.query_row(
                                    "SELECT state FROM media_session_requests
                                      WHERE user_id = ?1 AND request_id = ?2
                                        AND incarnation_id = ?3 AND request_fingerprint = ?4
                                        AND playback_id = ?5 AND owner_node_id = ?6",
                                    params![
                                        activation.user_id,
                                        activation.request_id.as_deref().unwrap_or_default(),
                                        activation.incarnation_id,
                                        activation.request_fingerprint,
                                        activation.playback_id,
                                        activation.owner_node_id,
                                    ],
                                    |row| row.get::<_, String>(0),
                                )
                                .optional()?,
                                Some(state) if state == "starting" || state == "failed"
                            )
                        {
                            tx.commit()?;
                            return Ok(None);
                        }
                    }
                    if let Some(request_id) = activation.request_id.as_deref() {
                        tx.execute(
                            "UPDATE media_session_requests SET state = 'failed', response_json = NULL,
                                    claim_expires_at_ms = ?1, updated_at_ms = ?1
                              WHERE user_id = ?2 AND request_id = ?3 AND incarnation_id = ?4
                                AND request_fingerprint = ?5 AND playback_id = ?6
                                AND owner_node_id = ?7 AND state = 'starting'",
                            params![
                                now_ms,
                                activation.user_id,
                                request_id,
                                activation.incarnation_id,
                                activation.request_fingerprint,
                                activation.playback_id,
                                activation.owner_node_id,
                            ],
                        )?;
                    }
                    if exact_route.is_some_and(|route| route.state == "active") {
                        tx.execute(
                            "UPDATE media_sessions SET state = 'ended', terminal_reason = 'replaced',
                                    lease_expires_at_ms = ?1,
                                    publication_ready_at_ms = ?4, updated_at_ms = ?1
                              WHERE incarnation_id = ?2 AND owner_node_id = ?3 AND owner_epoch = 1
                                AND state = 'active'",
                            params![
                                now_ms,
                                activation.incarnation_id,
                                activation.owner_node_id,
                                MEDIA_SESSION_PUBLICATION_BLOCKED,
                            ],
                        )?;
                        tx.execute(
                            "DELETE FROM media_playback_pointers
                              WHERE user_id = ?1 AND playback_id = ?2
                                AND current_incarnation_id = ?3",
                            params![
                                activation.user_id,
                                activation.playback_id,
                                activation.incarnation_id,
                            ],
                        )?;
                        tx.execute(
                            "UPDATE job_leases SET expires_at_ms = ?1, revision = revision + 1,
                                    updated_at_ms = ?1
                              WHERE resource = ?2 AND owner_node_id = ?3 AND fence = 1
                                AND revision < 9223372036854775807",
                            params![
                                now_ms,
                                format!("session:{}", activation.incarnation_id),
                                activation.owner_node_id,
                            ],
                        )?;
                        tx.execute(
                            "DELETE FROM cache_consumer_pins
                              WHERE consumer_kind = 'media_session' AND consumer_id = ?1",
                            [activation.incarnation_id.as_str()],
                        )?;
                    }
                    tx.commit()?;
                    Ok(None)
                }
            }
        })
        .await
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
        let request_id = request_id.to_owned();
        let incarnation_id = incarnation_id.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let route = tx
                .query_row(
                    &format!(
                        "SELECT {ROUTE_COLS} FROM media_sessions route
                          WHERE route.user_id = ?1 AND route.incarnation_id = ?2
                            AND route.state = 'active'
                            AND route.publication_ready_at_ms = 0
                            AND EXISTS (SELECT 1 FROM media_session_requests request
                              WHERE request.user_id = ?1 AND request.request_id = ?4
                                AND (request.state = 'resolved'
                                  OR (request.state = 'starting'
                                    AND route.lease_expires_at_ms > ?3))
                                AND request.incarnation_id = route.incarnation_id
                                AND request.request_fingerprint = route.request_fingerprint
                                AND request.playback_id = route.playback_id
                                AND (request.state = 'resolved'
                                  OR request.owner_node_id = route.owner_node_id))"
                    ),
                    params![user_id, incarnation_id, now_ms, request_id],
                    route_from_row,
                )
                .optional()?;
            let Some(route) = route else {
                tx.commit()?;
                return Ok(None);
            };
            let changed = tx.execute(
                "UPDATE media_session_requests SET state = 'resolved', response_json = ?1,
                        claim_expires_at_ms = ?2, updated_at_ms = ?3
                  WHERE user_id = ?4 AND request_id = ?5 AND incarnation_id = ?6
                    AND request_fingerprint = ?7 AND playback_id = ?8
                    AND owner_node_id = ?9 AND state = 'starting'",
                params![
                    route.response_json,
                    route.lease_expires_at_ms,
                    now_ms,
                    user_id,
                    request_id,
                    incarnation_id,
                    route.request_fingerprint,
                    route.playback_id,
                    route.owner_node_id,
                ],
            )?;
            if changed == 0 {
                let resolved: Option<String> = tx
                    .query_row(
                        "SELECT state FROM media_session_requests
                          WHERE user_id = ?1 AND request_id = ?2
                            AND incarnation_id = ?3 AND request_fingerprint = ?4
                            AND playback_id = ?5",
                        params![
                            user_id,
                            request_id,
                            incarnation_id,
                            route.request_fingerprint,
                            route.playback_id,
                        ],
                        |row| row.get(0),
                    )
                    .optional()?;
                if resolved.as_deref() != Some("resolved") {
                    tx.commit()?;
                    return Ok(None);
                }
            }
            tx.commit()?;
            Ok(Some(route))
        })
        .await
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
        let incarnation_id = incarnation_id.to_owned();
        let owner_node_id = owner_node_id.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            match proof {
                MediaSessionProjectionCompletion::PredecessorAcknowledged => tx.execute(
                    "UPDATE media_sessions SET publication_ready_at_ms = 0, updated_at_ms = ?1
                      WHERE incarnation_id = ?2 AND owner_node_id = ?3 AND owner_epoch = ?4
                        AND state = 'active' AND publication_ready_at_ms != 0",
                    params![now_ms, incarnation_id, owner_node_id, owner_epoch],
                )?,
                MediaSessionProjectionCompletion::SafetyBoundaryElapsed {
                    expected_not_before_ms,
                } if expected_not_before_ms > 0
                    && expected_not_before_ms != MEDIA_SESSION_PUBLICATION_BLOCKED
                    && expected_not_before_ms <= now_ms =>
                {
                    tx.execute(
                        "UPDATE media_sessions SET publication_ready_at_ms = 0, updated_at_ms = ?1
                          WHERE incarnation_id = ?2 AND owner_node_id = ?3 AND owner_epoch = ?4
                            AND state = 'active' AND publication_ready_at_ms = ?5",
                        params![
                            now_ms,
                            incarnation_id,
                            owner_node_id,
                            owner_epoch,
                            expected_not_before_ms,
                        ],
                    )?
                }
                MediaSessionProjectionCompletion::SafetyBoundaryElapsed { .. } => {
                    return Err(StoreError::Task(
                        "invalid media-session handoff safety proof".to_owned(),
                    ));
                }
            };
            let route = tx
                .query_row(
                    &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"),
                    [incarnation_id.as_str()],
                    route_from_row,
                )
                .optional()?
                .filter(|route| {
                    route.owner_node_id == owner_node_id
                        && route.owner_epoch == owner_epoch
                        && route.state == "active"
                        && route.publication_ready_at_ms == 0
                });
            tx.commit()?;
            Ok(route)
        })
        .await
    }

    async fn prepare_media_session(
        &self,
        preparation: &MediaSessionPreparation,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        validate_preparation(preparation)?;
        let preparation = preparation.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let Some(route) = prepare_within(&tx, &preparation)? else {
                tx.rollback()?;
                return Ok(None);
            };
            tx.commit()?;
            Ok(Some(route))
        })
        .await
    }

    async fn rejoin_media_session_preparation(
        &self,
        staged_incarnation_id: &str,
        preparation: &MediaSessionPreparation,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        validate_preparation(preparation)?;
        if !valid_uuid(staged_incarnation_id) || staged_incarnation_id == preparation.incarnation_id
        {
            return Err(StoreError::Task(
                "invalid media-session preparation rejoin".to_owned(),
            ));
        }
        let staged_incarnation_id = staged_incarnation_id.to_owned();
        let preparation = preparation.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let existing = tx
                .query_row(
                    &format!(
                        "SELECT {STAGED_COLS} FROM media_session_preparations
                          WHERE user_id = ?1 AND playback_id = ?2"
                    ),
                    params![preparation.user_id, preparation.playback_id],
                    staged_from_row,
                )
                .optional()?;
            let exact_replay = existing.as_ref().is_some_and(|staged| {
                staged.staged_incarnation_id == preparation.incarnation_id
                    && staged.expected_predecessor_incarnation_id
                        == preparation.expected_predecessor_incarnation_id
            });
            if exact_replay {
                let route = tx
                    .query_row(
                        &format!(
                            "SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"
                        ),
                        [preparation.incarnation_id.as_str()],
                        route_from_row,
                    )
                    .optional()?
                    .filter(|route| preparation_route_matches(route, &preparation));
                tx.commit()?;
                return Ok(route);
            }
            let named_owns_slot = existing
                .as_ref()
                .is_some_and(|staged| staged.staged_incarnation_id == staged_incarnation_id);
            if !named_owns_slot {
                tx.rollback()?;
                return Ok(None);
            }
            let named_predecessor_matches = existing.as_ref().is_some_and(|staged| {
                staged.expected_predecessor_incarnation_id
                    == preparation.expected_predecessor_incarnation_id
            });
            if !named_predecessor_matches {
                tx.rollback()?;
                return Err(StoreError::Task(
                    "media-session rejoin replacement is no longer admissible".to_owned(),
                ));
            }
            let retired = abort_staged_generation(
                &tx,
                preparation.user_id,
                &preparation.playback_id,
                &staged_incarnation_id,
                preparation.now_ms,
            )?;
            if retired != 1 {
                tx.rollback()?;
                return Err(StoreError::Task(
                    "media-session rejoin replacement is no longer admissible".to_owned(),
                ));
            }
            let route = prepare_within(&tx, &preparation)?;
            // This is the same second-line predicate as explicit abort. SQL
            // ledger guards are the safety boundary; the projection prevents
            // a wrong named incarnation from being reported as a rejoin.
            let named_was_replaced = tx
                .query_row(
                    &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"),
                    [staged_incarnation_id.as_str()],
                    route_from_row,
                )
                .optional()?
                .is_some_and(|route| {
                    route.state == "ended"
                        && route.terminal_reason.as_deref() == Some("replaced")
                        && route.user_id == preparation.user_id
                        && route.playback_id == preparation.playback_id
                });
            let Some(route) = route.filter(|_| named_was_replaced) else {
                tx.rollback()?;
                if named_owns_slot {
                    return Err(StoreError::Task(
                        "media-session rejoin replacement is no longer admissible".to_owned(),
                    ));
                }
                return Ok(None);
            };
            tx.commit()?;
            Ok(Some(route))
        })
        .await
    }

    async fn staged_media_session_for_playback(
        &self,
        user_id: i64,
        playback_id: &str,
    ) -> Result<Option<MediaSessionStagedGeneration>, StoreError> {
        if user_id <= 0 || playback_id.is_empty() || playback_id.len() > 128 {
            return Err(StoreError::Task(
                "invalid staged media-session lookup".to_owned(),
            ));
        }
        let playback_id = playback_id.to_owned();
        self.with_read(move |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {STAGED_COLS} FROM media_session_preparations
                          WHERE user_id = ?1 AND playback_id = ?2"
                    ),
                    params![user_id, playback_id],
                    staged_from_row,
                )
                .optional()?)
        })
        .await
    }

    async fn commit_media_session_preparation(
        &self,
        user_id: i64,
        playback_id: &str,
        request: &MediaSessionPreparationCommitRequest,
    ) -> Result<Option<MediaSessionPreparationCommit>, StoreError> {
        if user_id <= 0
            || playback_id.is_empty()
            || playback_id.len() > 128
            || !valid_uuid(&request.staged_incarnation_id)
            || request.expected_predecessor_owner_node_id.is_empty()
            || request.expected_predecessor_owner_node_id.len() > 256
            || request.expected_predecessor_owner_epoch <= 0
            || request.now_ms <= 0
            || request.lease_expires_at_ms <= request.now_ms
            || request.control_receipt.as_ref().is_some_and(|receipt| {
                !valid_terminal_ack(receipt)
                    || receipt.owner_node_id != request.expected_predecessor_owner_node_id
                    || receipt.owner_epoch != request.expected_predecessor_owner_epoch
            })
        {
            return Err(StoreError::Task(
                "invalid media-session preparation commit".to_owned(),
            ));
        }
        let playback_id = playback_id.to_owned();
        let request = request.clone();
        self.with_conn(move |conn| {
            let staged_incarnation_id = request.staged_incarnation_id.as_str();
            let now_ms = request.now_ms;
            let lease_expires_at_ms = request.lease_expires_at_ms;
            let tx = conn.unchecked_transaction()?;
            let staged = tx
                .query_row(
                    &format!(
                        "SELECT {STAGED_COLS} FROM media_session_preparations
                          WHERE user_id = ?1 AND playback_id = ?2
                            AND staged_incarnation_id = ?3"
                    ),
                    params![user_id, playback_id, staged_incarnation_id],
                    staged_from_row,
                )
                .optional()?;
            let Some(staged) = staged else {
                // Either it was never staged, or an earlier commit already
                // moved it. The replay is the pointer itself: if this
                // incarnation is what the pointer names, this exact commit
                // already happened and its outcome is the truthful answer.
                let route = tx
                    .query_row(
                        &format!(
                            "SELECT {ROUTE_COLS} FROM media_sessions
                              WHERE incarnation_id = (SELECT current_incarnation_id
                                FROM media_playback_pointers
                                 WHERE user_id = ?1 AND playback_id = ?2)
                                AND incarnation_id = ?3"
                        ),
                        params![user_id, playback_id, staged_incarnation_id],
                        route_from_row,
                    )
                    .optional()?;
                let control_receipt = if let Some(expected) = &request.control_receipt {
                    let stored = tx
                        .query_row(
                            "SELECT incarnation_id, session_id, owner_node_id, owner_epoch,
                                    client_instance_id, sequence, request_fingerprint, response_json,
                                    expires_at_ms, updated_at_ms
                               FROM media_session_terminal_acks WHERE session_id = ?1",
                            [expected.session_id.as_str()],
                            terminal_ack_from_row,
                        )
                        .optional()?;
                    if stored.as_ref() != Some(expected) {
                        tx.rollback()?;
                        return Ok(None);
                    }
                    stored
                } else {
                    None
                };
                tx.commit()?;
                return Ok(route.map(|route| MediaSessionPreparationCommit {
                    route,
                    predecessor: None,
                    control_receipt,
                }));
            };
            // The CAS this whole milestone exists for. The predecessor comes
            // from the ledger, never from a fresh read of the pointer: a
            // pointer that moved since the preparation means a newer player
            // generation is current, and reaping it would be the exact bug
            // that made a flag on `activate_media_session` unacceptable.
            let pointer_advanced = tx.execute(
                "UPDATE media_playback_pointers
                    SET current_incarnation_id = ?1, updated_at_ms = ?2
                  WHERE user_id = ?3 AND playback_id = ?4
                    AND current_incarnation_id = ?5
                    AND EXISTS (SELECT 1 FROM media_sessions predecessor
                      WHERE predecessor.incarnation_id = ?5
                        AND predecessor.owner_node_id = ?6
                        AND predecessor.owner_epoch = ?7
                        AND predecessor.state = 'active')
                    AND EXISTS (SELECT 1 FROM media_session_preparations preparation
                      WHERE preparation.user_id = ?3 AND preparation.playback_id = ?4
                        AND preparation.staged_incarnation_id = ?1
                        AND preparation.deadline_ms > ?2)
                    AND EXISTS (SELECT 1 FROM media_sessions
                      WHERE incarnation_id = ?1 AND user_id = ?3
                        AND playback_id = ?4 AND state = 'active')",
                params![
                    staged.staged_incarnation_id,
                    now_ms,
                    user_id,
                    playback_id,
                    staged.expected_predecessor_incarnation_id,
                    request.expected_predecessor_owner_node_id,
                    request.expected_predecessor_owner_epoch,
                ],
            )?;
            if pointer_advanced != 1 {
                // A newer generation exists. Abort the staged successor rather
                // than reap it — see the trait doc; this branch is the reason
                // that doc is as long as it is.
                //
                // The same body an explicit abort runs, rather than a second
                // spelling of it: the first version of this branch ended the
                // row and dropped the ledger entry but left the successor's
                // cache pins and job lease behind, which is a divergence from
                // the replicated twin for the same input.
                let predecessor_still_owned = tx
                    .query_row(
                        "SELECT 1 FROM media_playback_pointers pointer
                          JOIN media_sessions predecessor
                            ON predecessor.incarnation_id = pointer.current_incarnation_id
                         WHERE pointer.user_id = ?1 AND pointer.playback_id = ?2
                           AND pointer.current_incarnation_id = ?3
                           AND predecessor.owner_node_id = ?4
                           AND predecessor.owner_epoch = ?5",
                        params![
                            user_id,
                            playback_id,
                            staged.expected_predecessor_incarnation_id,
                            request.expected_predecessor_owner_node_id,
                            request.expected_predecessor_owner_epoch,
                        ],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                let pointer_moved = tx
                    .query_row(
                        "SELECT current_incarnation_id FROM media_playback_pointers
                          WHERE user_id = ?1 AND playback_id = ?2",
                        params![user_id, playback_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .as_deref()
                    != Some(staged.expected_predecessor_incarnation_id.as_str());
                if predecessor_still_owned || pointer_moved {
                    abort_staged_generation(
                        &tx,
                        user_id,
                        &playback_id,
                        &staged.staged_incarnation_id,
                        now_ms,
                    )?;
                }
                tx.commit()?;
                return Ok(None);
            }
            let staged_owner: String = tx.query_row(
                "SELECT owner_node_id FROM media_sessions WHERE incarnation_id = ?1",
                [staged.staged_incarnation_id.as_str()],
                |row| row.get(0),
            )?;
            let predecessor = tx
                .query_row(
                    &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"),
                    [staged.expected_predecessor_incarnation_id.as_str()],
                    route_from_row,
                )
                .optional()?;
            // The same retirement an activation performs, against an exact
            // named incarnation rather than whatever the pointer held. That
            // difference is the one the two backends disagreed about, and it
            // is settled here by not consulting the pointer at all.
            tx.execute(
                "UPDATE media_sessions
                    SET state = 'ended', terminal_reason = 'superseded',
                        lease_expires_at_ms = ?1, publication_ready_at_ms = ?3,
                        updated_at_ms = ?1
                  WHERE incarnation_id = ?2 AND state != 'ended'",
                params![
                    now_ms,
                    staged.expected_predecessor_incarnation_id,
                    MEDIA_SESSION_PUBLICATION_BLOCKED,
                ],
            )?;
            tx.execute(
                "DELETE FROM cache_consumer_pins
                  WHERE consumer_kind = 'media_session' AND consumer_id = ?1",
                params![staged.expected_predecessor_incarnation_id],
            )?;
            tx.execute(
                "UPDATE job_leases
                    SET expires_at_ms = CASE
                          WHEN expires_at_ms < ?1 THEN expires_at_ms ELSE ?1 END,
                        revision = revision + 1, updated_at_ms = ?1
                  WHERE resource = ?2 AND revision < 9223372036854775807",
                params![
                    now_ms,
                    format!("session:{}", staged.expected_predecessor_incarnation_id),
                ],
            )?;
            // The successor stops being a candidate and starts being a
            // stream, so it stops carrying the candidate's clock. Both halves
            // move: the row, and the `job_leases` fence renewal reads.
            tx.execute(
                "UPDATE media_sessions
                    SET lease_expires_at_ms = ?1, updated_at_ms = ?2
                  WHERE incarnation_id = ?3 AND state = 'active'",
                params![lease_expires_at_ms, now_ms, staged.staged_incarnation_id],
            )?;
            tx.execute(
                "UPDATE job_leases
                    SET expires_at_ms = ?1, revision = revision + 1, updated_at_ms = ?2
                  WHERE resource = ?3 AND owner_node_id = ?4 AND fence = 1
                    AND revision < 9223372036854775807",
                params![
                    lease_expires_at_ms,
                    now_ms,
                    format!("session:{}", staged.staged_incarnation_id),
                    staged_owner,
                ],
            )?;
            tx.execute(
                "DELETE FROM media_session_preparations
                  WHERE user_id = ?1 AND playback_id = ?2 AND staged_incarnation_id = ?3",
                params![user_id, playback_id, staged.staged_incarnation_id],
            )?;
            if let Some(receipt) = &request.control_receipt {
                if receipt.incarnation_id != staged.expected_predecessor_incarnation_id {
                    tx.rollback()?;
                    return Ok(None);
                }
                tx.execute(
                    "INSERT OR IGNORE INTO media_session_terminal_acks
                        (incarnation_id, session_id, owner_node_id, owner_epoch,
                         client_instance_id, sequence, request_fingerprint, response_json,
                         expires_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        receipt.incarnation_id,
                        receipt.session_id,
                        receipt.owner_node_id,
                        receipt.owner_epoch,
                        receipt.client_instance_id,
                        receipt.sequence,
                        receipt.request_fingerprint,
                        receipt.response_json,
                        receipt.expires_at_ms,
                        receipt.updated_at_ms,
                    ],
                )?;
                let stored = tx
                    .query_row(
                        "SELECT incarnation_id, session_id, owner_node_id, owner_epoch,
                                client_instance_id, sequence, request_fingerprint, response_json,
                                expires_at_ms, updated_at_ms
                           FROM media_session_terminal_acks WHERE session_id = ?1",
                        [receipt.session_id.as_str()],
                        terminal_ack_from_row,
                    )
                    .optional()?;
                if stored.as_ref() != Some(receipt) {
                    tx.rollback()?;
                    return Ok(None);
                }
            }
            // The exact post-commit projection: the route, and the pointer
            // that now names it. Derived from a re-read rather than from
            // `rows_affected`, so a replay reads the same as a first commit.
            let route = tx
                .query_row(
                    &format!(
                        "SELECT {ROUTE_COLS} FROM media_sessions
                          WHERE incarnation_id = (SELECT current_incarnation_id
                            FROM media_playback_pointers
                             WHERE user_id = ?1 AND playback_id = ?2)
                            AND incarnation_id = ?3"
                    ),
                    params![user_id, playback_id, staged.staged_incarnation_id],
                    route_from_row,
                )
                .optional()?
                .filter(|route| route.state == "active");
            let Some(route) = route else {
                // Unreachable in practice: the pointer CAS above already
                // required the staged row to be `active`, and that branch
                // aborts. Kept because a projection that cannot be read back
                // must never commit — and a rollback here is the right
                // outcome, since it also restores the ledger row rather than
                // leaving a preparation half-consumed.
                tx.rollback()?;
                return Ok(None);
            };
            let predecessor = match predecessor {
                Some(previous) => tx
                    .query_row(
                        &format!(
                            "SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"
                        ),
                        [previous.incarnation_id.as_str()],
                        route_from_row,
                    )
                    .optional()?,
                None => None,
            };
            let control_receipt = request.control_receipt.clone();
            tx.commit()?;
            Ok(Some(MediaSessionPreparationCommit {
                route,
                predecessor,
                control_receipt,
            }))
        })
        .await
    }

    async fn abort_media_session_preparation(
        &self,
        user_id: i64,
        playback_id: &str,
        request: &MediaSessionPreparationAbortRequest,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if user_id <= 0
            || playback_id.is_empty()
            || playback_id.len() > 128
            || !valid_uuid(&request.staged_incarnation_id)
            || request.expected_predecessor_owner_node_id.is_empty()
            || request.expected_predecessor_owner_node_id.len() > 256
            || request.expected_predecessor_owner_epoch <= 0
            || request.now_ms <= 0
        {
            return Err(StoreError::Task(
                "invalid media-session preparation abort".to_owned(),
            ));
        }
        let playback_id = playback_id.to_owned();
        let request = request.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let staged = tx
                .query_row(
                    &format!(
                        "SELECT {STAGED_COLS} FROM media_session_preparations
                          WHERE user_id = ?1 AND playback_id = ?2
                            AND staged_incarnation_id = ?3"
                    ),
                    params![user_id, playback_id, request.staged_incarnation_id],
                    staged_from_row,
                )
                .optional()?;
            let authorized = match &staged {
                Some(staged) => {
                    let pointer_owner_matches = tx
                        .query_row(
                            "SELECT 1 FROM media_playback_pointers pointer
                              JOIN media_sessions predecessor
                                ON predecessor.incarnation_id = pointer.current_incarnation_id
                             WHERE pointer.user_id = ?1 AND pointer.playback_id = ?2
                               AND pointer.current_incarnation_id = ?3
                               AND predecessor.owner_node_id = ?4
                               AND predecessor.owner_epoch = ?5",
                            params![
                                user_id,
                                playback_id,
                                staged.expected_predecessor_incarnation_id,
                                request.expected_predecessor_owner_node_id,
                                request.expected_predecessor_owner_epoch,
                            ],
                            |_| Ok(()),
                        )
                        .optional()?
                        .is_some();
                    let pointer_moved = tx
                        .query_row(
                            "SELECT current_incarnation_id FROM media_playback_pointers
                              WHERE user_id = ?1 AND playback_id = ?2",
                            params![user_id, playback_id],
                            |row| row.get::<_, String>(0),
                        )
                        .optional()?
                        .as_deref()
                        != Some(staged.expected_predecessor_incarnation_id.as_str());
                    pointer_owner_matches || pointer_moved
                }
                None => true,
            };
            if authorized {
                abort_staged_generation(
                    &tx,
                    user_id,
                    &playback_id,
                    &request.staged_incarnation_id,
                    request.now_ms,
                )?;
            }
            // The predicate is a second line, not the first: every write above
            // is itself ledger-gated. This reports success only for a row that
            // is ended, carries the abort's own terminal cause, and belongs to
            // the playback the caller named.
            let route = tx
                .query_row(
                    &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"),
                    [request.staged_incarnation_id.as_str()],
                    route_from_row,
                )
                .optional()?
                .filter(|route| {
                    route.state == "ended"
                        && route.terminal_reason.as_deref() == Some("replaced")
                        && route.user_id == user_id
                        && route.playback_id == playback_id
                });
            tx.commit()?;
            Ok(route)
        })
        .await
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
        let incarnation_id = incarnation_id.to_owned();
        let owner_node_id = owner_node_id.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE media_sessions SET publication_ready_at_ms = ?1, updated_at_ms = ?2
                  WHERE incarnation_id = ?3 AND owner_node_id = ?4 AND owner_epoch = ?5
                    AND state = 'active' AND publication_ready_at_ms = ?6",
                params![
                    publication_ready_at_ms,
                    now_ms,
                    incarnation_id,
                    owner_node_id,
                    owner_epoch,
                    MEDIA_SESSION_PUBLICATION_BLOCKED,
                ],
            )?;
            let route = tx
                .query_row(
                    &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"),
                    [incarnation_id.as_str()],
                    route_from_row,
                )
                .optional()?
                .filter(|route| {
                    route.owner_node_id == owner_node_id
                        && route.owner_epoch == owner_epoch
                        && route.state == "active"
                        && route.publication_ready_at_ms != MEDIA_SESSION_PUBLICATION_BLOCKED
                });
            tx.commit()?;
            Ok(route)
        })
        .await
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
        let incarnation_id = incarnation_id.to_owned();
        let owner_node_id = owner_node_id.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE media_sessions SET publication_ready_at_ms = ?1, updated_at_ms = ?2
                  WHERE incarnation_id = ?3 AND owner_node_id = ?4 AND owner_epoch = ?5
                    AND state = 'ended' AND publication_ready_at_ms = ?6",
                params![
                    projection_safe_at_ms,
                    now_ms,
                    incarnation_id,
                    owner_node_id,
                    owner_epoch,
                    MEDIA_SESSION_PUBLICATION_BLOCKED,
                ],
            )?;
            let route = tx
                .query_row(
                    &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"),
                    [incarnation_id.as_str()],
                    route_from_row,
                )
                .optional()?
                .filter(|route| {
                    route.owner_node_id == owner_node_id
                        && route.owner_epoch == owner_epoch
                        && route.state == "ended"
                        && route.publication_ready_at_ms != MEDIA_SESSION_PUBLICATION_BLOCKED
                });
            tx.commit()?;
            Ok(route)
        })
        .await
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
        let incarnation_id = incarnation_id.to_owned();
        let owner_node_id = owner_node_id.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            match proof {
                MediaSessionProjectionCompletion::PredecessorAcknowledged => tx.execute(
                    "UPDATE media_sessions SET publication_ready_at_ms = 0, updated_at_ms = ?1
                      WHERE incarnation_id = ?2 AND owner_node_id = ?3 AND owner_epoch = ?4
                        AND state = 'ended' AND publication_ready_at_ms != 0",
                    params![now_ms, incarnation_id, owner_node_id, owner_epoch],
                )?,
                MediaSessionProjectionCompletion::SafetyBoundaryElapsed {
                    expected_not_before_ms,
                } if expected_not_before_ms > 0
                    && expected_not_before_ms != MEDIA_SESSION_PUBLICATION_BLOCKED
                    && expected_not_before_ms <= now_ms =>
                {
                    tx.execute(
                        "UPDATE media_sessions SET publication_ready_at_ms = 0, updated_at_ms = ?1
                          WHERE incarnation_id = ?2 AND owner_node_id = ?3 AND owner_epoch = ?4
                            AND state = 'ended' AND publication_ready_at_ms = ?5",
                        params![
                            now_ms,
                            incarnation_id,
                            owner_node_id,
                            owner_epoch,
                            expected_not_before_ms,
                        ],
                    )?
                }
                MediaSessionProjectionCompletion::SafetyBoundaryElapsed { .. } => {
                    return Err(StoreError::Task(
                        "invalid media-session terminal projection safety proof".to_owned(),
                    ));
                }
            };
            let route = tx
                .query_row(
                    &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"),
                    [incarnation_id.as_str()],
                    route_from_row,
                )
                .optional()?
                .filter(|route| {
                    route.owner_node_id == owner_node_id
                        && route.owner_epoch == owner_epoch
                        && route.state == "ended"
                        && route.publication_ready_at_ms == 0
                });
            tx.commit()?;
            Ok(route)
        })
        .await
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
        let request_id = request_id.to_owned();
        let incarnation_id = incarnation_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE media_session_requests SET state = 'failed', claim_expires_at_ms = ?1,
                        updated_at_ms = ?1
                  WHERE user_id = ?2 AND request_id = ?3 AND incarnation_id = ?4
                    AND state = 'starting'",
                params![now_ms, user_id, request_id, incarnation_id],
            )? == 1)
        })
        .await
    }

    async fn media_session_route(
        &self,
        session_id: &str,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if !valid_uuid(session_id) {
            return Ok(None);
        }
        let session_id = session_id.to_owned();
        self.with_read(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE session_id = ?1"),
                    [session_id],
                    route_from_row,
                )
                .optional()?)
        })
        .await
    }

    async fn media_session_route_by_incarnation(
        &self,
        incarnation_id: &str,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if !valid_uuid(incarnation_id) {
            return Ok(None);
        }
        let incarnation_id = incarnation_id.to_owned();
        self.with_read(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"),
                    [incarnation_id],
                    route_from_row,
                )
                .optional()?)
        })
        .await
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
        let playback_id = playback_id.to_owned();
        self.with_read(move |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {ROUTE_COLS} FROM media_sessions
                          WHERE incarnation_id = (SELECT current_incarnation_id
                            FROM media_playback_pointers
                            WHERE user_id = ?1 AND playback_id = ?2)"
                    ),
                    params![user_id, playback_id],
                    route_from_row,
                )
                .optional()?)
        })
        .await
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
        let acknowledgement = acknowledgement.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "INSERT OR IGNORE INTO media_session_terminal_acks
                    (incarnation_id, session_id, owner_node_id, owner_epoch,
                     client_instance_id, sequence, request_fingerprint, response_json,
                     expires_at_ms, updated_at_ms)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10
                   WHERE EXISTS (SELECT 1 FROM media_sessions
                     WHERE incarnation_id = ?1 AND session_id = ?2
                       AND owner_node_id = ?3 AND owner_epoch = ?4
                       AND state IN ('active', 'ended'))",
                params![
                    acknowledgement.incarnation_id.as_str(),
                    acknowledgement.session_id.as_str(),
                    acknowledgement.owner_node_id.as_str(),
                    acknowledgement.owner_epoch,
                    acknowledgement.client_instance_id.as_str(),
                    acknowledgement.sequence,
                    acknowledgement.request_fingerprint.as_str(),
                    acknowledgement.response_json.as_str(),
                    acknowledgement.expires_at_ms,
                    acknowledgement.updated_at_ms,
                ],
            )?;
            let stored = tx
                .query_row(
                    "SELECT incarnation_id, session_id, owner_node_id, owner_epoch,
                            client_instance_id, sequence, request_fingerprint, response_json,
                            expires_at_ms, updated_at_ms
                       FROM media_session_terminal_acks WHERE session_id = ?1",
                    [acknowledgement.session_id.as_str()],
                    terminal_ack_from_row,
                )
                .optional()?;
            let exact = stored.as_ref() == Some(&acknowledgement);
            if exact {
                let lease_resource = format!("session:{}", acknowledgement.incarnation_id);
                tx.execute(
                    "UPDATE media_sessions SET state = 'ended',
                            terminal_reason = COALESCE(terminal_reason, 'deleted'), lease_expires_at_ms = ?1,
                            publication_ready_at_ms = 0, updated_at_ms = ?1
                      WHERE incarnation_id = ?2 AND session_id = ?3
                        AND owner_node_id = ?4 AND owner_epoch = ?5
                        AND state IN ('active', 'ended')
                        AND EXISTS (SELECT 1 FROM media_session_terminal_acks
                          WHERE session_id = ?3 AND incarnation_id = ?2
                            AND owner_node_id = ?4 AND owner_epoch = ?5
                            AND client_instance_id = ?6 AND sequence = ?7
                            AND request_fingerprint = ?8 AND response_json = ?9
                            AND expires_at_ms = ?10 AND updated_at_ms = ?1)",
                    params![
                        acknowledgement.updated_at_ms,
                        acknowledgement.incarnation_id,
                        acknowledgement.session_id,
                        acknowledgement.owner_node_id,
                        acknowledgement.owner_epoch,
                        acknowledgement.client_instance_id,
                        acknowledgement.sequence,
                        acknowledgement.request_fingerprint,
                        acknowledgement.response_json,
                        acknowledgement.expires_at_ms,
                    ],
                )?;
                tx.execute(
                    "UPDATE job_leases
                        SET expires_at_ms = CASE
                              WHEN expires_at_ms < ?1 THEN expires_at_ms ELSE ?1 END,
                            revision = revision + 1, updated_at_ms = ?1
                      WHERE resource = ?2 AND owner_node_id = ?3 AND fence = ?4
                        AND revision < 9223372036854775807
                        AND expires_at_ms > ?1
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = ?5 AND session_id = ?6
                            AND owner_node_id = ?3 AND owner_epoch = ?4
                            AND state = 'ended' AND updated_at_ms = ?1)",
                    params![
                        acknowledgement.updated_at_ms,
                        lease_resource,
                        acknowledgement.owner_node_id,
                        acknowledgement.owner_epoch,
                        acknowledgement.incarnation_id,
                        acknowledgement.session_id,
                    ],
                )?;
                tx.execute(
                    "DELETE FROM media_playback_pointers
                      WHERE current_incarnation_id = ?1
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = ?1 AND session_id = ?2
                            AND owner_node_id = ?3 AND owner_epoch = ?4
                            AND state = 'ended' AND updated_at_ms = ?5)",
                    params![
                        acknowledgement.incarnation_id,
                        acknowledgement.session_id,
                        acknowledgement.owner_node_id,
                        acknowledgement.owner_epoch,
                        acknowledgement.updated_at_ms,
                    ],
                )?;
                tx.execute(
                    "DELETE FROM cache_consumer_pins
                      WHERE consumer_kind = 'media_session' AND consumer_id = ?1
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = ?1 AND session_id = ?2
                            AND owner_node_id = ?3 AND owner_epoch = ?4
                            AND state = 'ended' AND updated_at_ms = ?5)",
                    params![
                        acknowledgement.incarnation_id,
                        acknowledgement.session_id,
                        acknowledgement.owner_node_id,
                        acknowledgement.owner_epoch,
                        acknowledgement.updated_at_ms,
                    ],
                )?;
            }
            tx.commit()?;
            Ok(exact)
        })
        .await
    }

    async fn media_session_terminal_ack(
        &self,
        session_id: &str,
        now_ms: i64,
    ) -> Result<Option<MediaSessionTerminalAck>, StoreError> {
        if !valid_uuid(session_id) || now_ms <= 0 {
            return Ok(None);
        }
        let session_id = session_id.to_owned();
        self.with_read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT incarnation_id, session_id, owner_node_id, owner_epoch,
                            client_instance_id, sequence, request_fingerprint, response_json,
                            expires_at_ms, updated_at_ms
                       FROM media_session_terminal_acks
                      WHERE session_id = ?1 AND expires_at_ms > ?2",
                    params![session_id, now_ms],
                    terminal_ack_from_row,
                )
                .optional()?)
        })
        .await
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
        let owner_node_id = owner_node_id.to_owned();
        let renewals = renewals.to_vec();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let mut renewed = Vec::new();
            for renewal in renewals {
                let lease_resource = format!("session:{}", renewal.incarnation_id);
                let lease_renewed = tx.execute(
                    "UPDATE job_leases SET expires_at_ms = ?1, revision = revision + 1,
                            updated_at_ms = ?2
                      WHERE resource = ?3 AND owner_node_id = ?4 AND fence = ?5
                        AND expires_at_ms > ?2 AND revision < 9223372036854775807
                        AND expires_at_ms = (SELECT lease_expires_at_ms FROM media_sessions
                          WHERE incarnation_id = ?6 AND owner_node_id = ?4 AND owner_epoch = ?5
                            AND state = 'active' AND lease_expires_at_ms > ?2
                            AND publication_ready_at_ms != ?7
                            -- A staged successor's deadline is its whole life,
                            -- and renewing it would be the one thing that can
                            -- make that deadline never arrive. The ledger's
                            -- reaper below says the retirement sweep ends a
                            -- staged row whose deadline passed like any other
                            -- active row — true only while nothing renews it.
                            -- The owner node holds a real VOD session for the
                            -- successor, so it is in `renewable_session_ids`
                            -- and would otherwise be pushed forward every tick
                            -- for as long as the node lives, holding an encoder
                            -- and an admission slot nobody will ever commit.
                            -- Keyed on the ledger row, which commit deletes, so
                            -- a committed successor renews normally from its
                            -- first tick after the commit.
                            AND NOT EXISTS (SELECT 1 FROM media_session_preparations staged
                              WHERE staged.staged_incarnation_id = media_sessions.incarnation_id)
                            AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                              WHERE request.user_id = media_sessions.user_id
                                AND request.incarnation_id = media_sessions.incarnation_id
                                AND request.state = 'starting'
                                AND media_sessions.publication_ready_at_ms = 0
                                AND request.claim_expires_at_ms <= media_sessions.lease_expires_at_ms))",
                    params![
                        lease_expires_at_ms,
                        now_ms,
                        lease_resource,
                        owner_node_id,
                        renewal.owner_epoch,
                        renewal.incarnation_id,
                        MEDIA_SESSION_PUBLICATION_BLOCKED,
                    ],
                )? == 1;
                if lease_renewed
                    && tx.execute(
                        "UPDATE media_sessions SET lease_expires_at_ms = ?1, updated_at_ms = ?2,
                              produced_playable_through_ms = MAX(
                                produced_playable_through_ms, ?7),
                              fetched_through_ms = MAX(fetched_through_ms, ?8),
                              media_sequence = MAX(media_sequence, ?9)
                      WHERE incarnation_id = ?3 AND owner_node_id = ?4 AND owner_epoch = ?5
                        AND state = 'active' AND lease_expires_at_ms > ?2
                        AND publication_ready_at_ms != ?10
                        AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                          WHERE request.user_id = media_sessions.user_id
                            AND request.incarnation_id = media_sessions.incarnation_id
                            AND request.state = 'starting'
                            AND media_sessions.publication_ready_at_ms = 0
                            AND request.claim_expires_at_ms <= media_sessions.lease_expires_at_ms)
                        AND EXISTS (SELECT 1 FROM job_leases
                          WHERE resource = ?6 AND owner_node_id = ?4 AND fence = ?5
                            AND expires_at_ms = ?1)",
                        params![
                            lease_expires_at_ms,
                            now_ms,
                            renewal.incarnation_id,
                            owner_node_id,
                            renewal.owner_epoch,
                            lease_resource,
                            renewal.produced_playable_through_ms,
                            renewal.fetched_through_ms,
                            renewal.media_sequence,
                            MEDIA_SESSION_PUBLICATION_BLOCKED,
                        ],
                    )? == 1
                {
                    tx.execute(
                        "UPDATE cache_consumer_pins
                            SET expires_at_ms = CASE
                                  WHEN expires_at_ms < ?1 THEN ?1 ELSE expires_at_ms END
                          WHERE consumer_kind = 'media_session' AND consumer_id = ?2
                            AND consumer_epoch = ?3 AND expires_at_ms > ?4
                            AND EXISTS (
                                SELECT 1 FROM transcode_cache_locations location
                                 WHERE location.storage_id = cache_consumer_pins.storage_id
                                   AND location.recipe_hash = cache_consumer_pins.recipe_hash
                                   AND location.generation_id = cache_consumer_pins.generation_id
                                   AND location.storage_class = 'shared'
                                   AND location.complete = 1)
                            AND EXISTS (
                                SELECT 1 FROM media_sessions session
                                 WHERE session.incarnation_id = ?2
                                   AND session.owner_node_id = ?5
                                   AND session.owner_epoch = ?3
                                   AND session.state = 'active'
                                   AND session.lease_expires_at_ms = ?1
                                   AND session.publication_ready_at_ms != ?6
                                   AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                                     WHERE request.user_id = session.user_id
                                       AND request.incarnation_id = session.incarnation_id
                                       AND request.state = 'starting'
                                       AND session.publication_ready_at_ms = 0
                                       AND request.claim_expires_at_ms <= session.lease_expires_at_ms))",
                        params![
                            lease_expires_at_ms,
                            renewal.incarnation_id,
                            renewal.owner_epoch,
                            now_ms,
                            owner_node_id,
                            MEDIA_SESSION_PUBLICATION_BLOCKED,
                        ],
                    )?;
                    renewed.push(renewal.incarnation_id);
                }
            }
            tx.commit()?;
            Ok(renewed)
        })
        .await
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
        self.with_read(move |conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {ROUTE_COLS} FROM media_sessions
                  WHERE state = 'active' AND lease_expires_at_ms <= ?1
                    AND publication_ready_at_ms != ?6
                    AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                      WHERE request.user_id = media_sessions.user_id
                        AND request.incarnation_id = media_sessions.incarnation_id
                        AND request.state = 'starting'
                        AND media_sessions.publication_ready_at_ms = 0
                        AND request.claim_expires_at_ms <= media_sessions.lease_expires_at_ms)
                    AND (?2 = 0 OR lease_expires_at_ms > ?3
                      OR (lease_expires_at_ms = ?3 AND incarnation_id > ?4))
                  ORDER BY lease_expires_at_ms, incarnation_id LIMIT ?5"
            ))?;
            let routes = statement
                .query_map(
                    params![
                        now_ms,
                        has_cursor,
                        after_lease_expires_at_ms,
                        after_incarnation_id,
                        limit,
                        MEDIA_SESSION_PUBLICATION_BLOCKED,
                    ],
                    route_from_row,
                )?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(routes)
        })
        .await
    }

    async fn claim_media_session_takeover(
        &self,
        takeover: &MediaSessionTakeover,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        validate_takeover(takeover)?;
        let takeover = takeover.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let lease_resource = format!("session:{}", takeover.incarnation_id);
            let removed_owner_key = removed_job_owner_key(&takeover.next_owner_node_id);
            let route = tx
                .query_row(
                    &format!(
                        "SELECT {ROUTE_COLS} FROM media_sessions session
                          WHERE incarnation_id = ?1 AND owner_node_id = ?2
                            AND owner_epoch = ?3 AND state = 'active'
                            AND lease_expires_at_ms <= ?4
                            AND publication_ready_at_ms != ?6
                            AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                              WHERE request.user_id = session.user_id
                                AND request.incarnation_id = session.incarnation_id
                                AND request.state = 'starting'
                                AND session.publication_ready_at_ms = 0
                                AND request.claim_expires_at_ms <= session.lease_expires_at_ms)
                            AND EXISTS (SELECT 1 FROM job_leases lease
                              WHERE lease.resource = ?5
                                AND lease.owner_node_id = session.owner_node_id
                                AND lease.fence = session.owner_epoch
                                AND lease.expires_at_ms = session.lease_expires_at_ms
                                AND lease.expires_at_ms <= ?4
                                AND lease.fence < 9223372036854775807
                                AND lease.revision < 9223372036854775807)"
                    ),
                    params![
                        takeover.incarnation_id,
                        takeover.expected_owner_node_id,
                        takeover.expected_owner_epoch,
                        takeover.now_ms,
                        lease_resource,
                        MEDIA_SESSION_PUBLICATION_BLOCKED,
                    ],
                    route_from_row,
                )
                .optional()?;
            let Some(mut route) = route else {
                tx.commit()?;
                return Ok(None);
            };
            let next_epoch = takeover.expected_owner_epoch.saturating_add(1);
            if tx.execute(
                "UPDATE job_leases
                    SET owner_node_id = ?1, fence = ?2, revision = revision + 1,
                        expires_at_ms = ?3, updated_at_ms = ?4
                  WHERE resource = ?5 AND owner_node_id = ?6 AND fence = ?7
                    AND expires_at_ms = ?8 AND expires_at_ms <= ?4
                    AND fence < 9223372036854775807
                    AND revision < 9223372036854775807
                    AND NOT EXISTS (SELECT 1 FROM settings WHERE key = ?9)",
                params![
                    takeover.next_owner_node_id,
                    next_epoch,
                    takeover.lease_expires_at_ms,
                    takeover.now_ms,
                    lease_resource,
                    takeover.expected_owner_node_id,
                    takeover.expected_owner_epoch,
                    route.lease_expires_at_ms,
                    removed_owner_key,
                ],
            )? != 1
            {
                tx.rollback()?;
                return Ok(None);
            }
            if tx.execute(
                "UPDATE media_sessions
                    SET owner_node_id = ?1, owner_epoch = ?2,
                        lease_expires_at_ms = ?3,
                        discontinuity_sequence = discontinuity_sequence + 1,
                        updated_at_ms = ?4
                  WHERE incarnation_id = ?5 AND owner_node_id = ?6 AND owner_epoch = ?7
                    AND state = 'active' AND lease_expires_at_ms = ?8
                    AND lease_expires_at_ms <= ?4
                    AND publication_ready_at_ms != ?10
                    AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                      WHERE request.user_id = media_sessions.user_id
                        AND request.incarnation_id = media_sessions.incarnation_id
                        AND request.state = 'starting'
                        AND media_sessions.publication_ready_at_ms = 0
                        AND request.claim_expires_at_ms <= media_sessions.lease_expires_at_ms)
                    AND discontinuity_sequence < 9223372036854775807
                    AND EXISTS (SELECT 1 FROM job_leases
                      WHERE resource = ?9 AND owner_node_id = ?1 AND fence = ?2
                        AND expires_at_ms = ?3 AND updated_at_ms = ?4)",
                params![
                    takeover.next_owner_node_id,
                    next_epoch,
                    takeover.lease_expires_at_ms,
                    takeover.now_ms,
                    takeover.incarnation_id,
                    takeover.expected_owner_node_id,
                    takeover.expected_owner_epoch,
                    route.lease_expires_at_ms,
                    lease_resource,
                    MEDIA_SESSION_PUBLICATION_BLOCKED,
                ],
            )? != 1
            {
                tx.rollback()?;
                return Ok(None);
            }
            let starting_requests: i64 = tx.query_row(
                "SELECT COUNT(*) FROM media_session_requests
                  WHERE user_id = ?1 AND incarnation_id = ?2
                    AND request_fingerprint = ?3 AND playback_id = ?4
                    AND state = 'starting'",
                params![
                    route.user_id,
                    takeover.incarnation_id,
                    route.request_fingerprint,
                    route.playback_id,
                ],
                |row| row.get(0),
            )?;
            if starting_requests > 1
                || (starting_requests == 1
                    && tx.execute(
                        "UPDATE media_session_requests
                            SET owner_node_id = ?1, updated_at_ms = ?2
                          WHERE user_id = ?3 AND incarnation_id = ?4
                            AND request_fingerprint = ?5 AND playback_id = ?6
                            AND owner_node_id = ?7 AND state = 'starting'",
                        params![
                            takeover.next_owner_node_id,
                            takeover.now_ms,
                            route.user_id,
                            takeover.incarnation_id,
                            route.request_fingerprint,
                            route.playback_id,
                            takeover.expected_owner_node_id,
                        ],
                    )? != 1)
            {
                tx.rollback()?;
                return Ok(None);
            }
            tx.execute(
                "DELETE FROM cache_consumer_pins
                  WHERE consumer_kind = 'media_session' AND consumer_id = ?1
                    AND consumer_epoch = ?2
                    AND EXISTS (SELECT 1 FROM media_sessions
                      WHERE incarnation_id = ?1 AND owner_node_id = ?3
                        AND owner_epoch = ?4 AND state = 'active')",
                params![
                    takeover.incarnation_id,
                    takeover.expected_owner_epoch,
                    takeover.next_owner_node_id,
                    next_epoch,
                ],
            )?;
            route.owner_node_id = takeover.next_owner_node_id;
            route.owner_epoch = next_epoch;
            route.lease_expires_at_ms = takeover.lease_expires_at_ms;
            route.discontinuity_sequence = route.discontinuity_sequence.saturating_add(1);
            route.updated_at_ms = takeover.now_ms;
            tx.commit()?;
            Ok(Some(route))
        })
        .await
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
        let end = end.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let mut route = tx
                .query_row(
                    &format!(
                        "SELECT {ROUTE_COLS} FROM media_sessions
                          WHERE incarnation_id = ?1 AND session_id = ?2
                            AND owner_node_id = ?3 AND owner_epoch = ?4"
                    ),
                    params![
                        end.incarnation_id,
                        end.session_id,
                        end.expected_owner_node_id,
                        end.expected_owner_epoch,
                    ],
                    route_from_row,
                )
                .optional()?;
            let Some(current_route) = route.as_mut() else {
                tx.commit()?;
                return Ok(None);
            };
            if current_route.state != "ended" {
                let changed = tx.execute(
                    "UPDATE media_sessions SET state = 'ended', terminal_reason = ?2, lease_expires_at_ms = ?1,
                            publication_ready_at_ms = ?7, updated_at_ms = ?1
                      WHERE incarnation_id = ?3 AND session_id = ?4
                        AND owner_node_id = ?5 AND owner_epoch = ?6
                        AND lease_expires_at_ms = ?8 AND state = 'active'",
                    params![
                        end.now_ms,
                        end.terminal_reason,
                        end.incarnation_id,
                        end.session_id,
                        end.expected_owner_node_id,
                        end.expected_owner_epoch,
                        MEDIA_SESSION_PUBLICATION_BLOCKED,
                        end.expected_lease_expires_at_ms,
                    ],
                )?;
                if changed != 1 {
                    tx.commit()?;
                    return Ok(None);
                }
                current_route.state = "ended".to_owned();
                current_route.terminal_reason = Some(end.terminal_reason.clone());
                current_route.lease_expires_at_ms = end.now_ms;
                current_route.publication_ready_at_ms = MEDIA_SESSION_PUBLICATION_BLOCKED;
                current_route.updated_at_ms = end.now_ms;
            }
            let lease_resource = format!("session:{}", end.incarnation_id);
            tx.execute(
                "UPDATE job_leases
                    SET expires_at_ms = CASE
                          WHEN expires_at_ms < ?1 THEN expires_at_ms ELSE ?1 END,
                        revision = revision + 1, updated_at_ms = ?1
                  WHERE resource = ?2 AND owner_node_id = ?3 AND fence = ?4
                    AND revision < 9223372036854775807",
                params![
                    end.now_ms,
                    lease_resource,
                    end.expected_owner_node_id,
                    end.expected_owner_epoch,
                ],
            )?;
            tx.execute(
                "DELETE FROM media_playback_pointers
                  WHERE user_id = ?1 AND playback_id = ?2 AND current_incarnation_id = ?3",
                params![
                    current_route.user_id,
                    current_route.playback_id,
                    end.incarnation_id
                ],
            )?;
            tx.execute(
                "DELETE FROM cache_consumer_pins
                  WHERE consumer_kind = 'media_session' AND consumer_id = ?1
                    AND consumer_epoch = ?2",
                params![end.incarnation_id, end.expected_owner_epoch],
            )?;
            tx.commit()?;
            Ok(route)
        })
        .await
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
        let session_id = session_id.to_owned();
        let terminal_reason = terminal_reason.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let mut route = tx
                .query_row(
                    &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE session_id = ?1"),
                    [session_id.as_str()],
                    route_from_row,
                )
                .optional()?;
            if let Some(route) = route.as_mut() {
                let lease_resource = format!("session:{}", route.incarnation_id);
                tx.execute(
                    "UPDATE media_sessions SET state = 'ended', terminal_reason = ?2,
                            lease_expires_at_ms = ?1, publication_ready_at_ms = ?4,
                            updated_at_ms = ?1
                      WHERE incarnation_id = ?3 AND state != 'ended'",
                    params![
                        now_ms,
                        terminal_reason,
                        route.incarnation_id,
                        MEDIA_SESSION_PUBLICATION_BLOCKED,
                    ],
                )?;
                if route.state != "ended" {
                    route.state = "ended".to_owned();
                    route.terminal_reason = Some(terminal_reason.clone());
                    route.lease_expires_at_ms = now_ms;
                    route.publication_ready_at_ms = MEDIA_SESSION_PUBLICATION_BLOCKED;
                    route.updated_at_ms = now_ms;
                }
                tx.execute(
                    "UPDATE job_leases
                        SET expires_at_ms = CASE
                              WHEN expires_at_ms < ?1 THEN expires_at_ms ELSE ?1 END,
                            revision = revision + 1, updated_at_ms = ?1
                      WHERE resource = ?2 AND owner_node_id = ?3 AND fence = ?4
                        AND revision < 9223372036854775807
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = ?5 AND state = 'ended')",
                    params![
                        now_ms,
                        lease_resource,
                        route.owner_node_id,
                        route.owner_epoch,
                        route.incarnation_id,
                    ],
                )?;
                tx.execute(
                    "DELETE FROM media_playback_pointers
                      WHERE user_id = ?1 AND playback_id = ?2 AND current_incarnation_id = ?3",
                    params![route.user_id, route.playback_id, route.incarnation_id],
                )?;
                tx.execute(
                    // Every epoch's pin, matching the replicated backend: the
                    // incarnation is over, so no generation of it may keep a
                    // shared-cache root alive. `consumer_id` is the
                    // incarnation, which §7.1 forbids reusing, so this can
                    // never reach a different live session's pin.
                    "DELETE FROM cache_consumer_pins
                      WHERE consumer_kind = 'media_session' AND consumer_id = ?1
                        AND EXISTS (SELECT 1 FROM media_sessions
                          WHERE incarnation_id = ?1 AND state = 'ended')",
                    params![route.incarnation_id],
                )?;
            }
            tx.commit()?;
            Ok(route)
        })
        .await
    }

    async fn maintain_media_sessions(&self, now_ms: i64) -> Result<(), StoreError> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let failed_cutoff = now_ms.saturating_sub(FAILED_RETENTION_MS);
            let retained_cutoff = now_ms.saturating_sub(RESOLVED_RETENTION_MS);
            let retire_before = now_ms.saturating_sub(TAKEOVER_RECOVERY_MS);
            // A preparation expires on **its own deadline**, and this is the
            // only thing that enforces it.
            //
            // A staged row sits at the publication sentinel — deliberately, so
            // takeover inventory never mistakes a successor nobody waited for
            // for a serving route. But `owned_media_sessions` and
            // `expired_media_sessions` both exclude that sentinel, so a staged
            // successor is invisible to the lease loop *and* to the retirement
            // sweep below. Nothing renews it and nothing reaps it: it would
            // simply wait for the generic retirement to notice its lease, which
            // is `now - TAKEOVER_RECOVERY_MS` on a five-minute tick — minutes
            // after the deadline the operator was promised, holding the
            // playback's one-preparation slot and one of the user's active rows
            // the whole time.
            //
            // Keyed on the ledger's `deadline_ms` rather than on the lease,
            // because that column *is* the contract: the owner wrote it, the
            // owner cannot renew past it, and a successor still wanted at that
            // moment has been committed already — commit deletes this row.
            tx.execute(
                "UPDATE media_sessions SET state = 'ended', terminal_reason = 'replaced', lease_expires_at_ms = ?1,
                        publication_ready_at_ms = ?3, updated_at_ms = ?1
                  WHERE incarnation_id IN (
                    SELECT staged.staged_incarnation_id FROM media_session_preparations staged
                     WHERE staged.deadline_ms <= ?1
                     ORDER BY staged.deadline_ms, staged.staged_incarnation_id LIMIT ?2)",
                params![now_ms, MAINTENANCE_BATCH, MEDIA_SESSION_PUBLICATION_BLOCKED],
            )?;
            tx.execute(
                "UPDATE media_sessions SET state = 'ended', terminal_reason = 'replaced', lease_expires_at_ms = ?1,
                        publication_ready_at_ms = ?4, updated_at_ms = ?1
                  WHERE incarnation_id IN (
                    SELECT incarnation_id FROM media_sessions
                     WHERE state = 'active' AND lease_expires_at_ms <= ?3
                     ORDER BY lease_expires_at_ms, incarnation_id LIMIT ?2)",
                params![
                    now_ms,
                    MAINTENANCE_BATCH,
                    retire_before,
                    MEDIA_SESSION_PUBLICATION_BLOCKED,
                ],
            )?;
            tx.execute(
                "DELETE FROM cache_consumer_pins
                  WHERE consumer_kind = 'media_session'
                    AND consumer_id IN (
                      SELECT incarnation_id FROM media_sessions
                       WHERE state = 'ended' AND lease_expires_at_ms <= ?1
                       ORDER BY updated_at_ms, incarnation_id LIMIT ?2)",
                params![now_ms, MAINTENANCE_BATCH],
            )?;
            tx.execute(
                "UPDATE job_leases
                    SET expires_at_ms = CASE
                          WHEN expires_at_ms < ?1 THEN expires_at_ms ELSE ?1 END,
                        revision = revision + 1, updated_at_ms = ?1
                  WHERE revision < 9223372036854775807 AND expires_at_ms > ?1
                    AND resource IN (
                      SELECT 'session:' || session.incarnation_id
                        FROM media_sessions session
                        JOIN job_leases lease
                          ON lease.resource = 'session:' || session.incarnation_id
                       WHERE session.state = 'ended'
                         AND session.lease_expires_at_ms <= ?1
                         AND lease.expires_at_ms > ?1
                       ORDER BY session.updated_at_ms, session.incarnation_id LIMIT ?2)",
                params![now_ms, MAINTENANCE_BATCH],
            )?;
            tx.execute(
                "DELETE FROM media_playback_pointers WHERE rowid IN (
                   SELECT pointer.rowid FROM media_playback_pointers pointer
                   LEFT JOIN media_sessions session
                     ON session.incarnation_id = pointer.current_incarnation_id
                  WHERE session.incarnation_id IS NULL OR session.state != 'active'
                     OR session.lease_expires_at_ms <= ?3
                  ORDER BY pointer.updated_at_ms, pointer.rowid LIMIT ?2)",
                params![now_ms, MAINTENANCE_BATCH, retire_before],
            )?;
            // The ledger's own reaper, and the reason the preparation deadline
            // can be a single clock. The retirement above already ended any
            // staged row whose deadline passed — a staged row is `active`, so
            // it is in that sweep like any other — and this clears the ledger
            // entry it left behind. Without it a dead owner's preparation
            // would hold the one-per-playback slot forever and refuse every
            // future prepare for that player.
            //
            // Keyed on the successor's state, never on the deadline, so it can
            // never race a live preparation whose owner is still renewing.
            tx.execute(
                "DELETE FROM media_session_preparations WHERE rowid IN (
                   SELECT preparation.rowid FROM media_session_preparations preparation
                   LEFT JOIN media_sessions session
                     ON session.incarnation_id = preparation.staged_incarnation_id
                  WHERE session.incarnation_id IS NULL OR session.state != 'active'
                  ORDER BY preparation.updated_at_ms, preparation.rowid LIMIT ?1)",
                params![MAINTENANCE_BATCH],
            )?;
            tx.execute(
                "DELETE FROM media_session_requests WHERE rowid IN (
                   SELECT rowid FROM media_session_requests
                    WHERE state = 'starting' AND claim_expires_at_ms <= ?1
                    ORDER BY claim_expires_at_ms, rowid LIMIT ?2)",
                params![now_ms, MAINTENANCE_BATCH],
            )?;
            tx.execute(
                "DELETE FROM media_session_requests WHERE rowid IN (
                   SELECT rowid FROM media_session_requests
                    WHERE state = 'failed' AND updated_at_ms < ?1
                    ORDER BY updated_at_ms, rowid LIMIT ?2)",
                params![failed_cutoff, MAINTENANCE_BATCH],
            )?;
            tx.execute(
                "DELETE FROM media_session_requests WHERE rowid IN (
                   SELECT request.rowid FROM media_session_requests request
                    WHERE request.state = 'resolved' AND request.updated_at_ms < ?1
                      AND NOT EXISTS (SELECT 1 FROM media_sessions session
                        WHERE session.incarnation_id = request.incarnation_id
                          AND session.state = 'active' AND session.lease_expires_at_ms > ?2)
                    ORDER BY request.updated_at_ms, request.rowid LIMIT ?3)",
                params![retained_cutoff, now_ms, MAINTENANCE_BATCH],
            )?;
            tx.execute(
                "DELETE FROM job_leases WHERE rowid IN (
                   SELECT lease.rowid FROM job_leases lease
                   JOIN media_sessions session
                     ON lease.resource = 'session:' || session.incarnation_id
                    WHERE session.state = 'ended' AND session.updated_at_ms < ?1
                    ORDER BY session.updated_at_ms, lease.rowid LIMIT ?2)",
                params![retained_cutoff, MAINTENANCE_BATCH],
            )?;
            tx.execute(
                "DELETE FROM media_session_terminal_acks WHERE rowid IN (
                   SELECT acknowledgement.rowid FROM media_session_terminal_acks acknowledgement
                    WHERE acknowledgement.expires_at_ms <= ?1
                       OR NOT EXISTS (SELECT 1 FROM media_sessions session
                            WHERE session.session_id = acknowledgement.session_id
                              AND session.incarnation_id = acknowledgement.incarnation_id)
                    ORDER BY acknowledgement.expires_at_ms, acknowledgement.rowid LIMIT ?2)",
                params![now_ms, MAINTENANCE_BATCH],
            )?;
            tx.execute(
                "DELETE FROM media_sessions WHERE rowid IN (
                   SELECT rowid FROM media_sessions
                    WHERE state = 'ended' AND updated_at_ms < ?1
                    ORDER BY updated_at_ms, rowid LIMIT ?2)",
                params![retained_cutoff, MAINTENANCE_BATCH],
            )?;
            tx.execute(
                "DELETE FROM job_leases WHERE rowid IN (
                   SELECT lease.rowid FROM job_leases lease
                    WHERE lease.resource LIKE 'session:%' AND lease.updated_at_ms < ?1
                      AND NOT EXISTS (SELECT 1 FROM media_sessions session
                        WHERE lease.resource = 'session:' || session.incarnation_id)
                    ORDER BY lease.updated_at_ms, lease.rowid LIMIT ?2)",
                params![retained_cutoff, MAINTENANCE_BATCH],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    async fn owned_media_sessions(
        &self,
        owner_node_id: &str,
        now_ms: i64,
    ) -> Result<Vec<OwnedMediaSessionLease>, StoreError> {
        if owner_node_id.is_empty() || owner_node_id.len() > 256 {
            return Err(StoreError::Task("invalid media-session owner".to_owned()));
        }
        let owner_node_id = owner_node_id.to_owned();
        self.with_read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT incarnation_id, session_id, owner_epoch, lease_expires_at_ms
                   FROM media_sessions
                  WHERE owner_node_id = ?1 AND state = 'active'
                    AND lease_expires_at_ms > ?2
                    AND publication_ready_at_ms != ?4
                    AND NOT EXISTS (SELECT 1 FROM media_session_requests request
                      WHERE request.user_id = media_sessions.user_id
                        AND request.incarnation_id = media_sessions.incarnation_id
                        AND request.state = 'starting'
                        AND media_sessions.publication_ready_at_ms = 0
                        AND request.claim_expires_at_ms <= media_sessions.lease_expires_at_ms)
                  ORDER BY updated_at_ms, incarnation_id LIMIT ?3",
            )?;
            let leases = statement
                .query_map(
                    params![
                        owner_node_id,
                        now_ms,
                        MAX_OWNED,
                        MEDIA_SESSION_PUBLICATION_BLOCKED,
                    ],
                    |row| {
                        Ok(OwnedMediaSessionLease {
                            incarnation_id: row.get(0)?,
                            session_id: row.get(1)?,
                            owner_epoch: row.get(2)?,
                            lease_expires_at_ms: row.get(3)?,
                        })
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(leases)
        })
        .await
    }
}
