use super::SqliteStore;
use crate::{error::StoreError, store::remote::*};
use async_trait::async_trait;
use rusqlite::{params, OptionalExtension};
fn receiver(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteReceiver> {
    Ok(RemoteReceiver {
        id: row.get(0)?,
        user_id: row.get(1)?,
        name: row.get(2)?,
        platform: row.get(3)?,
        created_at: row.get(4)?,
    })
}
#[async_trait]
impl RemoteStore for SqliteStore {
    async fn create_remote_receiver(&self, new: NewRemoteReceiver) -> Result<bool, StoreError> {
        self.with_conn(move |c| {
            let r = new.receiver;
            Ok(c.execute(
                INSERT_RECEIVER,
                params![
                    r.id,
                    r.user_id,
                    r.name,
                    r.platform,
                    new.secret_hash,
                    r.created_at
                ],
            )? > 0)
        })
        .await
    }
    async fn create_remote_grant(&self, new: NewRemoteGrant) -> Result<bool, StoreError> {
        self.with_conn(move |c| {
            let g = new.grant;
            Ok(c.execute(
                INSERT_GRANT,
                params![
                    g.id,
                    g.receiver_id,
                    new.user_id,
                    g.name,
                    new.secret_hash,
                    g.created_at
                ],
            )? > 0)
        })
        .await
    }
    async fn remote_authority(
        &self,
        id: &str,
        user: i64,
        proof: RemoteProof,
    ) -> Result<Option<RemoteReceiver>, StoreError> {
        let (mode, a, b) = proof.args()?;
        let (id, a, b) = (id.to_owned(), a.to_owned(), b.to_owned());
        self.with_conn(move |c| {
            Ok(
                c.query_row(AUTHORITY, params![id, user, mode, a, b], receiver)
                    .optional()?,
            )
        })
        .await
    }
    async fn remote_receiver_metadata(
        &self,
        id: &str,
        user: i64,
    ) -> Result<Option<RemoteReceiver>, StoreError> {
        let id = id.to_owned();
        self.with_conn(move |c| {
            Ok(c.query_row(METADATA, params![id, user], receiver)
                .optional()?)
        })
        .await
    }
    async fn remote_receivers(&self, user: i64) -> Result<Vec<RemoteReceiver>, StoreError> {
        self.with_conn(move |c| {
            Ok(c.prepare(LIST_RECEIVERS)?
                .query_map([user], receiver)?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }
    async fn remote_grants(&self, user: i64) -> Result<Vec<RemoteGrant>, StoreError> {
        self.with_conn(move |c| {
            Ok(c.prepare(LIST_GRANTS)?
                .query_map([user], |r| {
                    Ok(RemoteGrant {
                        id: r.get(0)?,
                        receiver_id: r.get(1)?,
                        name: r.get(2)?,
                        created_at: r.get(3)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }
    async fn revoke_remote_receiver(
        &self,
        id: &str,
        user: i64,
        now: i64,
    ) -> Result<(), StoreError> {
        let id = id.to_owned();
        self.with_conn(move|c| { let tx=c.unchecked_transaction()?; tx.execute(REVOKE_RECEIVER,params![now,id,user])?; tx.execute("UPDATE remote_grants SET revoked_at=$1 WHERE receiver_id=$2 AND user_id=$3 AND revoked_at IS NULL",params![now,id,user])?;tx.commit()?;Ok(()) }).await
    }
    async fn revoke_remote_grant(&self, id: &str, user: i64, now: i64) -> Result<(), StoreError> {
        let id = id.to_owned();
        self.with_conn(move |c| {
            c.execute(REVOKE_GRANT, params![now, id, user])?;
            Ok(())
        })
        .await
    }
    async fn admit_remote_pair_claim(&self, user: i64, now: i64) -> Result<bool, StoreError> {
        self.with_conn(move |c| Ok(c.execute(CLAIM_BUDGET, params![user, now])? > 0))
            .await
    }
}
