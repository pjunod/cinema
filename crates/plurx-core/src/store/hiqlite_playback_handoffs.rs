//! Replicated playback handoffs: the same statements as the standalone store,
//! with redemption as one Raft transaction. See `playback_handoffs.rs`.
use async_trait::async_trait;
use hiqlite::macros::params;

use super::hiqlite::{validate_sql, HiqliteAuthStore};
use super::playback_handoffs as ph;
use super::{
    bounded_device_label, NewPlaybackHandoff, PlaybackHandoff, PlaybackHandoffRedeemOutcome,
    PlaybackHandoffRedemption, PlaybackHandoffStore, PlaybackHandoffTokenScope,
};
use crate::domain::User;
use crate::error::StoreError;

struct HandoffRow(PlaybackHandoff, i64);

impl From<&mut hiqlite::Row<'_>> for HandoffRow {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self(
            PlaybackHandoff {
                id: row.get("id"),
                user_id: row.get("user_id"),
                file_id: row.get("file_id"),
                start_ms: row.get("start_ms"),
                audio: row.get("audio"),
                subtitle: row.get("subtitle"),
                origin: row.get("origin"),
                created_at: row.get("created_at"),
                expires_at: row.get("expires_at"),
                redeemed_at: row.get("redeemed_at"),
            },
            row.get("redeemed_here"),
        )
    }
}

struct HandoffUserRow(User);

impl From<&mut hiqlite::Row<'_>> for HandoffUserRow {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self(User {
            id: row.get("id"),
            username: row.get("username"),
            password_hash: row.get("password_hash"),
            is_admin: row.get::<i64>("is_admin") != 0,
            created_at: row.get("created_at"),
        })
    }
}

struct ScopeRow(PlaybackHandoffTokenScope);

impl From<&mut hiqlite::Row<'_>> for ScopeRow {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self(PlaybackHandoffTokenScope {
            handoff_id: row.get("handoff_id"),
            user_id: row.get("user_id"),
            file_id: row.get("file_id"),
            expires_at: row.get("expires_at"),
        })
    }
}

#[async_trait]
impl PlaybackHandoffStore for HiqliteAuthStore {
    async fn create_playback_handoff(
        &self,
        handoff: NewPlaybackHandoff,
    ) -> Result<bool, StoreError> {
        Ok(self
            .execute(
                ph::INSERT_SQL,
                params!(
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
                ),
            )
            .await?
            > 0)
    }

    async fn playback_handoff_for_minter(
        &self,
        id: &str,
        user_id: i64,
        source_token_hash: &str,
    ) -> Result<Option<PlaybackHandoff>, StoreError> {
        validate_sql(ph::STATUS_SQL)?;
        Ok(self
            .client()
            // authority: the web app polls this to tell a redeemed code from an unopened player; a stale replica would report a redemption late.
            .query_consistent_map::<HandoffRow, _>(
                ph::STATUS_SQL,
                params!(id, user_id, source_token_hash),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0))
    }

    async fn redeem_playback_handoff(
        &self,
        redemption: &PlaybackHandoffRedemption,
    ) -> Result<PlaybackHandoffRedeemOutcome, StoreError> {
        let r = redemption;
        let device = bounded_device_label(r.device.clone());
        // One Raft transaction: the claim and the token it mints.
        self.credential_mutation(vec![
            (ph::CLAIM_SQL, params!(r.now, r.code_hash.clone())),
            (
                ph::MINT_TOKEN_SQL,
                params!(
                    r.token_hash.clone(),
                    device,
                    r.now,
                    r.token_expires_at,
                    r.code_hash.clone()
                ),
            ),
        ])
        .await?;
        validate_sql(ph::OUTCOME_SQL)?;
        let row = self
            .client()
            // authority: the redemption verdict must observe the transaction just committed, never a replica that predates it.
            .query_consistent_map::<HandoffRow, _>(
                ph::OUTCOME_SQL,
                params!(r.token_hash.clone(), r.code_hash.clone()),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| (row.0, row.1 != 0));
        Ok(ph::redeem_outcome(row))
    }

    async fn playback_handoff_token_scope(
        &self,
        token_hash: &str,
        now: i64,
    ) -> Result<Option<PlaybackHandoffTokenScope>, StoreError> {
        validate_sql(ph::TOKEN_SCOPE_SQL)?;
        Ok(self
            .client()
            // authority: a handoff token is revoked with its minting token; a stale replica would keep serving it.
            .query_consistent_map::<ScopeRow, _>(ph::TOKEN_SCOPE_SQL, params!(token_hash, now))
            .await?
            .into_iter()
            .next()
            .map(|row| row.0))
    }

    async fn advance_playback_handoff(
        &self,
        token_hash: &str,
        expected_file_id: i64,
        next_file_id: i64,
        now: i64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .credential_mutation(vec![(
                ph::ADVANCE_SQL,
                params!(next_file_id, token_hash, expected_file_id, now),
            )])
            .await?[0]
            > 0)
    }

    async fn revoke_playback_handoff_token(&self, token_hash: &str) -> Result<bool, StoreError> {
        Ok(self
            .credential_mutation(vec![(ph::REVOKE_SQL, params!(token_hash))])
            .await?[0]
            > 0)
    }

    async fn revoke_playback_handoff_token_by_prefix(
        &self,
        user_id: i64,
        prefix: &str,
    ) -> Result<bool, StoreError> {
        Ok(self
            .credential_mutation(vec![(ph::REVOKE_PREFIX_SQL, params!(user_id, prefix))])
            .await?[0]
            > 0)
    }

    async fn prune_playback_handoffs(
        &self,
        now: i64,
        codes_before: i64,
    ) -> Result<u64, StoreError> {
        let changed = self
            .credential_mutation(vec![
                (ph::PRUNE_TOKENS_SQL, params!(now)),
                (ph::PRUNE_CODES_SQL, params!(codes_before)),
            ])
            .await?;
        Ok(changed.into_iter().map(|count| count as u64).sum())
    }
}

impl HiqliteAuthStore {
    /// `TokenAudience::PlaybackHandoff`: the user of a live handoff token.
    /// Read from its own table, never from `tokens`.
    pub(super) async fn authenticate_playback_handoff(
        &self,
        token_hash: &str,
    ) -> Result<Option<User>, StoreError> {
        validate_sql(ph::AUTHENTICATE_SQL)?;
        let now = self.now()?;
        Ok(self
            .client()
            // authority: a handoff token is revoked with its minting token; a stale replica would keep accepting it.
            .query_consistent_map::<HandoffUserRow, _>(
                ph::AUTHENTICATE_SQL,
                params!(token_hash, now),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0))
    }
}
