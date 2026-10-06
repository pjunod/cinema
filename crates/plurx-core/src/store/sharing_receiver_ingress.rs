//! Guarded B outer-writer metadata. No SQL result constructs physical closure.
use super::{
    sharing::{Backend, Statement},
    sharing_ingress_custody::{self as ledger, IngressCustodySnapshot},
    sharing_receiver_sessions::{
        source_assert, source_current, source_values, source_write_refused, ATTACHED, PUBLISHED,
    },
};
use crate::{
    domain::MediaSessionRoute,
    error::StoreError,
    sharing::invalid,
    sharing_ingress_custody::{CustodyMutation, IngressRegistration},
    sharing_receiver_delivery::{IngressCustodyMembers, ReceiverRelayReadAuthority},
};
use async_trait::async_trait;
use serde::Deserialize;
use uuid::Uuid;

#[async_trait]
pub trait SharingReceiverIngressStore: Send + Sync {
    async fn register_receiver_ingress(
        &self,
        authority: &ReceiverRelayReadAuthority,
        members: Option<&IngressCustodyMembers>,
        actual_local_boot: Uuid,
        registration: &IngressRegistration,
    ) -> Result<CustodyMutation, StoreError>;
    /// Cleanup permission survives grant revoke, expiry and seal. It remains
    /// bound to the exact current route fence and original immutable ledger.
    async fn receiver_ingress_snapshot(
        &self,
        route: &MediaSessionRoute,
    ) -> Result<Option<IngressCustodySnapshot>, StoreError>;
    async fn seal_receiver_ingress(
        &self,
        route: &MediaSessionRoute,
        snapshot: &IngressCustodySnapshot,
    ) -> Result<CustodyMutation, StoreError>;
    /// Daemon supplies only an actually joined local or authenticated exact
    /// peer receipt. This metadata method cannot validate physical custody.
    async fn acknowledge_receiver_ingress(
        &self,
        route: &MediaSessionRoute,
        snapshot: &IngressCustodySnapshot,
        registration: &IngressRegistration,
        confirmation: &str,
    ) -> Result<CustodyMutation, StoreError>;
    /// Exact terminal retirement is retained as the replay fence after safe
    /// adjunct reclamation; missing metadata never authorizes registration.
    async fn reclaim_receiver_ingress(
        &self,
        route: &MediaSessionRoute,
        snapshot: &IngressCustodySnapshot,
    ) -> Result<CustodyMutation, StoreError>;
}
fn route_guard(route: &MediaSessionRoute) -> Result<Statement, StoreError> {
    let Some(user) = route.principal.local_user_id() else {
        return Err(invalid());
    };
    let identity = serde_json::to_string(&serde_json::json!({"inc":route.incarnation_id,"session":route.session_id,"user":user,"node":route.owner_node_id,"epoch":route.owner_epoch,"recipe":route.recipe_json,"fingerprint":route.request_fingerprint,"playback":route.playback_id,"position":route.media_origin_ms})).map_err(|_|invalid())?;
    let predicate = "EXISTS(SELECT 1 FROM media_sessions s JOIN media_session_requests r ON r.incarnation_id=s.incarnation_id AND r.user_id=s.user_id AND r.owner_node_id=s.owner_node_id AND r.request_fingerprint=s.request_fingerprint AND r.playback_id=s.playback_id WHERE s.incarnation_id=json_extract($1,'$.inc') AND s.session_id=json_extract($1,'$.session') AND s.user_id=json_extract($1,'$.user') AND s.owner_node_id=json_extract($1,'$.node') AND s.owner_epoch=json_extract($1,'$.epoch') AND s.recipe_json=json_extract($1,'$.recipe') AND s.request_fingerprint=json_extract($1,'$.fingerprint') AND s.playback_id=json_extract($1,'$.playback') AND s.media_origin_ms=json_extract($1,'$.position') AND s.state IN('active','ended'))";
    Ok(source_assert(
        format!("({predicate}) AND ({})", ledger::schema_guard()),
        vec![identity.into()],
    ))
}
async fn apply<T: Backend>(
    store: &T,
    statements: Vec<Statement>,
    index: usize,
) -> Result<CustodyMutation, StoreError> {
    match store.sharing_txn(statements).await {
        Ok(counts) if counts.get(index) == Some(&1) => Ok(CustodyMutation::Applied),
        Ok(_) => Ok(CustodyMutation::Refused),
        Err(error) if source_write_refused(&error) => Ok(CustodyMutation::Refused),
        Err(error) => Err(error),
    }
}
async fn observed<T: Backend>(
    store: &T,
    route: &MediaSessionRoute,
) -> Result<Option<IngressCustodySnapshot>, StoreError> {
    let guard = route_guard(route)?;
    match store.sharing_txn(vec![guard]).await {
        Ok(_) => {}
        Err(error) if source_write_refused(&error) => return Ok(None),
        Err(error) => return Err(error),
    }
    let rows=store.sharing_read("SELECT json_object('owner',owner_identity) AS payload FROM sharing_ingress_custody WHERE principal_kind='receiver' AND incarnation_id=$1 LIMIT 2",vec![route.incarnation_id.clone().into()]).await?;
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
        owner: String,
    }
    let row: Row = serde_json::from_str(row).map_err(|_| invalid())?;
    ledger::read(
        store,
        "receiver",
        Uuid::parse_str(&route.incarnation_id).map_err(|_| invalid())?,
        &row.owner,
    )
    .await
}
#[async_trait]
impl<T: Backend> SharingReceiverIngressStore for T {
    async fn register_receiver_ingress(
        &self,
        proof: &ReceiverRelayReadAuthority,
        members: Option<&IngressCustodyMembers>,
        actual_local_boot: Uuid,
        registration: &IngressRegistration,
    ) -> Result<CustodyMutation, StoreError> {
        if !registration.valid()
            || registration.closed_confirmation.is_some()
            || actual_local_boot.is_nil()
            || actual_local_boot.get_version_num() != 4
            || self.sharing_is_replicated() != members.is_some()
        {
            return Ok(CustodyMutation::Refused);
        }
        let mut attachment = proof.attachment.clone();
        attachment.owner.now_ms = super::sharing::wall_clock_ms()?;
        let Some(mut values) = source_values(&proof.authority, &attachment)? else {
            return Ok(CustodyMutation::Refused);
        };
        let now = attachment.owner.now_ms;
        values.extend([
            crate::auth::hash_token(&attachment.owner.session_id.to_string()).into(),
            registration.node_id.clone().into(),
            format!("sharing_ingress_boot_v1:{}", registration.boot_id).into(),
        ]);
        let floor = if let Some(members) = members {
            let Ok((guard, roster, cutoff, observed)) = members.write_guard(now, 23, 24, 25) else {
                return Ok(CustodyMutation::Refused);
            };
            values.extend([roster.into(), cutoff.into(), observed.into()]);
            format!("({guard}) AND EXISTS(SELECT 1 FROM cluster_nodes n JOIN cluster_node_capabilities c ON c.node_id=n.node_id AND c.last_seen_at=n.last_seen_at WHERE n.node_id=$21 AND n.removed_at IS NULL AND n.last_seen_at BETWEEN $24 AND $25 AND c.capability=$22)")
        } else {
            if registration.node_id != attachment.owner.owner_node_id
                || registration.boot_id != actual_local_boot
            {
                return Ok(CustodyMutation::Refused);
            };
            // Bind the caller's actual local registry facts even without a
            // replicated member roster. Remote RPC is unavailable standalone.
            "length($21)>0 AND length($22)>0".into()
        };
        let predicate=format!("({}) AND ({}) AND ({floor}) AND EXISTS(SELECT 1 FROM sharing_delivery_grants g WHERE g.token_hash=$20 AND g.incarnation_id=$6 AND g.source_token_hash=$1 AND g.state='active' AND g.deadline_ms>$14)",source_current(ATTACHED,PUBLISHED),ledger::schema_guard());
        let guard = source_assert(predicate, values);
        let inc = proof.owner.incarnation_id;
        let owner = proof.owner_identity();
        match apply(
            self,
            vec![guard.clone(), ledger::create("receiver", inc, &owner)?],
            1,
        )
        .await?
        {
            CustodyMutation::Applied | CustodyMutation::Refused => {}
            CustodyMutation::Replay => {}
        }
        let Some(snapshot) = ledger::read(self, "receiver", inc, &owner).await? else {
            return Ok(CustodyMutation::Refused);
        };
        let mut next = snapshot.state.clone();
        let mutation = next.register(registration.clone());
        if mutation == CustodyMutation::Refused {
            return Ok(mutation);
        };
        let result = apply(
            self,
            vec![guard, ledger::compare_and_swap(&snapshot, &next)?],
            1,
        )
        .await?;
        Ok(if result == CustodyMutation::Applied {
            mutation
        } else {
            result
        })
    }
    async fn receiver_ingress_snapshot(
        &self,
        route: &MediaSessionRoute,
    ) -> Result<Option<IngressCustodySnapshot>, StoreError> {
        observed(self, route).await
    }
    async fn seal_receiver_ingress(
        &self,
        route: &MediaSessionRoute,
        snapshot: &IngressCustodySnapshot,
    ) -> Result<CustodyMutation, StoreError> {
        if snapshot.principal_kind != "receiver"
            || snapshot.incarnation_id.to_string() != route.incarnation_id
        {
            return Ok(CustodyMutation::Refused);
        };
        let mut next = snapshot.state.clone();
        let mutation = next.seal();
        let result = apply(
            self,
            vec![
                route_guard(route)?,
                ledger::compare_and_swap(snapshot, &next)?,
            ],
            1,
        )
        .await?;
        Ok(if result == CustodyMutation::Applied {
            mutation
        } else {
            result
        })
    }
    async fn acknowledge_receiver_ingress(
        &self,
        route: &MediaSessionRoute,
        snapshot: &IngressCustodySnapshot,
        registration: &IngressRegistration,
        confirmation: &str,
    ) -> Result<CustodyMutation, StoreError> {
        if snapshot.principal_kind != "receiver"
            || snapshot.incarnation_id.to_string() != route.incarnation_id
        {
            return Ok(CustodyMutation::Refused);
        };
        let mut next = snapshot.state.clone();
        let mutation = next.acknowledge(registration, confirmation);
        if mutation == CustodyMutation::Refused {
            return Ok(mutation);
        };
        let result = apply(
            self,
            vec![
                route_guard(route)?,
                ledger::compare_and_swap(snapshot, &next)?,
            ],
            1,
        )
        .await?;
        Ok(if result == CustodyMutation::Applied {
            mutation
        } else {
            result
        })
    }
    async fn reclaim_receiver_ingress(
        &self,
        route: &MediaSessionRoute,
        snapshot: &IngressCustodySnapshot,
    ) -> Result<CustodyMutation, StoreError> {
        if route.state != "ended"
            || snapshot.principal_kind != "receiver"
            || snapshot.incarnation_id.to_string() != route.incarnation_id
            || !snapshot.state.settled()
        {
            return Ok(CustodyMutation::Refused);
        };
        let guard = route_guard(route)?;
        let terminal=("INSERT INTO sharing_relay_upstream(incarnation_id,import_id,lifecycle_generation,assignment_generation,remote_library_id,remote_item_id,remote_file_id,remote_revision,source_request_id,endpoint_revision,source_position_ms) SELECT json_extract('receiver_source_authority_refused','$'),'',1,1,'0','0','0','','',1,0 WHERE NOT EXISTS(SELECT 1 FROM media_sessions WHERE incarnation_id=$1 AND state='ended' AND terminal_reason IS NOT NULL AND publication_ready_at_ms=0) OR EXISTS(SELECT 1 FROM sharing_relay_upstream WHERE incarnation_id=$1) OR EXISTS(SELECT 1 FROM job_leases WHERE resource='session:'||$1)".into(),vec![route.incarnation_id.clone().into()]);
        let delete=("DELETE FROM sharing_ingress_custody WHERE principal_kind='receiver' AND incarnation_id=$1 AND owner_identity=$2 AND revision=$3 AND custody_json=$4".into(),vec![snapshot.incarnation_id.into(),snapshot.owner_identity.clone().into(),snapshot.revision.into(),snapshot.state.encode()?.into()]);
        apply(self, vec![guard, terminal, delete], 2).await
    }
}
