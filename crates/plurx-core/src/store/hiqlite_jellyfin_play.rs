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
                        play.file_wire_id
                    ),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(results[1] == 1)
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
}
