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
    async fn receiver_cleanup_capsules(
        &self,
        route: &MediaSessionRoute,
        owner: &crate::sharing_receiver_sessions::ReceiverSourceOwner,
        binding: Option<&crate::sharing_receiver_sessions::ReceiverSourceBinding>,
        intent: &crate::sharing_receiver_sessions::ReceiverSessionIntent,
    ) -> Result<Option<super::ReceiverCleanupCapsules>, StoreError>;
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
    /// Install a sealed fence even when no ingress has registered yet. A
    /// retirement owner must complete this before it may send Source End;
    /// absent metadata cannot race an already-authorized late registration.
    async fn seal_receiver_ingress_route(
        &self,
        route: &MediaSessionRoute,
        original_owner_identity: &str,
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
pub(super) fn route_guard(route: &MediaSessionRoute) -> Result<Statement, StoreError> {
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
    let result = store.sharing_txn(statements).await;
    #[cfg(feature = "fixtures")]
    match &result {
        Ok(counts) => {
            eprintln!("B ingress Core actual transaction counts={counts:?} mutation_index={index}")
        }
        Err(error) => eprintln!(
            "B ingress Core actual transaction StoreError={error:?} mutation_index={index}"
        ),
    }
    match result {
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
    async fn receiver_cleanup_capsules(
        &self,
        route: &MediaSessionRoute,
        owner: &crate::sharing_receiver_sessions::ReceiverSourceOwner,
        binding: Option<&crate::sharing_receiver_sessions::ReceiverSourceBinding>,
        intent: &crate::sharing_receiver_sessions::ReceiverSessionIntent,
    ) -> Result<Option<super::ReceiverCleanupCapsules>, StoreError> {
        super::sharing_receiver_capsule_refresh::cleanup_capsules(
            self, route, owner, binding, intent,
        )
        .await
    }

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
            #[cfg(feature = "fixtures")]
            eprintln!("B ingress Core Register stage=validation refused valid={} closed={} boot_nil={} boot_v4={} replicated={} members={}", registration.valid(), registration.closed_confirmation.is_some(), actual_local_boot.is_nil(), actual_local_boot.get_version_num()==4, self.sharing_is_replicated(), members.is_some());
            return Ok(CustodyMutation::Refused);
        }
        let mut attachment = proof.attachment.clone();
        attachment.owner.now_ms = super::sharing::wall_clock_ms()?;
        let Some(mut values) = source_values(&proof.authority, &attachment)? else {
            #[cfg(feature = "fixtures")]
            eprintln!("B ingress Core Register stage=source_values refused");
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
                #[cfg(feature = "fixtures")]
                eprintln!("B ingress Core Register stage=member_write_guard refused");
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
        let inc = proof.attachment.owner.incarnation_id;
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
            #[cfg(feature = "fixtures")]
            eprintln!("B ingress Core Register stage=ledger_read absent after guarded create");
            return Ok(CustodyMutation::Refused);
        };
        let mut next = snapshot.state.clone();
        let mutation = next.register(registration.clone());
        if mutation == CustodyMutation::Refused {
            #[cfg(feature = "fixtures")]
            eprintln!("B ingress Core Register stage=state_register refused sealed={} open_slots={} physical_sequence={} principal_sequence={}", snapshot.state.is_sealed(), snapshot.state.open().count(), registration.driver_sequence, registration.registration_sequence);
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
    async fn seal_receiver_ingress_route(
        &self,
        route: &MediaSessionRoute,
        original_owner_identity: &str,
    ) -> Result<Option<IngressCustodySnapshot>, StoreError> {
        let inc = Uuid::parse_str(&route.incarnation_id).map_err(|_| invalid())?;
        let mut sealed = crate::sharing_ingress_custody::IngressCustodyState::default();
        sealed.seal();
        let mut create = ledger::create("receiver", inc, original_owner_identity)?;
        create.1[3] = sealed.encode()?.into();
        // A missing row is born sealed in this exact route-fenced write.
        // Existing rows retain their immutable original principal identity.
        match self.sharing_txn(vec![route_guard(route)?, create]).await {
            Ok(_) => {}
            Err(error) if source_write_refused(&error) => return Ok(None),
            Err(error) => return Err(error),
        }
        let Some(snapshot) = observed(self, route).await? else {
            return Ok(None);
        };
        if snapshot.owner_identity != original_owner_identity {
            return Ok(None);
        }
        let mut next = snapshot.state.clone();
        next.seal();
        if apply(
            self,
            vec![
                route_guard(route)?,
                ledger::compare_and_swap(&snapshot, &next)?,
            ],
            1,
        )
        .await?
            != CustodyMutation::Applied
        {
            return Ok(None);
        }
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
            // Caller already owns actual joined or authenticated exact close.
            // This same-write metadata fence handles ACK-before-register and
            // lost ACK after reclamation; it constructs no physical receipt.
            if snapshot.state.contains_driver(registration)
                || next.fence_closed_registration(registration, confirmation)
                    == CustodyMutation::Refused
            {
                return Ok(mutation);
            }
            let result = apply(
                self,
                vec![
                    route_guard(route)?,
                    ledger::compare_and_swap(snapshot, &next)?,
                ],
                1,
            )
            .await?;
            return Ok(if result == CustodyMutation::Applied {
                CustodyMutation::Replay
            } else {
                result
            });
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
        let mut next = snapshot.state.clone();
        next.compact_settled();
        // Preserve a small exact principal terminal fence for ingress release
        // reconciliation; missing metadata never constructs an acknowledgement.
        apply(
            self,
            vec![guard, terminal, ledger::compare_and_swap(snapshot, &next)?],
            2,
        )
        .await
    }
}
