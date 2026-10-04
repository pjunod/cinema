use super::SqliteStore;
use crate::error::StoreError;
use crate::store::jellyfin_play as jp;
use crate::store::{
    JellyfinPlay, JellyfinPlayActivation, JellyfinPlayScope, JellyfinPlayStore, NewJellyfinPlay,
};
use async_trait::async_trait;
use rusqlite::{params, OptionalExtension};

#[async_trait]
impl JellyfinPlayStore for SqliteStore {
    async fn end_jellyfin_login_plays(
        &self,
        scope: &JellyfinPlayScope,
        now_ms: i64,
    ) -> Result<Vec<JellyfinPlay>, StoreError> {
        jp::validate_scope(scope)?;
        let expiry = jp::terminal_expiry(now_ms)?;
        let scope = scope.clone();
        let rows = self
            .with_conn(move |conn| {
                let mut stmt = conn.prepare(jp::END_LOGIN)?;
                let rows = stmt
                    .query_map(
                        params![
                            expiry,
                            scope.user_id,
                            scope.token_digest,
                            scope.device_digest,
                            scope.client_family.as_str()
                        ],
                        |row| {
                            Ok(jp::RawPlay {
                                payload: row.get(0)?,
                                state: row.get(1)?,
                                expires_at_ms: row.get(2)?,
                                manual_revision: row.get(3)?,
                                native_incarnation_id: row.get(4)?,
                                direct_grant_id: row.get(5)?,
                            })
                        },
                    )?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await?;
        rows.into_iter().map(jp::RawPlay::decode).collect()
    }

    async fn create_jellyfin_play(&self, play: NewJellyfinPlay) -> Result<bool, StoreError> {
        let (payload, expiry) = jp::encode(&play)?;
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(jp::CLEANUP, params![play.created_at_ms])?;
            tx.execute(jp::RETIRE_ORPHANED_ACTIVE, params![play.created_at_ms])?;
            let created = tx.execute(
                jp::CREATE,
                params![
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
                ],
            )?;
            tx.commit()?;
            Ok(created == 1)
        })
        .await
    }
    async fn jellyfin_play(
        &self,
        play_id: &str,
        scope: &JellyfinPlayScope,
    ) -> Result<Option<JellyfinPlay>, StoreError> {
        jp::validate_key(play_id, scope)?;
        let id = play_id.to_owned();
        let scope = scope.clone();
        let row = self
            .with_conn(move |conn| {
                Ok(conn
                    .query_row(
                        jp::READ,
                        params![
                            id,
                            scope.user_id,
                            scope.token_digest,
                            scope.device_digest,
                            scope.client_family.as_str()
                        ],
                        |row| {
                            Ok(jp::RawPlay {
                                payload: row.get(0)?,
                                state: row.get(1)?,
                                expires_at_ms: row.get(2)?,
                                manual_revision: row.get(3)?,
                                native_incarnation_id: row.get(4)?,
                                direct_grant_id: row.get(5)?,
                            })
                        },
                    )
                    .optional()?)
            })
            .await?;
        row.map(jp::RawPlay::decode).transpose()
    }
    async fn jellyfin_play_for_direct_grant(
        &self,
        grant_id: &str,
    ) -> Result<Option<JellyfinPlay>, StoreError> {
        if !jp::reference(grant_id) {
            return Ok(None);
        }
        let id = grant_id.to_owned();
        let row = self
            .with_conn(move |conn| {
                Ok(conn
                    .query_row(jp::READ_BY_DIRECT_GRANT, params![id], |row| {
                        Ok(jp::RawPlay {
                            payload: row.get(0)?,
                            state: row.get(1)?,
                            expires_at_ms: row.get(2)?,
                            manual_revision: row.get(3)?,
                            native_incarnation_id: row.get(4)?,
                            direct_grant_id: row.get(5)?,
                        })
                    })
                    .optional()?)
            })
            .await?;
        row.map(jp::RawPlay::decode).transpose()
    }
    async fn jellyfin_plays_superseded_by(
        &self,
        play_id: &str,
        scope: &JellyfinPlayScope,
    ) -> Result<Vec<JellyfinPlay>, StoreError> {
        jp::validate_key(play_id, scope)?;
        let id = play_id.to_owned();
        let scope = scope.clone();
        let rows = self
            .with_conn(move |conn| {
                let mut stmt = conn.prepare(jp::READ_SUPERSEDED_BY)?;
                let rows = stmt
                    .query_map(
                        params![
                            scope.user_id,
                            id,
                            scope.token_digest,
                            scope.device_digest,
                            scope.client_family.as_str()
                        ],
                        |row| {
                            Ok(jp::RawPlay {
                                payload: row.get(0)?,
                                state: row.get(1)?,
                                expires_at_ms: row.get(2)?,
                                manual_revision: row.get(3)?,
                                native_incarnation_id: row.get(4)?,
                                direct_grant_id: row.get(5)?,
                            })
                        },
                    )?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await?;
        rows.into_iter().map(jp::RawPlay::decode).collect()
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
        let id = play_id.to_owned();
        let scope = scope.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let activated = tx.execute(
                sql,
                params![
                    reference,
                    i64::MAX,
                    id,
                    scope.user_id,
                    scope.token_digest,
                    scope.device_digest,
                    scope.client_family.as_str(),
                    now_ms,
                    nonce
                ],
            )? == 1;
            if activated {
                tx.execute(
                    jp::SUPERSEDE_AFTER_BINDING_ACTIVATION,
                    params![
                        id,
                        scope.user_id,
                        scope.token_digest,
                        scope.device_digest,
                        scope.client_family.as_str(),
                        expiry,
                        nonce
                    ],
                )?;
            }
            tx.commit()?;
            Ok(activated)
        })
        .await
    }
    async fn end_jellyfin_play(
        &self,
        play_id: &str,
        scope: &JellyfinPlayScope,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        jp::validate_key(play_id, scope)?;
        let expiry = jp::terminal_expiry(now_ms)?;
        let id = play_id.to_owned();
        let scope = scope.clone();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                jp::END,
                params![
                    expiry,
                    id,
                    scope.user_id,
                    scope.token_digest,
                    scope.device_digest,
                    scope.client_family.as_str()
                ],
            )? == 1)
        })
        .await
    }
}
