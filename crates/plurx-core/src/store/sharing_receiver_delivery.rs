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
    /// Read-only ingress authority over the exact currently published route.
    async fn receiver_relay_read_authority(
        &self,
        route: &crate::domain::MediaSessionRoute,
    ) -> Result<Option<crate::sharing_receiver_delivery::ReceiverRelayReadAuthority>, StoreError>;
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
    async fn receiver_relay_read_authority(
        &self,
        route: &crate::domain::MediaSessionRoute,
    ) -> Result<Option<crate::sharing_receiver_delivery::ReceiverRelayReadAuthority>, StoreError>
    {
        relay_read(self, route).await
    }
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

/// Decode metadata only, then repeat the existing guarded login, binding and
/// grant reads. A durable recipe is never turned into a process-local actor.
async fn relay_read<T: Backend>(
    store: &T,
    route: &crate::domain::MediaSessionRoute,
) -> Result<Option<crate::sharing_receiver_delivery::ReceiverRelayReadAuthority>, StoreError> {
    use super::SharingReceiverSessionStore;
    use crate::sharing_receiver_sessions::{
        ReceiverSessionIntent, ReceiverSourceOwner, RemoteSourceRecipe,
    };
    use crate::store::sharing_catalogue::ReceiverCatalogueScope;
    use serde::Deserialize;
    use sha2::Digest;
    use uuid::Uuid;
    let Some(user_id) = route.principal.local_user_id() else {
        return Ok(None);
    };
    let Ok(recipe) = serde_json::from_str::<RemoteSourceRecipe>(&route.recipe_json) else {
        return Ok(None);
    };
    if route.state != "active" || route.publication_ready_at_ms != 0 {
        return Ok(None);
    }
    let now = super::sharing::wall_clock_ms()?;
    let rows = store.sharing_read(
        "SELECT json_object('request',r.request_id,'claim',i.claim_id,'grant',i.remote_grant_id,'assignment',b.assignment_generation,'endpoint',b.endpoint_revision,'position',b.source_position_ms) AS payload FROM sharing_relay_upstream b JOIN media_sessions s ON s.incarnation_id=b.incarnation_id JOIN media_session_requests r ON r.incarnation_id=s.incarnation_id AND r.user_id=s.user_id JOIN sharing_imports i ON i.id=b.import_id WHERE s.session_id=$1 AND s.incarnation_id=$2 AND s.owner_node_id=$3 AND s.owner_epoch=$4 AND s.recipe_json=$5 AND s.lease_expires_at_ms=$6 AND s.state='active' AND s.publication_ready_at_ms=0 AND length(r.request_id)<=128 LIMIT 2",
        vec![route.session_id.clone().into(), route.incarnation_id.clone().into(), route.owner_node_id.clone().into(), route.owner_epoch.into(), route.recipe_json.clone().into(), route.lease_expires_at_ms.into()],
    ).await?;
    let [row] = rows.as_slice() else {
        return if rows.is_empty() {
            Ok(None)
        } else {
            Err(invalid())
        };
    };
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Row {
        request: String,
        claim: Uuid,
        grant: Uuid,
        assignment: i64,
        endpoint: i64,
        position: i64,
    }
    let row: Row = serde_json::from_str(row).map_err(|_| invalid())?;
    let intent = ReceiverSessionIntent {
        scope: ReceiverCatalogueScope {
            import_id: recipe.reference.import_id,
            source_server_id: recipe.reference.server_id,
            catalogue_epoch: recipe.reference.catalogue_epoch,
            lifecycle_generation: recipe.lifecycle_generation,
            assignment_generation: row.assignment,
            endpoint_generation: row.endpoint,
            claim_id: row.claim,
            remote_grant_id: row.grant,
            libraries: vec![recipe.reference.library_id.clone()],
        },
        user_id,
        login_hash: recipe.parent_login_hash.clone(),
        recipe,
        source_position_ms: row.position,
    };
    let Some(authority) = store.prepare_receiver_session_authority(intent).await? else {
        return Ok(None);
    };
    let owner = ReceiverSourceOwner {
        incarnation_id: Uuid::parse_str(&route.incarnation_id).map_err(|_| invalid())?,
        session_id: Uuid::parse_str(&route.session_id).map_err(|_| invalid())?,
        owner_node_id: route.owner_node_id.clone(),
        owner_epoch: route.owner_epoch,
        request_id: row.request,
        lease_expires_at_ms: route.lease_expires_at_ms,
        now_ms: now,
    };
    let Some(snapshot) = store.receiver_source_binding(&authority, &owner).await? else {
        return Ok(None);
    };
    if snapshot.response_json.is_none() {
        return Ok(None);
    }
    let attachment = ReceiverSourceAttachment {
        owner: owner.clone(),
        binding: snapshot.binding,
    };
    let hash = crate::auth::hash_token(&route.session_id);
    let Some(grant) = store
        .receiver_delivery(&authority, &attachment, &hash)
        .await?
    else {
        return Ok(None);
    };
    let b = &attachment.binding;
    let fingerprint = serde_json::to_vec(&serde_json::json!({
        "recipe": route.recipe_json, "reference":b.reference, "file":b.file_id,
        "revision":b.file_revision, "request":b.source_request_id, "session":b.source_session_id,
        "incarnation":b.source_incarnation_id,
    }))
    .map_err(|_| invalid())?;
    Ok(Some(
        crate::sharing_receiver_delivery::ReceiverRelayReadAuthority {
            owner,
            deadline_ms: grant.deadline_ms,
            binding_fingerprint: sha2::Sha256::digest(fingerprint).into(),
            authority,
            attachment,
        },
    ))
}
