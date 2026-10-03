use super::hiqlite::HiqliteAuthStore;
use super::{CacheAdminMutationClaim, JellyfinLoginStore, JellyfinLoginWrite};
use crate::error::StoreError;
use async_trait::async_trait;
use hiqlite::macros::params;
const MINT: &str = "INSERT INTO tokens(token_hash,user_id,device,created_at,last_seen_at) SELECT $1,id,$2,$3,$3 FROM users WHERE id=$4 AND password_hash=$5";
const MINT_CLAIM: &str = "INSERT INTO tokens(token_hash,user_id,device,created_at,last_seen_at) SELECT $1,id,$2,$3,$3 FROM users WHERE id=$4 AND password_hash=$5 AND EXISTS(SELECT 1 FROM cluster_cache_admin_revocation_leases WHERE claim_id=$6)";
const RETIRE: &str = "DELETE FROM tokens WHERE user_id=$1 AND token_hash IN (SELECT token_hash FROM jellyfin_login_tokens WHERE user_id=$1 AND device_digest=$2 AND client_family=$3) AND token_hash<>$4 AND EXISTS(SELECT 1 FROM tokens n JOIN users u ON u.id=n.user_id WHERE n.token_hash=$4 AND u.id=$1 AND u.password_hash=$5)";
const RETIRE_CLAIM: &str = "DELETE FROM tokens WHERE user_id=$1 AND token_hash IN (SELECT token_hash FROM jellyfin_login_tokens WHERE user_id=$1 AND device_digest=$2 AND client_family=$3) AND token_hash<>$4 AND EXISTS(SELECT 1 FROM tokens n JOIN users u ON u.id=n.user_id WHERE n.token_hash=$4 AND u.id=$1 AND u.password_hash=$5) AND EXISTS(SELECT 1 FROM cluster_cache_admin_revocation_leases WHERE claim_id=$6)";
const MAP: &str = "INSERT INTO jellyfin_login_tokens(token_hash,user_id,device_digest,client_family) SELECT $1,user_id,$2,$3 FROM tokens WHERE token_hash=$1 AND user_id=$4 AND EXISTS(SELECT 1 FROM users WHERE id=$4 AND password_hash=$5) ON CONFLICT(user_id,device_digest,client_family) DO UPDATE SET token_hash=excluded.token_hash";
const MAP_CLAIM: &str = "INSERT INTO jellyfin_login_tokens(token_hash,user_id,device_digest,client_family) SELECT $1,user_id,$2,$3 FROM tokens WHERE token_hash=$1 AND user_id=$4 AND EXISTS(SELECT 1 FROM users WHERE id=$4 AND password_hash=$5) AND EXISTS(SELECT 1 FROM cluster_cache_admin_revocation_leases WHERE claim_id=$6) ON CONFLICT(user_id,device_digest,client_family) DO UPDATE SET token_hash=excluded.token_hash";
const MINT_ENABLED: &str = "INSERT INTO tokens(token_hash,user_id,device,created_at,last_seen_at) SELECT $1,id,$2,$3,$3 FROM users WHERE id=$4 AND password_hash=$5 AND lower(trim(COALESCE((SELECT value FROM settings WHERE key='compat.jellyfin.enabled'),''),char(9)||char(10)||char(11)||char(12)||char(13)||' ')) IN ('1','true','yes','on') AND (SELECT value FROM settings WHERE key='compat.jellyfin.generation')=$6";
const MINT_CLAIM_ENABLED: &str = "INSERT INTO tokens(token_hash,user_id,device,created_at,last_seen_at) SELECT $1,id,$2,$3,$3 FROM users WHERE id=$4 AND password_hash=$5 AND EXISTS(SELECT 1 FROM cluster_cache_admin_revocation_leases WHERE claim_id=$6) AND lower(trim(COALESCE((SELECT value FROM settings WHERE key='compat.jellyfin.enabled'),''),char(9)||char(10)||char(11)||char(12)||char(13)||' ')) IN ('1','true','yes','on') AND (SELECT value FROM settings WHERE key='compat.jellyfin.generation')=$7";
struct SwitchRow(String);
impl From<&mut hiqlite::Row<'_>> for SwitchRow {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self(row.get("result_json"))
    }
}
#[async_trait]
impl JellyfinLoginStore for HiqliteAuthStore {
    async fn jellyfin_login_scope(
        &self,
        token_hash: String,
    ) -> Result<Option<super::JellyfinPlayScope>, StoreError> {
        let row = self
            .client()
            // authority: play and manual-edit origin must use current native token membership, never client-claimed device context.
            .query_consistent_map::<SwitchRow, _>(
                super::jellyfin_login::LOGIN_SCOPE,
                params!(token_hash),
            )
            .await?
            .into_iter()
            .next();
        row.map(|row| {
            serde_json::from_str(&row.0).map_err(|e| StoreError::Credential(e.to_string()))
        })
        .transpose()
    }
    async fn jellyfin_compatibility_state(
        &self,
    ) -> Result<crate::store::JellyfinCompatibilityState, StoreError> {
        let row = self
            .client()
            // authority: login admission observes enabled choice and generation from one committed snapshot.
            .query_consistent_map::<SwitchRow, _>(super::jellyfin_login::SWITCH_STATE, params!())
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                StoreError::Credential("missing compatibility setting snapshot".into())
            })?;
        serde_json::from_str(&row.0).map_err(|e| StoreError::Credential(e.to_string()))
    }
    async fn set_jellyfin_compatibility(&self, enabled: bool) -> Result<(), StoreError> {
        let generation = uuid::Uuid::new_v4().simple().to_string();
        let now = super::jellyfin_login::switch_save_time();
        let results = self
            .client()
            .txn([
                (
                    super::jellyfin_login::SAVE_GENERATION,
                    params!(generation, now),
                ),
                (
                    super::jellyfin_login::SAVE_SWITCH,
                    params!(if enabled { "1" } else { "0" }, now),
                ),
            ])
            .await?;
        for result in results {
            result.map_err(super::hiqlite::database_error)?;
        }
        Ok(())
    }
    async fn replace_jellyfin_login_if_enabled(
        &self,
        w: JellyfinLoginWrite,
        claim: Option<&CacheAdminMutationClaim>,
        generation: &str,
    ) -> Result<bool, StoreError> {
        super::jellyfin_login::validate_generation(generation)?;
        self.replace_jellyfin_login_inner(w, claim, Some(generation))
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
impl HiqliteAuthStore {
    async fn replace_jellyfin_login_inner(
        &self,
        w: JellyfinLoginWrite,
        claim: Option<&CacheAdminMutationClaim>,
        generation: Option<&str>,
    ) -> Result<bool, StoreError> {
        super::jellyfin_login::validate_write(&w)?;
        let mut statements = vec![
            (
                MINT,
                params!(
                    w.token_hash.clone(),
                    w.device_label,
                    w.created_at,
                    w.user_id,
                    w.expected_password_hash.clone()
                ),
            ),
            (
                RETIRE,
                params!(
                    w.user_id,
                    w.device_digest.clone(),
                    w.client_family.as_str(),
                    w.token_hash.clone(),
                    w.expected_password_hash.clone()
                ),
            ),
            (
                MAP,
                params!(
                    w.token_hash,
                    w.device_digest,
                    w.client_family.as_str(),
                    w.user_id,
                    w.expected_password_hash
                ),
            ),
        ];
        if let Some(claim) = claim {
            for ((sql, args), guarded) in
                statements
                    .iter_mut()
                    .zip([MINT_CLAIM, RETIRE_CLAIM, MAP_CLAIM])
            {
                *sql = guarded;
                args.push(claim.as_str().to_owned().into());
            }
        }
        if let Some(generation) = generation {
            statements[0].0 = if claim.is_some() {
                MINT_CLAIM_ENABLED
            } else {
                MINT_ENABLED
            };
            statements[0].1.push(generation.to_owned().into());
        }
        let changed = self.credential_mutation(statements).await?;
        Ok(changed[0] > 0 && changed[2] > 0)
    }
}
