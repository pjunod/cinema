//! Compact V3 retained data contract. Physical transfer and old-domain closure
//! remain separate, and resource credit needs the original frozen inventory.
use super::*;
use crate::adaptive_resources::ResourceVector;
use scratch::bootstrap::{
    validate_frozen_snapshot_ref, validate_frozen_snapshot_ref_shape, FrozenScratchRefV1,
    FrozenScratchV3, ScratchPlanV3,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedAssetsV2 {
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
    pub transfer: MemberTransfer,
    pub reader_ids: Vec<Id>,
    pub reconciliation: Reconciliation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RetainedAssetsDocument {
    V1(RetainedAssets),
    V2(RetainedAssetsV2),
}
impl RetainedAssetsDocument {
    pub(crate) fn asset_id(&self) -> Id {
        match self {
            Self::V1(v) => v.asset_id,
            Self::V2(v) => v.asset_id,
        }
    }
    pub(crate) fn target_obligation_id(&self) -> Id {
        match self {
            Self::V1(v) => v.target_obligation_id,
            Self::V2(v) => v.target_obligation_id,
        }
    }
    pub(crate) fn rendition_id(&self) -> &str {
        match self {
            Self::V1(v) => &v.rendition_id,
            Self::V2(v) => &v.rendition_id,
        }
    }
    pub(crate) fn generation_epoch(&self) -> u64 {
        match self {
            Self::V1(v) => v.generation_epoch,
            Self::V2(v) => v.generation_epoch,
        }
    }
}

pub(crate) fn decode_retained_assets(json: &str) -> Result<RetainedAssetsDocument, CustodyError> {
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    match codec::decode::<Version>(json)?.version {
        1 => Ok(RetainedAssetsDocument::V1(codec::decode(json)?)),
        2 => Ok(RetainedAssetsDocument::V2(codec::decode(json)?)),
        _ => Err(codec::invalid()),
    }
}

fn target<'a>(
    manifest: &'a Manifest,
    custody: &'a RetainedAssetsV2,
) -> Result<(&'a ScratchPlanV3, &'a FrozenScratchRefV1), CustodyError> {
    if manifest.members.len() != 1 {
        return Err(codec::invalid());
    }
    let MemberPlan::ScratchAssetsV3 { slot, plan } = &manifest.members[0] else {
        return Err(codec::invalid());
    };
    let TransferIdentity::ScratchAssetsV3 {
        plan: source_plan,
        snapshot,
    } = &custody.transfer.identity
    else {
        return Err(codec::invalid());
    };
    if plan != source_plan
        || slot.owned_by.is_some()
        || !slot.depends_on.is_empty()
        || slot.protected_paths.len() != 1
        || slot.protected_paths[0] != plan.bootstrap().protected_child_path
        || custody.transfer.source_member_id != snapshot.member_id
        || custody.transfer.target_member_id != slot.member_id
        || custody.transfer.source_member_id == custody.transfer.target_member_id
        || custody.producer_obligation_id != snapshot.obligation_id
        || custody.asset_id != plan.asset_id
        || custody.rendition_id != plan.rendition_id
        || custody.generation_epoch != plan.generation_epoch
    {
        return Err(codec::invalid());
    }
    validate_frozen_snapshot_ref_shape(plan, snapshot)?;
    let child = snapshot
        .state
        .asset
        .directory
        .as_ref()
        .ok_or_else(codec::invalid)?;
    if child.native_object.filesystem_domain != plan.filesystem_domain
        || child.native_object.file_type != FileType::Directory
        || child.held_copy_ids.is_empty()
    {
        return Err(codec::invalid());
    }
    Ok((plan, snapshot))
}

/// Validates the compact target shape. It does not establish full inventory
/// provenance; the transfer writer/recovery must compare the original fold.
pub(crate) fn validate_retained_assets_v2(
    manifest: &Manifest,
    custody: &RetainedAssetsV2,
) -> Result<(), CustodyError> {
    validate_manifest(manifest)?;
    if custody.version != 2
        || custody.target_graph_id != manifest.graph_id
        || custody.target_owner_id != manifest.owner_id
        || custody.producer_graph_id == custody.target_graph_id
        || custody.producer_obligation_id == custody.target_obligation_id
        || custody.reader_ids.len() > codec::ACTIVE_LIMIT
        || custody.reader_ids.iter().collect::<BTreeSet<_>>().len() != custody.reader_ids.len()
    {
        return Err(codec::invalid());
    }
    let (plan, snapshot) = target(manifest, custody)?;
    if plan.filesystem_domain.daemon_owner_id != custody.target_owner_id
        || custody.reader_ids.len() != snapshot.reader_count as usize
    {
        return Err(codec::invalid());
    }
    codec::text(&custody.rendition_id)?;
    if let Reconciliation::OldDomainRequired { owner_id, domain } = &custody.reconciliation {
        if *owner_id != custody.target_owner_id || domain.daemon_owner_id != *owner_id {
            return Err(codec::invalid());
        }
        domain.validate()?;
    }
    codec::encode(custody)?;
    Ok(())
}

/// A retained minimum is only trusted after an exact restored-full/ref match.
pub(crate) fn retained_minimum_resources_v2(
    manifest: &Manifest,
    custody: &RetainedAssetsV2,
    restored_full: &FrozenScratchV3,
) -> Result<ResourceVector, CustodyError> {
    validate_retained_assets_v2(manifest, custody)?;
    let (plan, snapshot) = target(manifest, custody)?;
    validate_frozen_snapshot_ref(plan, snapshot, restored_full)?;
    let actual_readers: BTreeSet<_> = restored_full.readers.iter().map(|r| r.lease_id).collect();
    let named_readers: BTreeSet<_> = custody.reader_ids.iter().copied().collect();
    if actual_readers.len() != restored_full.readers.len() || actual_readers != named_readers {
        return Err(codec::invalid());
    }
    let totals = &restored_full.state.asset.totals;
    if totals.handles_reserved == 0 || totals.charged_storage_bytes == 0 {
        return Err(codec::invalid());
    }
    Ok(ResourceVector {
        handles: u64::from(totals.handles_reserved),
        peak_memory_bytes: totals.memory_reserved_bytes,
        scratch_bytes: totals.charged_storage_bytes,
        ..ResourceVector::default()
    })
}

pub(crate) fn validate_retained_resources_v2(
    manifest: &Manifest,
    custody: &RetainedAssetsV2,
    restored_full: &FrozenScratchV3,
    resources: ResourceVector,
) -> Result<(), CustodyError> {
    let minimum = retained_minimum_resources_v2(manifest, custody, restored_full)?;
    if resources.cpu_ns_per_second != 0
        || resources.processes != 0
        || resources.hardware_slots != 0
        || resources.tuner_slots != 0
        || resources.checked_sub(minimum).is_none()
    {
        return Err(codec::invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
