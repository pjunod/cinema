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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SettlementDocument {
    Physical(Settlement),
    NeverArmed(NeverArmed),
}
pub(crate) fn decode_settlement(json: &str) -> Result<SettlementDocument, CustodyError> {
    #[derive(Deserialize)]
    struct Discriminator {
        kind: Option<String>,
    }
    match codec::decode::<Discriminator>(json)?.kind.as_deref() {
        None => Ok(SettlementDocument::Physical(codec::decode(json)?)),
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
pub(crate) enum ClosedFact {
    LegacyV1 {
        fact: super::super::ClosedMemberFact,
    },
    SourceV1 {
        holder_id: Id,
        copies: Vec<CopyClosed>,
    },
    VodOutputTaskV1 {
        task: VodOutputTask,
        join_operation_id: Id,
        owners: Vec<OwnerClosed>,
    },
    ScratchAssetsV1 {
        scratch: ScratchAssets,
        operation_id: Id,
        owners: Vec<OwnerClosed>,
        files: Vec<Id>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum TransferIdentity {
    SourceV1 {
        source: SourceObserved,
        retained_copy_ids: Vec<Id>,
        closed_copies: Vec<CopyClosed>,
    },
    ScratchAssetsV1 {
        scratch: ScratchAssets,
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
    if value.version != 2
        || value.graph_id != manifest.graph_id
        || value.owner_id != manifest.owner_id
        || value.members.len() != manifest.members.len()
        || envelopes.len() != manifest.members.len()
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
                    MemberPlan::VodOutputTaskV1 { task, .. },
                    ClosedFact::VodOutputTaskV1 {
                        task: closed,
                        owners,
                        ..
                    },
                ) if task == closed => validate_closed_owners(owners, &task.inventory)?,
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
                    || *target_graph_id == manifest.graph_id
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
