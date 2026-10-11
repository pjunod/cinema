use super::hiqlite::{database_error, HiqliteAuthStore};
use super::jellyfin_play as jp;
use super::{
    JellyfinPlay, JellyfinPlayActivation, JellyfinPlayScope, JellyfinPlayStore, NewJellyfinPlay,
};
use crate::error::StoreError;
use async_trait::async_trait;
use hiqlite::{macros::params, Row};
impl From<&mut Row<'_>> for jp::RawPlay {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            payload: row.get("payload"),
            state: row.get("state"),
            expires_at_ms: row.get("expires_at_ms"),
            manual_revision: row.get("manual_revision"),
            native_incarnation_id: row.get("native_incarnation_id"),
            direct_grant_id: row.get("direct_grant_id"),
        }
    }
}
#[async_trait]
impl JellyfinPlayStore for HiqliteAuthStore {
    async fn end_jellyfin_login_plays(
        &self,
        scope: &JellyfinPlayScope,
        now_ms: i64,
    ) -> Result<Vec<JellyfinPlay>, StoreError> {
        jp::validate_scope(scope)?;
        let expiry = jp::terminal_expiry(now_ms)?;
        self.client()
            .execute_returning_map::<_, jp::RawPlay>(
                jp::END_LOGIN,
                params!(
                    expiry,
                    scope.user_id,
                    &scope.token_digest,
                    &scope.device_digest,
                    scope.client_family.as_str()
                ),
            )
            .await?
            .into_iter()
            .map(|row| row.map_err(database_error).and_then(jp::RawPlay::decode))
            .collect()
    }

    async fn create_jellyfin_play(&self, play: NewJellyfinPlay) -> Result<bool, StoreError> {
        let (payload, expiry) = jp::encode(&play)?;
        let results = self
            .client()
            .txn(vec![
                (jp::CLEANUP, params!(play.created_at_ms)),
                (jp::RETIRE_ORPHANED_ACTIVE, params!(play.created_at_ms)),
                (
                    jp::TRIM_LOGIN_TOMBSTONES,
                    params!(
                        play.scope.user_id,
                        play.scope.token_digest.clone(),
                        play.scope.device_digest.clone(),
                        play.scope.client_family.as_str()
                    ),
                ),
                (
                    jp::CREATE,
                    params!(
                        play.play_id,
                        play.scope.user_id,
                        play.scope.token_digest,
                        play.scope.device_digest,
                        play.scope.client_family.as_str(),
                        play.playback_id,
                        play.item_id,
                        play.file_id,
                        payload,
                        expiry,
                        play.item_wire_id,
                        play.file_wire_id,
                        play.media_grant_id
                    ),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(results[3] == 1)
    }
    async fn jellyfin_play(
        &self,
        play_id: &str,
        scope: &JellyfinPlayScope,
    ) -> Result<Option<JellyfinPlay>, StoreError> {
        jp::validate_key(play_id, scope)?;
        self.client()
            // authority: activation and late stop require the current exact play tombstone.
            .query_consistent_map::<jp::RawPlay, _>(
                jp::READ,
                params!(
                    play_id,
                    scope.user_id,
                    &scope.token_digest,
                    &scope.device_digest,
                    scope.client_family.as_str()
                ),
            )
            .await?
            .into_iter()
            .next()
            .map(jp::RawPlay::decode)
            .transpose()
    }
    async fn jellyfin_play_for_direct_grant(
        &self,
        grant_id: &str,
    ) -> Result<Option<JellyfinPlay>, StoreError> {
        if !jp::reference(grant_id) {
            return Ok(None);
        }
        self.client()
            // authority: an anonymous scoped media link must observe Stop, supersession and login replacement exactly.
            .query_consistent_map::<jp::RawPlay, _>(jp::READ_BY_DIRECT_GRANT, params!(grant_id))
            .await?
            .into_iter()
            .next()
            .map(jp::RawPlay::decode)
            .transpose()
    }
    async fn jellyfin_current_direct_play(
        &self,
        scope: &JellyfinPlayScope,
        item_wire_id: &str,
        file_wire_id: &str,
    ) -> Result<Option<JellyfinPlay>, StoreError> {
        jp::validate_source(scope, item_wire_id, file_wire_id)?;
        self.client()
            // authority: a direct request without a play id must resolve the play Stop, supersession and renegotiation last committed.
            .query_consistent_map::<jp::RawPlay, _>(
                jp::READ_CURRENT_DIRECT,
                params!(
                    scope.user_id,
                    &scope.token_digest,
                    &scope.device_digest,
                    scope.client_family.as_str(),
                    item_wire_id,
                    file_wire_id
                ),
            )
            .await?
            .into_iter()
            .next()
            .map(jp::RawPlay::decode)
            .transpose()
    }
    async fn jellyfin_plays_superseded_by(
        &self,
        play_id: &str,
        scope: &JellyfinPlayScope,
    ) -> Result<Vec<JellyfinPlay>, StoreError> {
        jp::validate_key(play_id, scope)?;
        self.client()
            // authority: releasing a predecessor's exact resources must see the supersession its own activation committed.
            .query_consistent_map::<jp::RawPlay, _>(
                jp::READ_SUPERSEDED_BY,
                params!(
                    scope.user_id,
                    play_id,
                    &scope.token_digest,
                    &scope.device_digest,
                    scope.client_family.as_str()
                ),
            )
            .await?
            .into_iter()
            .map(jp::RawPlay::decode)
            .collect()
    }
    async fn activate_jellyfin_play(
        &self,
        play_id: &str,
        scope: &JellyfinPlayScope,
        activation: JellyfinPlayActivation,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        jp::validate_key(play_id, scope)?;
        let expiry = jp::terminal_expiry(now_ms)?;
        let (sql, reference) = jp::activation_sql(activation)?;
        let nonce = uuid::Uuid::new_v4().to_string();
        let changed = self
            .client()
            .txn(vec![
                (
                    sql,
                    params!(
                        reference,
                        i64::MAX,
                        play_id,
                        scope.user_id,
                        &scope.token_digest,
                        &scope.device_digest,
                        scope.client_family.as_str(),
                        now_ms,
                        nonce.as_str()
                    ),
                ),
                (
                    jp::SUPERSEDE_AFTER_BINDING_ACTIVATION,
                    params!(
                        play_id,
                        scope.user_id,
                        &scope.token_digest,
                        &scope.device_digest,
                        scope.client_family.as_str(),
                        expiry,
                        nonce.as_str()
                    ),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(changed.first().copied() == Some(1))
    }
    async fn end_jellyfin_play(
        &self,
        play_id: &str,
        scope: &JellyfinPlayScope,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        jp::validate_key(play_id, scope)?;
        let expiry = jp::terminal_expiry(now_ms)?;
        Ok(self
            .client()
            .execute(
                jp::END,
                params!(
                    expiry,
                    play_id,
                    scope.user_id,
                    &scope.token_digest,
                    &scope.device_digest,
                    scope.client_family.as_str()
                ),
            )
            .await?
            == 1)
    }
    async fn withdraw_pending_jellyfin_play(
        &self,
        play_id: &str,
        scope: &JellyfinPlayScope,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        jp::validate_key(play_id, scope)?;
        let expiry = jp::terminal_expiry(now_ms)?;
        Ok(self
            .client()
            .execute(
                jp::WITHDRAW_PENDING,
                params!(
                    expiry,
                    play_id,
                    scope.user_id,
                    &scope.token_digest,
                    &scope.device_digest,
                    scope.client_family.as_str()
                ),
            )
            .await?
            == 1)
    }
}
