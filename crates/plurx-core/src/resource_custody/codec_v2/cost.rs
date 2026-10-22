//! Validate retained charges against the concrete retained handle/file inventory.
//! This never admits acquisition or infers closure from an on-disk size.
use super::*;
use crate::adaptive_resources::ResourceVector;

pub(crate) fn retained_minimum_resources(
    manifest: &Manifest,
) -> Result<ResourceVector, CustodyError> {
    let mut minimum = ResourceVector::default();
    let mut holders = BTreeSet::new();
    let mut file_ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut owners = BTreeSet::new();
    let mut directories = 0usize;
    for member in &manifest.members {
        let part = match member {
            MemberPlan::SourceV1 { source, .. } => {
                if !holders.insert(source.holder_id) {
                    return Err(codec::invalid());
                }
                ResourceVector {
                    handles: u64::try_from(source.planned_copies.len())
                        .map_err(|_| codec::invalid())?,
                    ..ResourceVector::default()
                }
            }
            MemberPlan::ScratchAssetsV1 { scratch, .. } => {
                if !scratch.inventory.pending_io.is_empty() {
                    return Err(codec::invalid());
                }
                directories += 1;
                let mut part = ResourceVector {
                    handles: 1,
                    ..ResourceVector::default()
                };
                for file in &scratch.files {
                    if !file_ids.insert(file.file_id)
                        || !paths.insert((&scratch.directory_path, &file.relative_path))
                    {
                        return Err(codec::invalid());
                    }
                    part.scratch_bytes = part
                        .scratch_bytes
                        .checked_add(file.charged_bytes)
                        .ok_or_else(codec::invalid)?;
                }
                for owner in scratch
                    .inventory
                    .buffers
                    .iter()
                    .chain(&scratch.inventory.readers)
                {
                    if !owners.insert(owner.owner_id) {
                        return Err(codec::invalid());
                    }
                    part.peak_memory_bytes = part
                        .peak_memory_bytes
                        .checked_add(owner.memory_bytes)
                        .ok_or_else(codec::invalid)?;
                    part.handles = part
                        .handles
                        .checked_add(owner.handles)
                        .ok_or_else(codec::invalid)?;
                }
                part
            }
            _ => return Err(codec::invalid()),
        };
        minimum = minimum.checked_add(part).ok_or_else(codec::invalid)?;
    }
    if directories == 0 {
        return Err(codec::invalid());
    }
    Ok(minimum)
}

pub(crate) fn validate_retained_resources(
    manifest: &Manifest,
    resources: ResourceVector,
) -> Result<(), CustodyError> {
    let minimum = retained_minimum_resources(manifest)?;
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
