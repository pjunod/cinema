//! Versioned close/transfer AUDIT records, never physical capabilities.
use super::*;

/// Disjoint cancellation audit: never label an uninvoked source/task as an
/// actually joined/closed owner. No physical capability is serialized here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NeverArmed {
    pub version: u32,
    pub kind: NeverArmedKind,
    pub owner_id: Id,
    pub graph_id: Id,
    pub operation_id: Id,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NeverArmedKind {
    PreparedNeverArmed,
}
/// Audit of one daemon-only direct domain's extinction. The private native
/// exit capability is consumed by the dedicated journal command; this record
/// can never authorize a current-runtime close or accounting refund.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectDaemonExit {
    pub version: u32,
    pub kind: DirectDaemonExitKind,
    pub owner_id: Id,
    pub graph_id: Id,
    pub operation_id: Id,
    pub closing_owner_id: Id,
    pub prior_operation_id: Id,
    pub prior_sequence: i64,
    pub prior_phase: crate::resource_custody::Phase,
    pub member_id: Id,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DirectDaemonExitKind {
    DirectDaemonDomainExtinctV1,
}

/// Only these immutable plans own exclusively daemon-local descriptors and
/// threads. Other graphs can have surviving children and require their own
/// positive native closure, even when the old daemon exited.
pub(crate) fn direct_daemon_scope(manifest: &Manifest) -> Result<Option<Id>, CustodyError> {
    validate_manifest(manifest)?;
    match manifest.members.as_slice() {
        [MemberPlan::DirectSourceV1 { source, slot }]
            if slot.owned_by.is_none() && slot.depends_on.is_empty() =>
        {
            // validate_manifest checks the sole DaemonSourceCell copy, its
            // original owner/graph/member, and absence of inherited copies.
            Ok(Some(source.read_domain_obligation_id))
        }
        [MemberPlan::DirectReadDomainV1 { .. }] => Ok(None),
        _ => Err(codec::invalid()),
    }
}

