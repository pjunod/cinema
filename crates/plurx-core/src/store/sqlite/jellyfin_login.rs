//! Standalone compatibility login replacement shares the native token table.
use super::SqliteStore;
use crate::error::StoreError;
use crate::store::{CacheAdminMutationClaim, JellyfinLoginStore, JellyfinLoginWrite};
use async_trait::async_trait;
use rusqlite::{params, OptionalExtension};

#[async_trait]
impl JellyfinLoginStore for SqliteStore {
    async fn jellyfin_login_scope(
        &self,
        token_hash: String,
    ) -> Result<Option<crate::store::JellyfinPlayScope>, StoreError> {
        self.with_conn(move |conn| {
            let json: Option<String> = conn
                .query_row(
                    crate::store::jellyfin_login::LOGIN_SCOPE,
                    params![token_hash],
                    |row| row.get(0),
                )
                .optional()?;
            json.map(|json| {
                serde_json::from_str(&json).map_err(|e| StoreError::Credential(e.to_string()))
            })
            .transpose()
        })
        .await
    }
    async fn jellyfin_compatibility_state(
        &self,
    ) -> Result<crate::store::JellyfinCompatibilityState, StoreError> {
        self.with_conn(|conn| {
            let text: String =
                conn.query_row(crate::store::jellyfin_login::SWITCH_STATE, [], |row| {
                    row.get(0)
                })?;
            serde_json::from_str(&text).map_err(|e| StoreError::Credential(e.to_string()))
        })
        .await
    }
    async fn set_jellyfin_compatibility(&self, enabled: bool) -> Result<(), StoreError> {
        let generation = uuid::Uuid::new_v4().simple().to_string();
        let now = crate::store::jellyfin_login::switch_save_time();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                crate::store::jellyfin_login::SAVE_GENERATION,
                params![generation, now],
            )?;
            tx.execute(
                crate::store::jellyfin_login::SAVE_SWITCH,
                params![if enabled { "1" } else { "0" }, now],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
    async fn replace_jellyfin_login_if_enabled(
        &self,
        w: JellyfinLoginWrite,
        claim: Option<&CacheAdminMutationClaim>,
        generation: &str,
    ) -> Result<bool, StoreError> {
        crate::store::jellyfin_login::validate_generation(generation)?;
        self.replace_jellyfin_login_inner(w, claim, Some(generation.to_owned()))
            .await
    }
    async fn replace_jellyfin_login(
        &self,
        w: JellyfinLoginWrite,
        claim: Option<&CacheAdminMutationClaim>,
    ) -> Result<bool, StoreError> {
        self.replace_jellyfin_login_inner(w, claim, None).await
    }
}
impl SqliteStore {
    async fn replace_jellyfin_login_inner(
        &self,
        w: JellyfinLoginWrite,
        claim: Option<&CacheAdminMutationClaim>,
        generation: Option<String>,
    ) -> Result<bool, StoreError> {
        super::users::require_standalone_claim(claim)?;
        crate::store::jellyfin_login::validate_write(&w)?;
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let minted = tx.execute(
                "INSERT INTO tokens(token_hash,user_id,device,created_at,last_seen_at) \
                 SELECT ?1,id,?2,?3,?3 FROM users WHERE id=?4 AND password_hash=?5 AND (?6 IS NULL OR (lower(trim(COALESCE((SELECT value FROM settings WHERE key='compat.jellyfin.enabled'),''),char(9)||char(10)||char(11)||char(12)||char(13)||' ')) IN ('1','true','yes','on') AND (SELECT value FROM settings WHERE key='compat.jellyfin.generation')=?6))",
                params![
                    w.token_hash,
                    w.device_label,
                    w.created_at,
                    w.user_id,
                    w.expected_password_hash,
                    generation
                ],
            )?;
            if minted == 0 {
                return Ok(false);
            }
            tx.execute(
                "DELETE FROM tokens WHERE user_id=?1 AND token_hash IN \
                 (SELECT token_hash FROM jellyfin_login_tokens \
                  WHERE user_id=?1 AND device_digest=?2 AND client_family=?3) \
                 AND token_hash<>?4",
                params![
                    w.user_id,
                    w.device_digest,
                    w.client_family.as_str(),
                    w.token_hash
                ],
            )?;
            tx.execute(
                "INSERT INTO jellyfin_login_tokens(token_hash,user_id,device_digest,client_family) \
                 VALUES (?1,?2,?3,?4) ON CONFLICT(user_id,device_digest,client_family) \
                 DO UPDATE SET token_hash=excluded.token_hash",
                params![w.token_hash, w.user_id, w.device_digest, w.client_family.as_str()],
            )?;
            tx.commit()?;
            Ok(true)
        })
        .await
    }
}
