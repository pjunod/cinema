//! Source-owned adjunct accounting. These writes never mint driver closure;
//! the retained physical owner authenticates exact receipts before calling Ack.
use super::sharing::{Backend, Statement};
use super::sharing_ingress_custody::{self as ledger, IngressCustodySnapshot};
use crate::{
    error::StoreError,
    sharing::invalid,
    sharing_ingress_custody::{CustodyMutation, IngressRegistration},
    sharing_source_sessions::{
        SourceAdmissionMembers, SourceDispatchAssignment, SourceIngressAdmissionPermission,
    },
};
use async_trait::async_trait;
use uuid::Uuid;

pub(crate) async fn routing<T: Backend>(
    store: &T,
    binding: &crate::sharing_source_sessions::SourceBindingHandle,
    owner: &str,
) -> Result<Option<crate::sharing_ingress_custody::SourceIngressRouting>, StoreError> {
    let hash = crate::sharing_source_sessions::binding_custody_identity(binding, owner, 1);
    Ok(
        ledger::read(store, "source", binding.incarnation_id(), &hash)
            .await?
            .and_then(|row| row.state.source_routing().cloned())
            .filter(|route| route.owner_node_id == owner),
    )
}
/// Untrusted durable routing observation. It cannot be converted to factory,
/// admission, non-admission or physical closure authority.
#[derive(Clone, serde::Deserialize)]
pub struct SourceIngressRoute {
    pub grant_id: Uuid,
    pub viewer_key: String,
    pub incarnation_id: Uuid,
    pub request_fingerprint: String,
    pub playback_id: String,
    pub dispatch_generation: i64,
    pub owner_identity: String,
    pub owner_node_id: String,
    pub registry_boot_id: Uuid,
    pub sealed: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceCustodyWrite {
    Applied,
    ExactReplay,
    /// Metadata fence after an independently verified actual closure receipt.
    ReconciledClosed,
    Refused,
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
fn source_guard(assignment: &SourceDispatchAssignment) -> String {
    let b = assignment.binding();
    let exact = format!(
        "b.incarnation_id={} AND b.owner_key={} AND b.request_id={} AND b.request_fingerprint={} AND b.playback_id={} AND b.source_server_id={} AND b.catalogue_epoch={} AND b.library_id={} AND b.item_id={} AND b.file_id={} AND b.file_revision={} AND b.reservation_state='held' AND b.dispatch_generation=1",
        quote(&b.incarnation_id().to_string()),
        quote(&b.principal().owner_key()),
        quote(b.request_id()),
        quote(b.request_fingerprint()),
        quote(b.playback_id()),
        quote(&b.source_server_id().to_string()),
        quote(&b.catalogue_epoch().to_string()),
        quote(b.library_id().as_str()),
        quote(b.item_id().as_str()),
        quote(b.file_id().as_str()),
        quote(b.file_revision().as_str())
    );
    format!(
        "({}) AND EXISTS(SELECT 1 FROM sharing_source_session_bindings b JOIN media_session_requests r ON r.incarnation_id=b.incarnation_id AND r.owner_key=b.owner_key AND r.request_id=b.request_id AND r.request_fingerprint=b.request_fingerprint AND r.playback_id=b.playback_id AND r.principal_kind='sharing' AND r.user_id IS NULL AND r.share_grant_id=b.share_grant_id AND r.share_viewer_key=b.share_viewer_key JOIN sharing_identity s ON s.singleton=1 AND s.server_id=b.source_server_id AND s.catalogue_epoch=b.catalogue_epoch WHERE {exact} AND r.owner_node_id={} AND r.state IN('starting','resolved','failed'))",
        super::sharing_source_schema::installed_guard(),
        quote(assignment.owner_node_id())
    )
}
fn assertion(condition: &str) -> Statement {
    (
        format!(
            "INSERT INTO sharing_source_session_bindings(incarnation_id) SELECT NULL WHERE NOT({condition})"
        ),
        vec![],
    )
}
fn snapshot_guard(snapshot: &IngressCustodySnapshot) -> Result<String, StoreError> {
    Ok(format!(
        "EXISTS(SELECT 1 FROM sharing_ingress_custody WHERE principal_kind='source' AND incarnation_id={} AND owner_identity={} AND revision={} AND custody_json={})",
        quote(&snapshot.incarnation_id.to_string()),
        quote(&snapshot.owner_identity),
        snapshot.revision,
        quote(&snapshot.state.encode()?)
    ))
}
async fn snapshot<T: Backend>(
    store: &T,
    assignment: &SourceDispatchAssignment,
) -> Result<Option<IngressCustodySnapshot>, StoreError> {
    ledger::read(
        store,
        "source",
        assignment.binding().incarnation_id(),
        &assignment.custody_identity(),
    )
    .await
}
async fn initialized<T: Backend>(
    store: &T,
    assignment: &SourceDispatchAssignment,
) -> Result<SourceCustodyWrite, StoreError> {
    if assignment.dispatch_generation() != 1 {
        return Err(invalid());
    }
    let Some(snapshot) = snapshot(store, assignment).await? else {
        return Ok(SourceCustodyWrite::Refused);
    };
    let Some(routing) = snapshot.state.source_routing() else {
        return Ok(SourceCustodyWrite::Refused);
    };
    if routing.owner_node_id != assignment.owner_node_id() {
        return Ok(SourceCustodyWrite::Refused);
    }
    let counts = store
        .sharing_txn(vec![assertion(&format!(
            "{} AND {}",
            source_guard(assignment),
            snapshot_guard(&snapshot)?
        ))])
        .await?;
    if counts.as_slice() == [0] {
        Ok(SourceCustodyWrite::ExactReplay)
    } else {
        Err(invalid())
    }
}
async fn replace<T: Backend>(
    store: &T,
    assignment: &SourceDispatchAssignment,
    snapshot: &IngressCustodySnapshot,
    next: &crate::sharing_ingress_custody::IngressCustodyState,
    extra: &str,
    values: Vec<super::sharing::Value>,
) -> Result<SourceCustodyWrite, StoreError> {
    let guard = format!(
        "({}) AND ({}) AND ({extra})",
        source_guard(assignment),
        snapshot_guard(snapshot)?
    );
    let mut check = assertion(&guard);
    check.1 = values;
    let counts = store
        .sharing_txn(vec![check, ledger::compare_and_swap(snapshot, next)?])
        .await?;
    if counts.as_slice() == [0, 1] {
        Ok(SourceCustodyWrite::Applied)
    } else {
        Err(invalid())
    }
}
/// Accounting-only replay after terminal Source release. The caller owns an
/// exact actual closure receipt; this guard cannot issue publication/admission.
async fn replace_closed<T: Backend>(
    store: &T,
    assignment: &SourceDispatchAssignment,
    snapshot: &IngressCustodySnapshot,
    next: &crate::sharing_ingress_custody::IngressCustodyState,
) -> Result<SourceCustodyWrite, StoreError> {
    let source = source_guard(assignment).replace(
        "b.reservation_state='held'",
        "b.reservation_state IN('held','released')",
    );
    let released=format!("NOT EXISTS(SELECT 1 FROM sharing_source_session_bindings b WHERE b.incarnation_id={} AND b.reservation_state='released') OR ({})",quote(&assignment.binding().incarnation_id().to_string()),if snapshot.state.is_sealed() && snapshot.state.settled(){"1"}else{"0"});
    let guard = format!(
        "({source}) AND ({released}) AND ({})",
        snapshot_guard(snapshot)?
    );
    let counts = store
        .sharing_txn(vec![
            assertion(&guard),
            ledger::compare_and_swap(snapshot, next)?,
        ])
        .await?;
    if counts.as_slice() == [0, 1] {
        Ok(SourceCustodyWrite::Applied)
    } else {
        Err(invalid())
    }
}
fn admission_guard(
    assignment: &SourceDispatchAssignment,
    boot: Uuid,
    members: &SourceAdmissionMembers,
) -> Result<(String, Vec<super::sharing::Value>), StoreError> {
    if boot.is_nil() || boot.get_version_num() != 4 {
        return Err(invalid());
    }
    let now = super::sharing::wall_clock_ms()?;
    let (guard, roster, cutoff, observed) =
        members.write_guard(now, 1, 2, 3).map_err(|_| invalid())?;
    #[cfg(feature = "hiqlite-store")]
    let floor = crate::cluster::membership::sharing_member_guard_predicate(
        crate::cluster::membership::SharingMemberFloor::IngressCustody,
        1,
        2,
        3,
    );
    #[cfg(not(feature = "hiqlite-store"))]
    let floor = "0".to_owned();
    let boot = format!(
        "EXISTS(SELECT 1 FROM cluster_nodes n JOIN cluster_node_capabilities c ON c.node_id=n.node_id AND c.last_seen_at=n.last_seen_at WHERE n.node_id={} AND n.removed_at IS NULL AND c.capability={})",
        quote(assignment.owner_node_id()),
        quote(&format!("sharing_ingress_boot_v1:{boot}"))
    );
    Ok((
        format!("{guard} AND {floor} AND {boot}"),
        vec![roster.into(), cutoff.into(), observed.into()],
    ))
}
async fn ingress_admission<T: Backend>(
    store: &T,
    assignment: &SourceDispatchAssignment,
    boot: Uuid,
    members: &SourceAdmissionMembers,
    fresh_factory: bool,
) -> Result<Option<Box<SourceIngressAdmissionPermission>>, StoreError> {
    let Some(snapshot) = snapshot(store, assignment).await? else {
        return Ok(None);
    };
    let Some(routing) = snapshot.state.source_routing() else {
        return Ok(None);
    };
    if snapshot.state.sealed
        || routing.owner_node_id != assignment.owner_node_id()
        || routing.registry_boot_id != boot
        || (fresh_factory && snapshot.state.open().next().is_none())
    {
        return Ok(None);
    }
    let (floor, values) = admission_guard(assignment, boot, members)?;
    let mut check = assertion(&format!(
        "{} AND {} AND ({floor})",
        source_guard(assignment),
        snapshot_guard(&snapshot)?
    ));
    check.1 = values;
    let counts = store.sharing_txn(vec![check]).await?;
    if counts.as_slice() != [0] {
        return Err(invalid());
    }
    Ok(Some(Box::new(SourceIngressAdmissionPermission {
        assignment: assignment.clone(),
        registry_boot_id: boot,
        members: members.clone(),
    })))
}
#[async_trait]
pub trait SharingSourceIngressCustodyStore: Send + Sync {
    // Keep every exact reference field explicit; this lookup grants no authority.
    #[allow(clippy::too_many_arguments)]
    async fn lookup_source_ingress_route(
        &self,
        credential_hash: &str,
        viewer_key: &str,
        request_id: &str,
        source_server: Uuid,
        catalogue_epoch: Uuid,
        library_id: &str,
        item_id: &str,
        file_id: &str,
        file_revision: &str,
    ) -> Result<Option<SourceIngressRoute>, StoreError>;
    /// Refresh only a retained actual factory-issued permission; a connection gap
    /// is ordinary lifetime behavior and never revokes the admitted producer.
    async fn refresh_source_ingress_admission(
        &self,
        retained: &SourceIngressAdmissionPermission,
        members: &SourceAdmissionMembers,
    ) -> Result<Option<Box<SourceIngressAdmissionPermission>>, StoreError>;
    async fn prepare_source_ingress_admission(
        &self,
        assignment: &SourceDispatchAssignment,
        registry_boot: Uuid,
        members: &SourceAdmissionMembers,
    ) -> Result<Option<Box<SourceIngressAdmissionPermission>>, StoreError>;
    async fn initialize_source_ingress_custody(
        &self,
        assignment: &SourceDispatchAssignment,
    ) -> Result<SourceCustodyWrite, StoreError>;
    async fn source_ingress_custody(
        &self,
        assignment: &SourceDispatchAssignment,
    ) -> Result<Option<IngressCustodySnapshot>, StoreError>;
    async fn register_source_ingress_custody(
        &self,
        assignment: &SourceDispatchAssignment,
        registration: &IngressRegistration,
        members: &SourceAdmissionMembers,
    ) -> Result<SourceCustodyWrite, StoreError>;
    async fn seal_source_ingress_custody(
        &self,
        assignment: &SourceDispatchAssignment,
    ) -> Result<SourceCustodyWrite, StoreError>;
    async fn acknowledge_source_ingress_custody(
        &self,
        assignment: &SourceDispatchAssignment,
        registration: &IngressRegistration,
        confirmation: &str,
    ) -> Result<SourceCustodyWrite, StoreError>;
    async fn current_source_ingress_owner(
        &self,
        incarnation: Uuid,
        owner_identity: &str,
        caller_node: &str,
        dispatch_generation: i64,
    ) -> Result<bool, StoreError>;
}
#[async_trait]
impl<T: Backend> SharingSourceIngressCustodyStore for T {
    async fn lookup_source_ingress_route(
        &self,
        credential_hash: &str,
        viewer_key: &str,
        request_id: &str,
        source_server: Uuid,
        catalogue_epoch: Uuid,
        library_id: &str,
        item_id: &str,
        file_id: &str,
        file_revision: &str,
    ) -> Result<Option<SourceIngressRoute>, StoreError> {
        if !crate::sharing::is_hash(credential_hash) || !crate::sharing::is_hash(viewer_key) {
            return Err(invalid());
        }
        let rows=self.sharing_read("SELECT json_object('grant_id',b.share_grant_id,'viewer_key',b.share_viewer_key,'incarnation_id',b.incarnation_id,'request_fingerprint',b.request_fingerprint,'playback_id',b.playback_id,'dispatch_generation',b.dispatch_generation,'owner_identity',c.owner_identity,'owner_node_id',json_extract(c.custody_json,'$.source_routing.owner_node_id'),'registry_boot_id',json_extract(c.custody_json,'$.source_routing.registry_boot_id'),'sealed',json(CASE WHEN json_extract(c.custody_json,'$.sealed')=1 THEN 'true' ELSE 'false' END)) AS payload FROM sharing_source_session_bindings b JOIN sharing_ingress_custody c ON c.principal_kind='source' AND c.incarnation_id=b.incarnation_id WHERE b.share_viewer_key=$2 AND b.request_id=$3 AND b.source_server_id=$4 AND b.catalogue_epoch=$5 AND b.library_id=$6 AND b.item_id=$7 AND b.file_id=$8 AND b.file_revision=$9 AND (json_extract(c.custody_json,'$.source_routing.initial_credential_hash')=$1 OR EXISTS(SELECT 1 FROM sharing_exports e WHERE e.id=b.share_grant_id AND e.token_hash=$1)) LIMIT 2",vec![credential_hash.to_owned().into(),viewer_key.to_owned().into(),request_id.to_owned().into(),source_server.into(),catalogue_epoch.into(),library_id.to_owned().into(),item_id.to_owned().into(),file_id.to_owned().into(),file_revision.to_owned().into()]).await?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => serde_json::from_str(row).map(Some).map_err(|_| invalid()),
            _ => Ok(None),
        }
    }
    async fn refresh_source_ingress_admission(
        &self,
        retained: &SourceIngressAdmissionPermission,
        members: &SourceAdmissionMembers,
    ) -> Result<Option<Box<SourceIngressAdmissionPermission>>, StoreError> {
        ingress_admission(
            self,
            retained.assignment(),
            retained.registry_boot_id(),
            members,
            false,
        )
        .await
    }
    async fn prepare_source_ingress_admission(
        &self,
        assignment: &SourceDispatchAssignment,
        registry_boot: Uuid,
        members: &SourceAdmissionMembers,
    ) -> Result<Option<Box<SourceIngressAdmissionPermission>>, StoreError> {
        ingress_admission(self, assignment, registry_boot, members, true).await
    }
    async fn initialize_source_ingress_custody(
        &self,
        assignment: &SourceDispatchAssignment,
    ) -> Result<SourceCustodyWrite, StoreError> {
        initialized(self, assignment).await
    }
    async fn source_ingress_custody(
        &self,
        assignment: &SourceDispatchAssignment,
    ) -> Result<Option<IngressCustodySnapshot>, StoreError> {
        snapshot(self, assignment).await
    }
    async fn register_source_ingress_custody(
        &self,
        assignment: &SourceDispatchAssignment,
        registration: &IngressRegistration,
        fresh_members: &SourceAdmissionMembers,
    ) -> Result<SourceCustodyWrite, StoreError> {
        if !registration.valid() || registration.closed_confirmation.is_some() {
            return Err(invalid());
        }
        let Some(snapshot) = snapshot(self, assignment).await? else {
            return Ok(SourceCustodyWrite::Refused);
        };
        let mut next = snapshot.state.clone();
        match next.register(registration.clone()) {
            CustodyMutation::Refused => Ok(SourceCustodyWrite::Refused),
            CustodyMutation::Replay => Ok(SourceCustodyWrite::ExactReplay),
            CustodyMutation::Applied => {
                let now = chrono::Utc::now().timestamp_millis();
                let (members, roster, cutoff, observed) = fresh_members
                    .write_guard(now, 1, 2, 3)
                    .map_err(|_| invalid())?;
                #[cfg(feature = "hiqlite-store")]
                let floor = crate::cluster::membership::sharing_member_guard_predicate(
                    crate::cluster::membership::SharingMemberFloor::IngressCustody,
                    1,
                    2,
                    3,
                );
                #[cfg(not(feature = "hiqlite-store"))]
                let floor = "0".to_owned();
                let boot = format!(
                    "EXISTS(SELECT 1 FROM cluster_nodes n JOIN cluster_node_capabilities c ON c.node_id=n.node_id AND c.last_seen_at=n.last_seen_at WHERE n.node_id={} AND n.removed_at IS NULL AND c.capability={})",
                    quote(&registration.node_id),
                    quote(&format!("sharing_ingress_boot_v1:{}", registration.boot_id))
                );
                replace(
                    self,
                    assignment,
                    &snapshot,
                    &next,
                    &format!("{members} AND {floor} AND {boot}"),
                    vec![roster.into(), cutoff.into(), observed.into()],
                )
                .await
            }
        }
    }
    async fn seal_source_ingress_custody(
        &self,
        assignment: &SourceDispatchAssignment,
    ) -> Result<SourceCustodyWrite, StoreError> {
        let Some(snapshot) = snapshot(self, assignment).await? else {
            return Ok(SourceCustodyWrite::Refused);
        };
        let mut next = snapshot.state.clone();
        if next.seal() == CustodyMutation::Replay {
            return Ok(SourceCustodyWrite::ExactReplay);
        }
        replace(self, assignment, &snapshot, &next, "1", vec![]).await
    }
    async fn acknowledge_source_ingress_custody(
        &self,
        assignment: &SourceDispatchAssignment,
        registration: &IngressRegistration,
        confirmation: &str,
    ) -> Result<SourceCustodyWrite, StoreError> {
        let Some(snapshot) = snapshot(self, assignment).await? else {
            return Ok(SourceCustodyWrite::Refused);
        };
        let mut next = snapshot.state.clone();
        let outcome = next.acknowledge(registration, confirmation);
        if outcome == CustodyMutation::Refused {
            if next.fence_closed_registration(registration, confirmation)
                == CustodyMutation::Refused
            {
                return Ok(SourceCustodyWrite::Refused);
            }
            // Same-write owner/snapshot CAS installs or confirms a fence before
            // any delayed Register can admit this closed ordinal.
            return match replace_closed(self, assignment, &snapshot, &next).await? {
                SourceCustodyWrite::Applied => Ok(SourceCustodyWrite::ReconciledClosed),
                _ => Err(invalid()),
            };
        }
        match outcome {
            CustodyMutation::Applied => replace_closed(self, assignment, &snapshot, &next).await,
            CustodyMutation::Replay => {
                match replace_closed(self, assignment, &snapshot, &next).await? {
                    SourceCustodyWrite::Applied => Ok(SourceCustodyWrite::ExactReplay),
                    _ => Err(invalid()),
                }
            }
            CustodyMutation::Refused => Ok(SourceCustodyWrite::Refused),
        }
    }
    async fn current_source_ingress_owner(
        &self,
        incarnation: Uuid,
        owner_identity: &str,
        caller_node: &str,
        dispatch_generation: i64,
    ) -> Result<bool, StoreError> {
        if incarnation.is_nil()
            || !crate::sharing::is_hash(owner_identity)
            || caller_node.is_empty()
            || dispatch_generation != 1
        {
            return Ok(false);
        }
        let sql = format!(
            "SELECT json_quote(count(*)) AS payload FROM sharing_source_session_bindings b JOIN media_session_requests r ON r.incarnation_id=b.incarnation_id AND r.owner_key=b.owner_key AND r.request_id=b.request_id AND r.request_fingerprint=b.request_fingerprint AND r.playback_id=b.playback_id AND r.principal_kind='sharing' AND r.user_id IS NULL AND r.share_grant_id=b.share_grant_id AND r.share_viewer_key=b.share_viewer_key JOIN sharing_ingress_custody c ON c.principal_kind='source' AND c.incarnation_id=b.incarnation_id JOIN sharing_identity s ON s.singleton=1 AND s.server_id=b.source_server_id AND s.catalogue_epoch=b.catalogue_epoch WHERE b.incarnation_id=$1 AND b.dispatch_generation=1 AND b.reservation_state='held' AND r.owner_node_id=$2 AND c.owner_identity=$3 AND ({})",
            super::sharing_source_schema::installed_guard()
        );
        Ok(self
            .sharing_read(
                &sql,
                vec![
                    incarnation.into(),
                    caller_node.to_owned().into(),
                    owner_identity.to_owned().into(),
                ],
            )
            .await?
            .as_slice()
            == ["1"])
    }
}
