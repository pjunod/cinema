//! Standalone playback handoffs share the native token table; see
//! `store/playback_handoffs.rs` for the audience rules.
use async_trait::async_trait;
use rusqlite::{params, OptionalExtension};

use super::SqliteStore;
use crate::error::StoreError;
use crate::store::playback_handoffs as ph;
use crate::store::{
    bounded_device_label, NewPlaybackHandoff, PlaybackHandoff, PlaybackHandoffRedeemOutcome,
    PlaybackHandoffRedemption, PlaybackHandoffStore, PlaybackHandoffTokenScope,
};

fn handoff_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(PlaybackHandoff, bool)> {
    Ok((
        PlaybackHandoff {
            id: row.get(0)?,
            user_id: row.get(1)?,
            file_id: row.get(2)?,
            start_ms: row.get(3)?,
            audio: row.get(4)?,
            subtitle: row.get(5)?,
            origin: row.get(6)?,
            created_at: row.get(7)?,
            expires_at: row.get(8)?,
            redeemed_at: row.get(9)?,
        },
        row.get::<_, i64>(10)? != 0,
    ))
}

#[async_trait]
impl PlaybackHandoffStore for SqliteStore {
    async fn create_playback_handoff(
        &self,
        handoff: NewPlaybackHandoff,
    ) -> Result<bool, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn.execute(
                ph::INSERT_SQL,
                params![
                    handoff.id,
                    handoff.code_hash,
                    handoff.user_id,
                    handoff.file_id,
                    handoff.source_token_hash,
                    handoff.start_ms,
                    handoff.audio,
                    handoff.subtitle,
                    handoff.origin,
                    handoff.created_at,
                    handoff.expires_at
                ],
            )? > 0)
        })
        .await
    }

    async fn playback_handoff_for_minter(
        &self,
        id: &str,
        user_id: i64,
        source_token_hash: &str,
    ) -> Result<Option<PlaybackHandoff>, StoreError> {
        let id = id.to_owned();
        let source_token_hash = source_token_hash.to_owned();
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(
                    ph::STATUS_SQL,
                    params![id, user_id, source_token_hash],
                    handoff_row,
                )
                .optional()?
                .map(|(handoff, _)| handoff))
        })
        .await
    }

    async fn redeem_playback_handoff(
        &self,
        redemption: &PlaybackHandoffRedemption,
    ) -> Result<PlaybackHandoffRedeemOutcome, StoreError> {
        let r = redemption.clone();
        let device = bounded_device_label(r.device.clone());
        self.with_conn(move |conn| {
            // One fixed batch: each later statement is conditional on the
            // claim this batch made, and the guard rolls everything back if a
            // token row would be left without its audience marker.
            let tx = conn.unchecked_transaction()?;
            tx.execute(ph::CLAIM_SQL, params![r.now, r.code_hash])?;
            tx.execute(
                ph::MINT_TOKEN_SQL,
                params![r.token_hash, device, r.now, r.code_hash],
            )?;
            tx.execute(
                ph::MARK_TOKEN_SQL,
                params![r.token_hash, r.token_expires_at, r.code_hash, r.now],
            )?;
            tx.execute(ph::MARKER_GUARD_SQL, params![r.token_hash])?;
            tx.commit()?;
            let row = conn
                .query_row(
                    ph::OUTCOME_SQL,
                    params![r.token_hash, r.code_hash],
                    handoff_row,
                )
                .optional()?;
            Ok(ph::redeem_outcome(row))
        })
        .await
    }

    async fn playback_handoff_token_scope(
        &self,
        token_hash: &str,
        now: i64,
    ) -> Result<Option<PlaybackHandoffTokenScope>, StoreError> {
        let token_hash = token_hash.to_owned();
        self.with_read(move |conn| {
            Ok(conn
                .query_row(ph::TOKEN_SCOPE_SQL, params![token_hash, now], |row| {
                    Ok(PlaybackHandoffTokenScope {
                        handoff_id: row.get(0)?,
                        user_id: row.get(1)?,
                        file_id: row.get(2)?,
                        expires_at: row.get(3)?,
                    })
                })
                .optional()?)
        })
        .await
    }

    async fn prune_playback_handoffs(
        &self,
        now: i64,
        codes_before: i64,
    ) -> Result<u64, StoreError> {
        self.with_conn(move |conn| {
            // Each statement is safe on its own: a token row goes before its
            // marker, and a marker goes only once its token row is gone.
            let tokens = conn.execute(ph::PRUNE_TOKENS_SQL, params![now])?;
            let markers = conn.execute(ph::PRUNE_MARKERS_SQL, [])?;
            let codes = conn.execute(ph::PRUNE_CODES_SQL, params![codes_before])?;
            Ok((tokens + markers + codes) as u64)
        })
        .await
    }
}

/// Test fixture: move a handoff's clocks so code and token expiry can be
/// exercised without waiting them out. Compiled only for tests and the
/// `fixtures` feature the plurxd test suite enables.
#[cfg(any(test, feature = "fixtures"))]
impl SqliteStore {
    pub async fn fixture_expire_playback_handoff(
        &self,
        handoff_id: &str,
        expires_at: i64,
    ) -> Result<u64, StoreError> {
        let handoff_id = handoff_id.to_owned();
        self.with_conn(move |conn| {
            let codes = conn.execute(
                "UPDATE playback_handoffs SET expires_at = ?2 WHERE id = ?1",
                params![handoff_id, expires_at],
            )?;
            let tokens = conn.execute(
                "UPDATE playback_handoffs_tokens SET expires_at = ?2 WHERE handoff_id = ?1",
                params![handoff_id, expires_at],
            )?;
            Ok((codes + tokens) as u64)
        })
        .await
    }
}
