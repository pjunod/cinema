use async_trait::async_trait;
use rusqlite::{params, OptionalExtension, Row};

use super::SqliteStore;
use crate::domain::{
    MediaSessionActivation, MediaSessionActivationOutcome, MediaSessionRenewal,
    MediaSessionRequestClaim, MediaSessionRoute, OwnedMediaSessionLease,
};
use crate::error::StoreError;
use crate::store::MediaSessionStore;

const MAX_IN_FLIGHT_PER_USER: i64 = 32;
const MAX_CURRENT_PER_USER: i64 = 64;
const MAX_REQUEST_ROWS_PER_USER: i64 = 4_096;
const MAX_SESSION_ROWS_PER_USER: i64 = 4_096;
const MAX_RENEWALS: usize = 256;
const MAX_OWNED: i64 = 4_096;
const MAINTENANCE_BATCH: i64 = 256;
const FAILED_RETENTION_MS: i64 = 60 * 60 * 1_000;
const RESOLVED_RETENTION_MS: i64 = 24 * 60 * 60 * 1_000;
const ROUTE_COLS: &str = "incarnation_id, session_id, user_id, playback_id, \
    request_fingerprint, owner_node_id, owner_epoch, lease_expires_at_ms, state, \
    recipe_json, response_json, produced_playable_through_ms, fetched_through_ms, \
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
        recipe_json: row.get(9)?,
        response_json: row.get(10)?,
        produced_playable_through_ms: row.get(11)?,
        fetched_through_ms: row.get(12)?,
        media_origin_ms: row.get(13)?,
        media_sequence: row.get(14)?,
        discontinuity_sequence: row.get(15)?,
        updated_at_ms: row.get(16)?,
    })
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
    incarnation_id: &str,
    now_ms: i64,
    expires_at_ms: i64,
) -> Result<(), StoreError> {
    if user_id <= 0
        || request_id.is_empty()
        || request_id.len() > 128
        || !valid_fingerprint(fingerprint)
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
        && activation
            .request_id
            .as_ref()
            .is_none_or(|value| !value.is_empty() && value.len() <= 128)
        && valid_fingerprint(&activation.request_fingerprint)
        && !activation.owner_node_id.is_empty()
        && activation.owner_node_id.len() <= 256
        && activation.recipe_json.len() <= 32 * 1024
        && activation.response_json.len() <= 64 * 1024
        && activation.lease_expires_at_ms > activation.now_ms;
    if valid {
        Ok(())
    } else {
        Err(StoreError::Task(
            "invalid media-session activation".to_owned(),
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
        incarnation_id: &str,
        now_ms: i64,
        claim_expires_at_ms: i64,
    ) -> Result<MediaSessionRequestClaim, StoreError> {
        validate_claim(
            user_id,
            request_id,
            request_fingerprint,
            incarnation_id,
            now_ms,
            claim_expires_at_ms,
        )?;
        self.maintain_media_sessions(now_ms).await?;
        let request_id = request_id.to_owned();
        let request_fingerprint = request_fingerprint.to_ascii_lowercase();
        let incarnation_id = incarnation_id.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let existing = tx
                .query_row(
                    "SELECT request_fingerprint, state, incarnation_id, owner_node_id,
                            claim_expires_at_ms
                       FROM media_session_requests WHERE user_id = ?1 AND request_id = ?2",
                    params![user_id, request_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((fingerprint, state, existing_incarnation, owner, expires)) = existing {
                let outcome = if fingerprint != request_fingerprint {
                    MediaSessionRequestClaim::Conflict
                } else if state == "failed" {
                    let reacquired = tx.execute(
                        "UPDATE media_session_requests
                            SET state = 'starting', claim_expires_at_ms = ?1,
                                incarnation_id = ?2, owner_node_id = NULL,
                                response_json = NULL, updated_at_ms = ?3
                          WHERE user_id = ?4 AND request_id = ?5 AND state = 'failed'
                            AND request_fingerprint = ?6
                            AND (SELECT COUNT(*) FROM media_session_requests
                                  WHERE user_id = ?4 AND state = 'starting'
                                    AND claim_expires_at_ms > ?3) < ?7
                            AND (SELECT COUNT(*) FROM media_sessions
                                  WHERE user_id = ?4 AND state IN ('starting', 'active')
                                    AND lease_expires_at_ms > ?3) < ?8",
                        params![
                            claim_expires_at_ms,
                            incarnation_id,
                            now_ms,
                            user_id,
                            request_id,
                            request_fingerprint,
                            MAX_IN_FLIGHT_PER_USER,
                            MAX_CURRENT_PER_USER,
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
                    .map(MediaSessionRequestClaim::Resolved)
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
                    AND lease_expires_at_ms > ?2",
                params![user_id, now_ms],
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
                    (user_id, request_id, request_fingerprint, state, claim_expires_at_ms,
                     incarnation_id, owner_node_id, response_json, updated_at_ms)
                 VALUES (?1, ?2, ?3, 'starting', ?4, ?5, NULL, NULL, ?6)",
                params![
                    user_id,
                    request_id,
                    request_fingerprint,
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
            if let Some(request_id) = activation.request_id.as_deref() {
                let matches: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM media_session_requests
                      WHERE user_id = ?1 AND request_id = ?2 AND incarnation_id = ?3
                        AND request_fingerprint = ?4 AND owner_node_id = ?5
                        AND ((state = 'starting' AND claim_expires_at_ms > ?6)
                          OR (state = 'resolved' AND response_json = ?7))",
                    params![
                        activation.user_id,
                        request_id,
                        activation.incarnation_id,
                        activation.request_fingerprint,
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
            if let Some(expected) = activation.expected_predecessor_incarnation_id.as_deref() {
                let current = tx
                    .query_row(
                        "SELECT current_incarnation_id FROM media_playback_pointers
                          WHERE user_id = ?1 AND playback_id = ?2",
                        params![activation.user_id, activation.playback_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                if current.as_deref() != Some(expected) {
                    tx.commit()?;
                    return Ok(None);
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
                "UPDATE media_sessions SET state = 'ended', lease_expires_at_ms = ?1,
                        updated_at_ms = ?1
                  WHERE incarnation_id = (SELECT current_incarnation_id
                    FROM media_playback_pointers WHERE user_id = ?2 AND playback_id = ?3)
                    AND incarnation_id != ?4 AND state != 'ended'",
                params![
                    activation.now_ms,
                    activation.user_id,
                    activation.playback_id,
                    activation.incarnation_id,
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
                     media_origin_ms, media_sequence, discontinuity_sequence, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, 'active', ?8, ?9,
                         0, 0, 0, 0, 0, ?10)
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
                     WHERE resource = ?11 AND owner_node_id = ?6 AND fence = 1
                       AND expires_at_ms = ?7 AND expires_at_ms > ?10)",
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
                    activation.now_ms,
                    lease_resource,
                ],
            )?;
            let route = tx
                .query_row(
                    &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE incarnation_id = ?1"),
                    [activation.incarnation_id.as_str()],
                    route_from_row,
                )
                .optional()?;
            let Some(route) = route.filter(|route| {
                route.session_id == activation.session_id
                    && route.owner_node_id == activation.owner_node_id
                    && route.request_fingerprint == activation.request_fingerprint
                    && route.state == "active"
            }) else {
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
            if let Some(request_id) = activation.request_id.as_deref() {
                tx.execute(
                    "UPDATE media_session_requests SET state = 'resolved', response_json = ?1,
                            claim_expires_at_ms = ?2, updated_at_ms = ?3
                      WHERE user_id = ?4 AND request_id = ?5 AND incarnation_id = ?6
                        AND request_fingerprint = ?7 AND owner_node_id = ?8",
                    params![
                        activation.response_json,
                        activation.lease_expires_at_ms,
                        activation.now_ms,
                        activation.user_id,
                        request_id,
                        activation.incarnation_id,
                        activation.request_fingerprint,
                        activation.owner_node_id,
                    ],
                )?;
            }
            tx.commit()?;
            Ok(Some(MediaSessionActivationOutcome { route, predecessor }))
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
        self.with_conn(move |conn| {
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
        self.with_conn(move |conn| {
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
            || renewals
                .iter()
                .any(|renewal| !valid_uuid(&renewal.incarnation_id) || renewal.owner_epoch <= 0)
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
                            AND state = 'active' AND lease_expires_at_ms > ?2)",
                    params![
                        lease_expires_at_ms,
                        now_ms,
                        lease_resource,
                        owner_node_id,
                        renewal.owner_epoch,
                        renewal.incarnation_id,
                    ],
                )? == 1;
                if lease_renewed
                    && tx.execute(
                        "UPDATE media_sessions SET lease_expires_at_ms = ?1, updated_at_ms = ?2
                      WHERE incarnation_id = ?3 AND owner_node_id = ?4 AND owner_epoch = ?5
                        AND state = 'active' AND lease_expires_at_ms > ?2
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
                        ],
                    )? == 1
                {
                    renewed.push(renewal.incarnation_id);
                }
            }
            tx.commit()?;
            Ok(renewed)
        })
        .await
    }

    async fn end_media_session(
        &self,
        session_id: &str,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        if !valid_uuid(session_id) {
            return Ok(None);
        }
        let session_id = session_id.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let route = tx
                .query_row(
                    &format!("SELECT {ROUTE_COLS} FROM media_sessions WHERE session_id = ?1"),
                    [session_id.as_str()],
                    route_from_row,
                )
                .optional()?;
            if let Some(route) = route.as_ref() {
                let lease_resource = format!("session:{}", route.incarnation_id);
                tx.execute(
                    "UPDATE media_sessions SET state = 'ended', lease_expires_at_ms = ?1,
                            updated_at_ms = ?1 WHERE incarnation_id = ?2 AND state != 'ended'",
                    params![now_ms, route.incarnation_id],
                )?;
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
            tx.execute(
                "UPDATE media_sessions SET state = 'ended', lease_expires_at_ms = ?1,
                        updated_at_ms = ?1
                  WHERE incarnation_id IN (
                    SELECT incarnation_id FROM media_sessions
                     WHERE state = 'active' AND lease_expires_at_ms <= ?1
                     ORDER BY lease_expires_at_ms, incarnation_id LIMIT ?2)",
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
                     OR session.lease_expires_at_ms <= ?1
                  ORDER BY pointer.updated_at_ms, pointer.rowid LIMIT ?2)",
                params![now_ms, MAINTENANCE_BATCH],
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
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(
                "SELECT incarnation_id, session_id, owner_epoch, lease_expires_at_ms
                   FROM media_sessions
                  WHERE owner_node_id = ?1 AND state = 'active'
                    AND lease_expires_at_ms > ?2
                  ORDER BY updated_at_ms, incarnation_id LIMIT ?3",
            )?;
            Ok(statement
                .query_map(params![owner_node_id, now_ms, MAX_OWNED], |row| {
                    Ok(OwnedMediaSessionLease {
                        incarnation_id: row.get(0)?,
                        session_id: row.get(1)?,
                        owner_epoch: row.get(2)?,
                        lease_expires_at_ms: row.get(3)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }
}
