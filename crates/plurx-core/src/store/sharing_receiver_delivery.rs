//! Exact published receiver grants, guarded identically on both backends.
use super::{
    sharing::{Backend, Value},
    sharing_receiver_sessions::{
        source_assert, source_current, source_values, source_write_refused, ATTACHED, PUBLISHED,
    },
};
use crate::{
    error::StoreError,
    sharing::{invalid, is_hash},
    sharing_receiver_delivery::{ReceiverDeliveryGrant, ReceiverDeliveryWrite},
    sharing_receiver_sessions::{ReceiverSessionWriteAuthority, ReceiverSourceAttachment},
};
use async_trait::async_trait;

#[async_trait]
pub trait SharingReceiverDeliveryStore: Send + Sync {
    async fn issue_receiver_delivery(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        attachment: &ReceiverSourceAttachment,
        grant: &ReceiverDeliveryGrant,
    ) -> Result<ReceiverDeliveryWrite, StoreError>;
    async fn receiver_delivery(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        attachment: &ReceiverSourceAttachment,
        token_hash: &str,
    ) -> Result<Option<ReceiverDeliveryGrant>, StoreError>;
    async fn revoke_receiver_delivery(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        attachment: &ReceiverSourceAttachment,
        token_hash: &str,
    ) -> Result<ReceiverDeliveryWrite, StoreError>;
}
fn grant_identity() -> &'static str {
    "g.token_hash=$20 AND g.incarnation_id=$6 AND g.source_token_hash=$1"
}
fn valid_deadline() -> &'static str {
    "$21>$14 AND $21<=$13 AND $21<=$14+30000"
}
#[async_trait]
impl<T: Backend> SharingReceiverDeliveryStore for T {
    async fn issue_receiver_delivery(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        attachment: &ReceiverSourceAttachment,
        grant: &ReceiverDeliveryGrant,
    ) -> Result<ReceiverDeliveryWrite, StoreError> {
        if !is_hash(&grant.token_hash) {
            return Err(invalid());
        }
        let Some(mut values) = source_values(authority, attachment)? else {
            return Ok(ReceiverDeliveryWrite::Refused);
        };
        if authority
            .login_expires_at_s
            .is_some_and(|d| grant.deadline_ms > d.saturating_mul(1000))
        {
            return Ok(ReceiverDeliveryWrite::Refused);
        }
        values.extend([grant.token_hash.clone().into(), grant.deadline_ms.into()]);
        let current = source_current(ATTACHED, PUBLISHED);
        let exact=format!("EXISTS(SELECT 1 FROM sharing_delivery_grants g WHERE {} AND g.state='active' AND g.deadline_ms=$21)",grant_identity());
        let accepted=format!("({current}) AND ({}) AND (NOT EXISTS(SELECT 1 FROM sharing_delivery_grants g WHERE g.token_hash=$20) OR ({exact}))",valid_deadline());
        let sql="INSERT INTO sharing_delivery_grants(token_hash,incarnation_id,source_token_hash,state,deadline_ms) SELECT $1,$2,$3,'active',$4 WHERE NOT EXISTS(SELECT 1 FROM sharing_delivery_grants WHERE token_hash=$1)";
        let statements = vec![
            source_assert(accepted, values.clone()),
            (
                sql.into(),
                vec![
                    grant.token_hash.clone().into(),
                    attachment.owner.incarnation_id.into(),
                    authority.intent.login_hash.clone().into(),
                    grant.deadline_ms.into(),
                ],
            ),
            source_assert(format!("({current}) AND ({exact})"), values),
        ];
        result(self, statements).await
    }
    async fn receiver_delivery(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        attachment: &ReceiverSourceAttachment,
        token_hash: &str,
    ) -> Result<Option<ReceiverDeliveryGrant>, StoreError> {
        if !is_hash(token_hash) {
            return Err(invalid());
        }
        let Some(mut values) = source_values(authority, attachment)? else {
            return Ok(None);
        };
        values.push(token_hash.to_owned().into());
        let login = authority
            .login_expires_at_s
            .map(|s| s.saturating_mul(1000))
            .unwrap_or(i64::MAX);
        values.push(login.into());
        let sql=format!("SELECT CAST(g.deadline_ms AS TEXT) AS payload FROM sharing_delivery_grants g WHERE {} AND g.state='active' AND g.deadline_ms>$14 AND g.deadline_ms<=$14+30000 AND g.deadline_ms<=$13 AND g.deadline_ms<=$21 AND ({}) LIMIT 2",grant_identity(),source_current(ATTACHED,PUBLISHED));
        let rows = self.sharing_read(&sql, values).await?;
        match rows.as_slice() {
            [] => Ok(None),
            [deadline] => Ok(Some(ReceiverDeliveryGrant {
                token_hash: token_hash.into(),
                deadline_ms: deadline.parse().map_err(|_| invalid())?,
            })),
            _ => Err(invalid()),
        }
    }
    async fn revoke_receiver_delivery(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        attachment: &ReceiverSourceAttachment,
        token_hash: &str,
    ) -> Result<ReceiverDeliveryWrite, StoreError> {
        if !is_hash(token_hash) {
            return Err(invalid());
        }
        let Some(mut values) = source_values(authority, attachment)? else {
            return Ok(ReceiverDeliveryWrite::Refused);
        };
        values.push(token_hash.to_owned().into());
        let current = source_current(ATTACHED, PUBLISHED);
        let identity = grant_identity();
        let before=format!("({current}) AND EXISTS(SELECT 1 FROM sharing_delivery_grants g WHERE {identity} AND g.state IN('active','revoked'))");
        let after=format!("({current}) AND EXISTS(SELECT 1 FROM sharing_delivery_grants g WHERE {identity} AND g.state='revoked')");
        let sql="UPDATE sharing_delivery_grants SET state='revoked' WHERE token_hash=$1 AND incarnation_id=$2 AND source_token_hash=$3 AND state='active'";
        result(
            self,
            vec![
                source_assert(before, values.clone()),
                (
                    sql.into(),
                    vec![
                        token_hash.to_owned().into(),
                        attachment.owner.incarnation_id.into(),
                        authority.intent.login_hash.clone().into(),
                    ],
                ),
                source_assert(after, values),
            ],
        )
        .await
    }
}
async fn result<T: Backend>(
    store: &T,
    statements: Vec<(String, Vec<Value>)>,
) -> Result<ReceiverDeliveryWrite, StoreError> {
    match store.sharing_txn(statements).await {
        Ok(counts) if counts == [0, 1, 0] => Ok(ReceiverDeliveryWrite::Applied),
        Ok(counts) if counts == [0, 0, 0] => Ok(ReceiverDeliveryWrite::Replay),
        Ok(_) => Err(invalid()),
        Err(e) if source_write_refused(&e) => Ok(ReceiverDeliveryWrite::Refused),
        Err(e) => Err(e),
    }
}
