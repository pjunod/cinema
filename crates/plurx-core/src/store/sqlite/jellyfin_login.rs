//! Standalone compatibility login replacement shares the native token table.
use super::SqliteStore;
use crate::error::StoreError;
use crate::store::{CacheAdminMutationClaim, JellyfinLoginStore, JellyfinLoginWrite};
use async_trait::async_trait;
use rusqlite::params;

#[async_trait]
impl JellyfinLoginStore for SqliteStore {
    async fn replace_jellyfin_login(
        &self,
        w: JellyfinLoginWrite,
        claim: Option<&CacheAdminMutationClaim>,
    ) -> Result<bool, StoreError> {
        super::users::require_standalone_claim(claim)?;
        crate::store::jellyfin_login::validate_write(&w)?;
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let minted = tx.execute(
                "INSERT INTO tokens(token_hash,user_id,device,created_at,last_seen_at) \
                 SELECT ?1,id,?2,?3,?3 FROM users WHERE id=?4 AND password_hash=?5",
                params![
                    w.token_hash,
                    w.device_label,
                    w.created_at,
                    w.user_id,
                    w.expected_password_hash
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