pub(crate) fn validate_direct_daemon_exit(
    manifest: &Manifest,
    record: &DirectDaemonExit,
) -> Result<(), CustodyError> {
    direct_daemon_scope(manifest)?;
    if record.version != 2
        || record.owner_id != manifest.owner_id
        || record.graph_id != manifest.graph_id
        || record.closing_owner_id == record.owner_id
        || record.prior_sequence <= 0
        || manifest.members[0].member_id() != record.member_id.uuid()
    {
        return Err(codec::invalid());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SettlementDocument {
    Physical(Settlement),
    NeverArmed(NeverArmed),
    DirectDaemonExit(DirectDaemonExit),
}
pub(crate) fn decode_settlement(json: &str) -> Result<SettlementDocument, CustodyError> {
    #[derive(Deserialize)]
    struct Discriminator {
        kind: Option<String>,
    }
    match codec::decode::<Discriminator>(json)?.kind.as_deref() {
        None => Ok(SettlementDocument::Physical(codec::decode(json)?)),
        Some("direct_daemon_domain_extinct_v1") => {
            let value: DirectDaemonExit = codec::decode(json)?;
            if value.version != 2 {
                return Err(codec::invalid());
            }
            Ok(SettlementDocument::DirectDaemonExit(value))
        }
        Some("prepared_never_armed") => {
            let value: NeverArmed = codec::decode(json)?;
            if value.version != 2 {
                return Err(codec::invalid());
            }
            Ok(SettlementDocument::NeverArmed(value))
        }
        _ => Err(codec::invalid()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum CopyClosed {
    ActualOwnedClose {
        copy: SourceCopy,
        operation_id: Id,
    },
    ProvenNeverAcquired {
        copy: SourceCopy,
        operation_id: Id,
    },
    DomainExtinct {
        copy: SourceCopy,
        operation_id: Id,
        domain: FilesystemDomain,
    },
}
impl CopyClosed {
    fn copy(&self) -> &SourceCopy {
        match self {
            Self::ActualOwnedClose { copy, .. }
            | Self::ProvenNeverAcquired { copy, .. }
            | Self::DomainExtinct { copy, .. } => copy,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnerClosed {
    pub owner_id: Id,
    pub operation_id: Id,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum FixedEngineOwnerDispositionV1 {
    ActualOwnedClose { owner_id: Id, operation_id: Id },
    ProvenNeverAcquired { owner_id: Id, operation_id: Id },
}
impl FixedEngineOwnerDispositionV1 {
    fn owner_id(&self) -> Id {
        match self {
            Self::ActualOwnedClose { owner_id, .. }
            | Self::ProvenNeverAcquired { owner_id, .. } => *owner_id,
        }
    }
    fn operation_id(&self) -> Id {
        match self {
            Self::ActualOwnedClose { operation_id, .. }
            | Self::ProvenNeverAcquired { operation_id, .. } => *operation_id,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum ClosedFact {
    LegacyV1 {
        fact: super::super::ClosedMemberFact,
    },
    LegacyNeverAcquiredV1 {
        planned: super::super::PlannedMember,
        operation_id: Id,
    },
    SourceV1 {
        holder_id: Id,
        copies: Vec<CopyClosed>,
    },
    DirectSourceV1 {
        holder_id: Id,
        copies: Vec<CopyClosed>,
    },
    FixedProbeDiscoverSourceV1 {
        holder_id: Id,
        copies: Vec<CopyClosed>,
    },
    DirectReadDomainV1 {
        domain_id: Id,
        close_operation_id: Id,
    },
    /// The original registered task's JoinHandle completed and every
    /// inventoried owner closed under join_operation_id. This asserts nothing
    /// about parser entry, output production or success.
    VodOutputTaskV1 {
        task: VodOutputTask,
        join_operation_id: Id,
        owners: Vec<OwnerClosed>,
    },
    FixedProbeTaskV1 {
        task: FixedProbeTaskV1,
        join_operation_id: Id,
        owners: Vec<OwnerClosed>,
    },
    FixedProbeTaskNeverAcquiredV1 {
        task: FixedProbeTaskV1,
        operation_id: Id,
    },
    FixedProbeDiscoverTaskV1 {
        task: FixedProbeDiscoverTaskV1,
        join_operation_id: Id,
        version_close_operation_id: Id,
        document_close_operation_id: Id,
        owners: Vec<OwnerClosed>,
    },
    FixedProbeDiscoverTaskNeverAcquiredV1 {
        task: FixedProbeDiscoverTaskV1,
        operation_id: Id,
    },
    FixedPlanningEngineTaskV1 {
        task: FixedPlanningEngineTaskV1,
        join_operation_id: Id,
        version_close_operation_id: Id,
        dependency_close_operation_id: Id,
        used_file_slots: u16,
        owners: Vec<FixedEngineOwnerDispositionV1>,
    },
    /// Compact binding to the unchanged manifest task and its observation.
    FixedPlanningEngineTaskV2 {
        member_id: Id,
        plan_sha256: String,
        join_operation_id: Id,
        version_close_operation_id: Id,
        dependency_close_operation_id: Id,
        used_file_slots: u16,
        owners: Vec<FixedEngineOwnerDispositionV1>,
    },
    FixedPlanningEngineTaskNeverAcquiredV1 {
        task: FixedPlanningEngineTaskV1,
        operation_id: Id,
    },
    FixedProbeMetadataV1 {
        metadata: FixedProbeMetadataV1,
        release_operation_id: Id,
    },
    /// Same-boot fixed software root and daemon domains were positively
    /// extinct before import. This records old task-owner extinction, not a
    /// fabricated join or an assertion that no task effect occurred.
    VodOutputTaskDomainExtinctV1 {
        task: VodOutputTask,
        root_member_id: Id,
        root_pid: u32,
        root_creation_marker: u64,
        operation_id: Id,
    },
    /// The original graph proved that no writer was registered. This is
    /// disjoint from a joined VodOutputTaskV1 and closes no invented owner.
    VodOutputTaskNeverAcquiredV1 {
        task: VodOutputTask,
        operation_id: Id,
    },
    ScratchAssetsV1 {
        scratch: ScratchAssets,
        operation_id: Id,
        owners: Vec<OwnerClosed>,
        files: Vec<Id>,
    },
    ScratchAssetsV3 {
        plan: super::scratch::bootstrap::ScratchPlanV3,
        exact_head: super::scratch::AssetEventRefV1,
        operation_id: Id,
    },
    ScratchAssetsNeverAcquiredV3 {
        plan: super::scratch::bootstrap::ScratchPlanV3,
        operation_id: Id,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "The bounded frozen inventory stays inline in MemberTransfer and its charged owner layout; do not introduce a new allocation during conserving transfer."
)]
pub(crate) enum TransferIdentity {
    SourceV1 {
        source: SourceObserved,
        retained_copy_ids: Vec<Id>,
        closed_copies: Vec<CopyClosed>,
    },
    ScratchAssetsV1 {
        scratch: ScratchAssets,
    },
    ScratchAssetsV3 {
        plan: scratch::bootstrap::ScratchPlanV3,
        snapshot: scratch::bootstrap::FrozenScratchRefV1,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemberTransfer {
    pub source_member_id: Id,
    pub target_member_id: Id,
    pub identity: TransferIdentity,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "disposition", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "Settlement vector capacity includes the complete inline disposition layout; boxing a transferred proof would add a new allocation during original settlement handoff."
)]
pub(crate) enum MemberDisposition {
    Closed {
        member_id: Id,
        fact: ClosedFact,
    },
    Transferred {
        transfer: MemberTransfer,
        target_obligation_id: Id,
        target_graph_id: Id,
        asset_id: Id,
        operation_id: Id,
    },
}
impl MemberDisposition {
    pub(crate) fn member_id(&self) -> Id {
        match self {
            Self::Closed { member_id, .. } => *member_id,
            Self::Transferred { transfer, .. } => transfer.source_member_id,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settlement {
    pub version: u32,
    pub owner_id: Id,
    pub graph_id: Id,
    pub operation_id: Id,
    pub members: Vec<MemberDisposition>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "state", rename_all = "snake_case")]
pub(crate) enum Reconciliation {
    LiveOwned,
    OldDomainRequired {
        owner_id: Id,
        domain: FilesystemDomain,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedAssets {
    pub version: u32,
    pub asset_id: Id,
    pub rendition_id: String,
    pub generation_epoch: u64,
    pub producer_obligation_id: Id,
    pub producer_graph_id: Id,
    pub transfer_operation_id: Id,
    pub target_obligation_id: Id,
    pub target_graph_id: Id,
    pub target_owner_id: Id,
    pub transfers: Vec<MemberTransfer>,
    pub directory_path: String,
    pub directory_object: NativeObject,
    pub reader_ids: Vec<Id>,
    pub reconciliation: Reconciliation,
}

fn validate_closed_owners(
    owners: &[OwnerClosed],
    inventory: &IoInventory,
) -> Result<(), CustodyError> {
    let expected: BTreeSet<_> = inventory
        .pending_io
        .iter()
        .chain(&inventory.buffers)
        .chain(&inventory.readers)
        .map(|o| o.owner_id)
        .collect();
    let actual: BTreeSet<_> = owners.iter().map(|o| o.owner_id).collect();
    if expected != actual || actual.len() != owners.len() {
        return Err(codec::invalid());
    }
    Ok(())
}
pub(crate) fn validate_settlement(
    manifest: &Manifest,
    envelopes: &[Envelope],
    value: &Settlement,
) -> Result<(), CustodyError> {
    validate_settlement_scope(
        manifest.owner_id,
        manifest.graph_id,
        manifest.members.len(),
        envelopes,
        value,
    )
}

/// The same audit validator over an already retained manifest scope. This
/// avoids cloning a Manifest merely to validate original runtime metadata;
/// callers still validate each envelope against the exact original plan slice.
/// No data validation here constructs a physical settlement or refund.
pub(crate) fn validate_settlement_scope(
    owner_id: Id,
    graph_id: Id,
    member_count: usize,
    envelopes: &[Envelope],
    value: &Settlement,
) -> Result<(), CustodyError> {
    if value.version != 2
        || value.graph_id != graph_id
        || value.owner_id != owner_id
        || value.members.len() != member_count
        || envelopes.len() != member_count
    {
        return Err(codec::invalid());
    }
    let mut seen = BTreeSet::new();
    for disposition in &value.members {
        let id = disposition.member_id();
        if !seen.insert(id) {
            return Err(codec::invalid());
        }
        let envelope = envelopes
            .iter()
            .find(|e| e.planned.member_id() == id.uuid())
            .ok_or_else(codec::invalid)?;
        match disposition {
            MemberDisposition::Closed { fact, .. } => match (&envelope.planned, fact) {
                (MemberPlan::LegacyV1 { planned }, ClosedFact::LegacyV1 { fact }) => {
                    if fact.member_id != planned.member_id
                        || fact.adapter != planned.adapter
                        || fact.adapter_version != planned.adapter_version
                        || fact.domain != planned.domain
                    {
                        return Err(codec::invalid());
                    }
                    codec::long_text(&fact.object_identity)?;
                    codec::long_text(&fact.evidence)?;
                    if let Some(Observed::LegacyV1 { identity }) = &envelope.observed {
                        if identity.object_identity != fact.object_identity {
                            return Err(codec::invalid());
                        }
                    }
                }
                (
                    MemberPlan::LegacyV1 { planned },
                    ClosedFact::LegacyNeverAcquiredV1 {
                        planned: closed,
                        operation_id,
                    },
                ) if planned == closed
                    && envelope.observed.is_none()
                    && !operation_id.uuid().is_nil() => {}
                (
                    MemberPlan::SourceV1 { source, .. },
                    ClosedFact::SourceV1 { holder_id, copies },
                ) => {
                    if *holder_id != source.holder_id || copies.len() != source.planned_copies.len()
                    {
                        return Err(codec::invalid());
                    }
                    let ids: BTreeSet<_> = copies.iter().map(|c| c.copy().copy_id).collect();
                    if ids.len() != copies.len()
                        || copies
                            .iter()
                            .any(|c| !source.planned_copies.contains(c.copy()))
                    {
                        return Err(codec::invalid());
                    }
                    validate_copy_closures(source, &envelope.observed, copies)?;
                }
                (
                    MemberPlan::FixedProbeDiscoverSourceV1 { source, .. },
                    ClosedFact::FixedProbeDiscoverSourceV1 { holder_id, copies },
                ) => {
                    if *holder_id != source.holder_id
                        || copies.len() != source.planned_copies.len()
                        || copies.iter().zip(&source.planned_copies)
                            .any(|(closed, planned)| closed.copy() != planned)
                        || copies.iter().any(|closed| match closed {
                            CopyClosed::DomainExtinct { .. } => true,
                            CopyClosed::ProvenNeverAcquired { copy, .. } =>
                                matches!(&envelope.observed,
                                    Some(Observed::FixedProbeDiscoverSourceV1 { source: observed })
                                        if observed.copies.contains(copy)),
                            CopyClosed::ActualOwnedClose { .. } => false,
                        })
                    {
                        return Err(codec::invalid());
                    }
                }
                (
                    MemberPlan::DirectSourceV1 { source, .. },
                    ClosedFact::DirectSourceV1 { holder_id, copies },
                ) => {
                    if *holder_id != source.holder_id
                        || copies.len() != 1
                        || copies[0].copy() != &source.planned_copies[0]
                    {
                        return Err(codec::invalid());
                    }
                    match &copies[0] {
                        CopyClosed::ProvenNeverAcquired { copy, .. }
                            if matches!(&envelope.observed,
                                Some(Observed::DirectSourceV1 { source: observed })
                                    if observed.copies.contains(copy)) =>
                        {
                            return Err(codec::invalid());
                        }
                        CopyClosed::DomainExtinct { .. } => return Err(codec::invalid()),
                        _ => {}
                    }
                }
                (
                    MemberPlan::DirectReadDomainV1 { domain, .. },
                    ClosedFact::DirectReadDomainV1 { domain_id, close_operation_id },
                ) if *domain_id == domain.domain_id && !close_operation_id.uuid().is_nil() => {}
                (
                    MemberPlan::VodOutputTaskV1 { task, .. },
                    ClosedFact::VodOutputTaskV1 {
                        task: closed,
                        join_operation_id,
                        owners,
                    },
                ) if task == closed
                    && !join_operation_id.uuid().is_nil()
                    && matches!(&envelope.observed,
                        Some(Observed::VodOutputTaskV1 { task: observed }) if observed == task) =>
                {
                    validate_closed_owners(owners, &task.inventory)?
                }
                (
                    MemberPlan::FixedProbeTaskV1 { task, .. },
                    ClosedFact::FixedProbeTaskV1 {
                        task: closed,
                        join_operation_id,
                        owners,
                    },
                ) if task == closed
                    && !join_operation_id.uuid().is_nil()
                    && matches!(&envelope.observed,
                        Some(Observed::FixedProbeTaskV1 { task: observed }) if observed == task) =>
                {
                    validate_closed_owners(owners, &task.inventory)?
                }
                (
                    MemberPlan::FixedProbeTaskV1 { task, .. },
                    ClosedFact::FixedProbeTaskNeverAcquiredV1 { task: closed, operation_id },
                ) if task == closed
                    && envelope.observed.is_none()
                    && !operation_id.uuid().is_nil() => {}
                (
                    MemberPlan::FixedProbeDiscoverTaskV1 { task, .. },
                    ClosedFact::FixedProbeDiscoverTaskV1 {
                        task: closed, join_operation_id, version_close_operation_id,
                        document_close_operation_id, owners,
                    },
                ) if task == closed
                    && !join_operation_id.uuid().is_nil()
                    && *version_close_operation_id == task.version_close_operation_id
                    && *document_close_operation_id == task.document_close_operation_id
                    && matches!(&envelope.observed,
                        Some(Observed::FixedProbeDiscoverTaskV1 { task: observed })
                            if observed.task == *task) =>
                {
                    validate_closed_owners(owners, &task.inventory)?;
                    for (owner_id, operation_id) in [
                        (task.image_owner_id, task.image_close_operation_id),
                        (task.snapshot_owner_id, task.snapshot_close_operation_id),
                        (task.source_child_owner_id, task.source_child_close_operation_id),
                        (task.version_stdout_owner_id, task.version_stdout_close_operation_id),
                        (task.stdout_owner_id, task.stdout_close_operation_id),
                        (task.diagnostic_owner_id, task.diagnostic_close_operation_id),
                    ] {
                        if !owners.iter().any(|owner| owner.owner_id == owner_id
                            && owner.operation_id == operation_id) {
                            return Err(codec::invalid());
                        }
                    }
                }
                (
                    MemberPlan::FixedProbeDiscoverTaskV1 { task, .. },
                    ClosedFact::FixedProbeDiscoverTaskNeverAcquiredV1 { task: closed, operation_id },
                ) if task == closed
                    && envelope.observed.is_none()
                    && !operation_id.uuid().is_nil() => {}
                (
                    MemberPlan::FixedPlanningEngineTaskV1 { task, .. },
                    ClosedFact::FixedPlanningEngineTaskV1 {
                        task: closed, join_operation_id,
                        version_close_operation_id, dependency_close_operation_id,
                        used_file_slots, owners,
                    },
                ) if task == closed
                    && !join_operation_id.uuid().is_nil()
                    && *version_close_operation_id == task.version_close_operation_id
                    && *dependency_close_operation_id == task.dependency_close_operation_id
                    && matches!(&envelope.observed,
                        Some(Observed::FixedPlanningEngineTaskV1 { task: observed })
                            if observed.task == *task
                                && observed.used_file_slots == *used_file_slots) =>
                {
                    let inventoried: BTreeSet<_> = task.inventory.pending_io.iter()
                        .chain(&task.inventory.buffers)
                        .chain(&task.inventory.readers)
                        .map(|owner| owner.owner_id).collect();
                    let closed_ids: BTreeSet<_> = owners.iter()
                        .map(FixedEngineOwnerDispositionV1::owner_id).collect();
                    if owners.len() != inventoried.len() || closed_ids != inventoried {
                        return Err(codec::invalid());
                    }
                    for (index, slot) in task.file_slots.iter().enumerate() {
                        let matching = owners.iter().find(|closed|
                            closed.owner_id() == slot.owner_id
                                && closed.operation_id() == slot.close_operation_id);
                        let valid = if index < usize::from(*used_file_slots) {
                            matches!(matching,
                                Some(FixedEngineOwnerDispositionV1::ActualOwnedClose { .. }))
                        } else {
                            matches!(matching,
                                Some(FixedEngineOwnerDispositionV1::ProvenNeverAcquired { .. }))
                        };
                        if !valid {
                            return Err(codec::invalid());
                        }
                    }
                    for (owner_id, operation_id) in [
                        (task.version_stdout_owner_id, task.version_stdout_close_operation_id),
                        (task.version_diagnostic_owner_id, task.version_diagnostic_close_operation_id),
                        (task.dependency_stdout_owner_id, task.dependency_stdout_close_operation_id),
                        (task.dependency_diagnostic_owner_id, task.dependency_diagnostic_close_operation_id),
                        (task.hash_buffer_owner_id, task.hash_buffer_close_operation_id),
                    ] {
                        if !owners.iter().any(|owner| matches!(owner,
                            FixedEngineOwnerDispositionV1::ActualOwnedClose {
                                owner_id: actual, operation_id: close,
                            } if *actual == owner_id && *close == operation_id)) {
                            return Err(codec::invalid());
                        }
                    }
                }
                (
                    MemberPlan::FixedPlanningEngineTaskV1 { task, .. },
                    ClosedFact::FixedPlanningEngineTaskV2 {
                        member_id, plan_sha256, join_operation_id,
                        version_close_operation_id, dependency_close_operation_id,
                        used_file_slots, owners,
                    },
                ) => {
                    let Some(Observed::FixedPlanningEngineTaskV2 { task: observed }) =
                        &envelope.observed else { return Err(codec::invalid()); };
                    fixed_probe::validate_engine_task_observed_v2(task, observed)?;
                    if *member_id != task.member_id
                        || plan_sha256 != &observed.plan_sha256
                        || join_operation_id.uuid().is_nil()
                        || *version_close_operation_id != task.version_close_operation_id
                        || *dependency_close_operation_id != task.dependency_close_operation_id
                        || *used_file_slots != observed.used_file_slots
                    {
                        return Err(codec::invalid());
                    }
                    let inventoried: BTreeSet<_> = task.inventory.pending_io.iter()
                        .chain(&task.inventory.buffers)
                        .chain(&task.inventory.readers)
                        .map(|owner| owner.owner_id).collect();
                    let closed_ids: BTreeSet<_> = owners.iter()
                        .map(FixedEngineOwnerDispositionV1::owner_id).collect();
                    if owners.len() != inventoried.len() || closed_ids != inventoried {
                        return Err(codec::invalid());
                    }
                    for (index, slot) in task.file_slots.iter().enumerate() {
                        let matching = owners.iter().find(|closed|
                            closed.owner_id() == slot.owner_id
                                && closed.operation_id() == slot.close_operation_id);
                        let valid = if index < usize::from(*used_file_slots) {
                            matches!(matching,
                                Some(FixedEngineOwnerDispositionV1::ActualOwnedClose { .. }))
                        } else {
                            matches!(matching,
                                Some(FixedEngineOwnerDispositionV1::ProvenNeverAcquired { .. }))
                        };
                        if !valid {
                            return Err(codec::invalid());
                        }
                    }
                    for (owner_id, operation_id) in [
                        (task.version_stdout_owner_id, task.version_stdout_close_operation_id),
                        (task.version_diagnostic_owner_id, task.version_diagnostic_close_operation_id),
                        (task.dependency_stdout_owner_id, task.dependency_stdout_close_operation_id),
                        (task.dependency_diagnostic_owner_id, task.dependency_diagnostic_close_operation_id),
                        (task.hash_buffer_owner_id, task.hash_buffer_close_operation_id),
                    ] {
                        if !owners.iter().any(|owner| matches!(owner,
                            FixedEngineOwnerDispositionV1::ActualOwnedClose {
                                owner_id: actual, operation_id: close,
                            } if *actual == owner_id && *close == operation_id)) {
                            return Err(codec::invalid());
                        }
                    }
                }
                (
                    MemberPlan::FixedPlanningEngineTaskV1 { task, .. },
                    ClosedFact::FixedPlanningEngineTaskNeverAcquiredV1 { task: closed, operation_id },
                ) if task == closed
                    && envelope.observed.is_none()
                    && !operation_id.uuid().is_nil() => {}
                (
                    MemberPlan::FixedProbeMetadataV1 { metadata, .. },
                    ClosedFact::FixedProbeMetadataV1 {
                        metadata: closed,
                        release_operation_id,
                    },
                ) if metadata == closed
                    && !release_operation_id.uuid().is_nil()
                    && envelope.observed.as_ref().is_none_or(|observed|
                        matches!(observed, Observed::FixedProbeMetadataV1 { metadata: actual }
                            if actual == metadata)) => {}
                (
                    MemberPlan::VodOutputTaskV1 { task, .. },
                    ClosedFact::VodOutputTaskDomainExtinctV1 {
                        task: closed,
                        root_member_id,
                        root_pid,
                        root_creation_marker,
                        operation_id,
                    },
                ) if task == closed
                    && task.root_member_id == *root_member_id
                    && *root_pid > 0
                    && *root_creation_marker > 0
                    && !operation_id.uuid().is_nil()
                    && envelope.observed.as_ref().is_none_or(|observed|
                        matches!(observed, Observed::VodOutputTaskV1 { task: actual } if actual == task)) => {}
                (
                    MemberPlan::VodOutputTaskV1 { task, .. },
                    ClosedFact::VodOutputTaskNeverAcquiredV1 {
                        task: closed,
                        operation_id,
                    },
                ) if task == closed
                    && envelope.observed.is_none()
                    && !operation_id.uuid().is_nil() => {}
                (
                    MemberPlan::ScratchAssetsV1 { scratch, .. },
                    ClosedFact::ScratchAssetsV1 {
                        scratch: closed,
                        owners,
                        files,
                        ..
                    },
                ) if scratch == closed => {
                    validate_closed_owners(owners, &scratch.inventory)?;
                    let expected: BTreeSet<_> = scratch.files.iter().map(|f| f.file_id).collect();
                    let actual: BTreeSet<_> = files.iter().copied().collect();
                    if expected != actual || actual.len() != files.len() {
                        return Err(codec::invalid());
                    }
                }
                (
                    MemberPlan::ScratchAssetsV3 { plan, .. },
                    ClosedFact::ScratchAssetsV3 {
                        plan: closed,
                        exact_head,
                        operation_id,
                    },
                ) => {
                    let Some(Observed::ScratchAssetsV3 { state }) = &envelope.observed else {
                        return Err(codec::invalid());
                    };
                    if plan != closed
                        || operation_id.uuid().is_nil()
                        || exact_head.operation_id != *operation_id
                        || exact_head.revision == 0
                        || state.version != 3
                        || state.asset.asset_id != plan.asset_id
                        || state.asset.lifecycle != scratch::ScratchLifecycleV1::RetiredClosed
                        || state.asset.head.as_ref() != Some(exact_head)
                        || state.asset.directory.is_some()
                        || state.asset.totals != scratch::AssetTotalsV1::default()
                        || state.bootstrap.phase != scratch::bootstrap::BootstrapPhase::Closed
                        || state.bootstrap.current.is_some()
                        || state.bootstrap.parent.is_some()
                        || state.bootstrap.possible_child.is_some()
                        || state.bootstrap.retained_parent_synced
                    {
                        return Err(codec::invalid());
                    }
                }
                (
                    MemberPlan::ScratchAssetsV3 { plan, .. },
                    ClosedFact::ScratchAssetsNeverAcquiredV3 {
                        plan: closed,
                        operation_id,
                    },
                ) if plan == closed
                    && envelope.observed.is_none()
                    && !operation_id.uuid().is_nil() => {}
                _ => return Err(codec::invalid()),
            },
            MemberDisposition::Transferred {
                transfer,
                target_obligation_id,
                target_graph_id,
                asset_id,
                operation_id,
            } => {
                if *operation_id != value.operation_id
                    || *target_graph_id == graph_id
                    || transfer.target_member_id == id
                {
                    return Err(codec::invalid());
                }
                validate_transfer_identity(envelope, transfer)?;
                let _ = (target_obligation_id, asset_id); // Cross-row equality checked in restoration/transaction.
            }
        }
    }
    codec::encode(value)?;
    Ok(())
}
fn validate_transfer_identity(
    envelope: &Envelope,
    transfer: &MemberTransfer,
) -> Result<(), CustodyError> {
    match (&envelope.planned, &envelope.observed, &transfer.identity) {
        (
            MemberPlan::SourceV1 { source: plan, .. },
            Some(Observed::SourceV1 { source: observed }),
            TransferIdentity::SourceV1 {
                source,
                retained_copy_ids,
                closed_copies,
            },
        ) if source == observed => {
            let retained: BTreeSet<_> = retained_copy_ids.iter().copied().collect();
            let closed: BTreeSet<_> = closed_copies.iter().map(|c| c.copy().copy_id).collect();
            let planned: BTreeSet<_> = plan.planned_copies.iter().map(|c| c.copy_id).collect();
            if retained.is_empty()
                || retained.len() != retained_copy_ids.len()
                || closed.len() != closed_copies.len()
                || !retained.is_disjoint(&closed)
                || retained.union(&closed).copied().collect::<BTreeSet<_>>() != planned
                || retained
                    .iter()
                    .any(|id| !source.copies.iter().any(|c| c.copy_id == *id))
                || closed_copies
                    .iter()
                    .any(|c| !plan.planned_copies.contains(c.copy()))
            {
                return Err(codec::invalid());
            }
            validate_copy_closures(plan, &envelope.observed, closed_copies)?;
            Ok(())
        }
        (
            MemberPlan::ScratchAssetsV1 { .. },
            Some(Observed::ScratchAssetsV1 { scratch: observed }),
            TransferIdentity::ScratchAssetsV1 { scratch },
        ) if scratch == observed => Ok(()),
        (
            MemberPlan::ScratchAssetsV3 {
                plan: source_plan, ..
            },
            Some(Observed::ScratchAssetsV3 { state }),
            TransferIdentity::ScratchAssetsV3 { plan, snapshot },
        ) if plan == source_plan
            && snapshot.state == *state
            && snapshot.member_id == transfer.source_member_id
            && snapshot.version == 1 =>
        {
            scratch::bootstrap::validate_frozen_snapshot_ref_shape(plan, snapshot)
        }
        _ => Err(codec::invalid()),
    }
}
pub(crate) fn validate_retained_assets(
    manifest: &Manifest,
    custody: &RetainedAssets,
) -> Result<(), CustodyError> {
    if custody.version != 1
        || custody.target_graph_id != manifest.graph_id
        || custody.target_owner_id != manifest.owner_id
        || custody.producer_graph_id == custody.target_graph_id
        || custody.producer_obligation_id == custody.target_obligation_id
        || custody.transfers.is_empty()
        || custody.transfers.len() != manifest.members.len()
        || custody.reader_ids.len() > codec::ACTIVE_LIMIT
        || custody.reader_ids.iter().collect::<BTreeSet<_>>().len() != custody.reader_ids.len()
        || custody.directory_object.file_type != FileType::Directory
    {
        return Err(codec::invalid());
    }
    codec::long_text(&custody.rendition_id)?;
    codec::long_text(&custody.directory_path)?;
    custody.directory_object.validate()?;
    if !std::path::Path::new(&custody.directory_path).is_absolute() {
        return Err(codec::invalid());
    }
    let sources: BTreeSet<_> = custody
        .transfers
        .iter()
        .map(|t| t.source_member_id)
        .collect();
    let targets: BTreeSet<_> = custody
        .transfers
        .iter()
        .map(|t| t.target_member_id)
        .collect();
    if sources.len() != custody.transfers.len()
        || targets.len() != custody.transfers.len()
        || !sources.is_disjoint(&targets)
    {
        return Err(codec::invalid());
    }
    for member in &manifest.members {
        if !targets.iter().any(|id| id.uuid() == member.member_id()) {
            return Err(codec::invalid());
        }
        match member {
            MemberPlan::SourceV1 { source, .. } => {
                if source.planned_copies.iter().any(|copy| !matches!(copy.owner, CopyOwner::RetainedAssets { asset_id, source_member_id } if asset_id == custody.asset_id && source_member_id == source.member_id)) { return Err(codec::invalid()); }
            },
            MemberPlan::ScratchAssetsV1 { scratch, .. } if scratch.asset_id == custody.asset_id && scratch.directory_path == custody.directory_path && scratch.directory_object == custody.directory_object => {},
            _ => return Err(codec::invalid()),
        }
    }
    super::retained_minimum_resources(manifest)?;
    let expected_readers: BTreeSet<_> = manifest
        .members
        .iter()
        .filter_map(|m| match m {
            MemberPlan::ScratchAssetsV1 { scratch, .. } => Some(&scratch.inventory.readers),
            _ => None,
        })
        .flatten()
        .map(|r| r.owner_id)
        .collect();
    if expected_readers != custody.reader_ids.iter().copied().collect() {
        return Err(codec::invalid());
    }
    if let Reconciliation::OldDomainRequired { owner_id, domain } = &custody.reconciliation {
        if *owner_id != custody.target_owner_id || domain.daemon_owner_id != *owner_id {
            return Err(codec::invalid());
        }
        domain.validate()?;
    }
    codec::encode(custody)?;
    Ok(())
}

fn validate_copy_closures(
    plan: &SourcePlan,
    observed: &Option<Observed>,
    copies: &[CopyClosed],
) -> Result<(), CustodyError> {
    for copy in copies {
        match copy {
            CopyClosed::ProvenNeverAcquired { copy, .. } => {
                if matches!(observed, Some(Observed::SourceV1 { source }) if source.copies.iter().any(|c| c.copy_id == copy.copy_id))
                {
                    return Err(codec::invalid());
                }
            }
            CopyClosed::DomainExtinct { domain, .. } => {
                if domain != &plan.filesystem_domain
                    || !matches!(domain.os_boot, BootIdentity::Known { .. })
                    || !matches!(domain.mount_namespace, NativeValue::Known { .. })
                {
                    return Err(codec::invalid());
                }
            }
            CopyClosed::ActualOwnedClose { .. } => {}
        }
    }
    Ok(())
}
