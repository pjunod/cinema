use super::hiqlite::{database_error, HiqliteAuthStore};
use crate::{error::StoreError, store::remote::*};
use async_trait::async_trait;
use hiqlite::{macros::params, Row};
impl From<&mut Row<'_>> for RemoteReceiver {
    fn from(r: &mut Row<'_>) -> Self {
        Self {
            id: r.get("id"),
            user_id: r.get("user_id"),
            name: r.get("name"),
            platform: r.get("platform"),
            created_at: r.get("created_at"),
        }
    }
}
impl From<&mut Row<'_>> for RemoteGrant {
    fn from(r: &mut Row<'_>) -> Self {
        Self {
            id: r.get("id"),
            receiver_id: r.get("receiver_id"),
            name: r.get("name"),
            created_at: r.get("created_at"),
        }
    }
}
#[async_trait]
impl RemoteStore for HiqliteAuthStore {
    async fn create_remote_receiver(&self, new: NewRemoteReceiver) -> Result<bool, StoreError> {
        let r = new.receiver;
        Ok(self
            .execute(
                INSERT_RECEIVER,
                params!(
                    r.id,
                    r.user_id,
                    r.name,
                    r.platform,
                    new.secret_hash,
                    r.created_at
                ),
            )
            .await?
            > 0)
    }
    async fn create_remote_grant(&self, new: NewRemoteGrant) -> Result<bool, StoreError> {
        let g = new.grant;
        Ok(self
            .execute(
                INSERT_GRANT,
                params!(
                    g.id,
                    g.receiver_id,
                    new.user_id,
                    g.name,
                    new.secret_hash,
                    g.created_at
                ),
            )
            .await?
            > 0)
    }
    async fn remote_authority(
        &self,
        id: &str,
        user: i64,
        proof: RemoteProof,
    ) -> Result<Option<RemoteReceiver>, StoreError> {
        let (mode, a, b) = proof.args()?;
        Ok(self
            .client()
            // authority: current receiver/grant revocation and ownership decide control admission.
            .query_consistent_map::<RemoteReceiver, _>(AUTHORITY, params!(id, user, mode, a, b))
            .await
            .map_err(database_error)?
            .into_iter()
            .next())
    }
    async fn remote_receiver_metadata(
        &self,
        id: &str,
        user: i64,
    ) -> Result<Option<RemoteReceiver>, StoreError> {
        Ok(self
            .client()
            // authority: return current active receiver identity after credential revocation.
            .query_consistent_map::<RemoteReceiver, _>(METADATA, params!(id, user))
            .await
            .map_err(database_error)?
            .into_iter()
            .next())
    }
    async fn remote_receivers(&self, user: i64) -> Result<Vec<RemoteReceiver>, StoreError> {
        self.client()
            // authority: publish active receiver catalogue from committed revocation state.
            .query_consistent_map::<RemoteReceiver, _>(LIST_RECEIVERS, params!(user))
            .await
            .map_err(database_error)
    }
    async fn remote_grants(&self, user: i64) -> Result<Vec<RemoteGrant>, StoreError> {
        self.client()
            // authority: publish current grants for the owner without resurrecting a revoked grant.
            .query_consistent_map::<RemoteGrant, _>(LIST_GRANTS, params!(user))
            .await
            .map_err(database_error)
    }
    async fn revoke_remote_receiver(
        &self,
        id: &str,
        user: i64,
        now: i64,
    ) -> Result<(), StoreError> {
        self.client().txn(vec![(REVOKE_RECEIVER,params!(now,id,user)),("UPDATE remote_grants SET revoked_at=$1 WHERE receiver_id=$2 AND user_id=$3 AND revoked_at IS NULL",params!(now,id,user))]).await?;
        Ok(())
    }
    async fn revoke_remote_grant(&self, id: &str, user: i64, now: i64) -> Result<(), StoreError> {
        self.execute(REVOKE_GRANT, params!(now, id, user)).await?;
        Ok(())
    }
    async fn admit_remote_pair_claim(&self, user: i64, now: i64) -> Result<bool, StoreError> {
        Ok(self.execute(CLAIM_BUDGET, params!(user, now)).await? > 0)
    }
}
