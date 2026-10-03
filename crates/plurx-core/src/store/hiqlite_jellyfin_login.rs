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
#[async_trait]
impl JellyfinLoginStore for HiqliteAuthStore {
    async fn replace_jellyfin_login(
        &self,
        w: JellyfinLoginWrite,
        claim: Option<&CacheAdminMutationClaim>,
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
        let changed = self.credential_mutation(statements).await?;
        Ok(changed[0] > 0 && changed[2] > 0)
    }
}
