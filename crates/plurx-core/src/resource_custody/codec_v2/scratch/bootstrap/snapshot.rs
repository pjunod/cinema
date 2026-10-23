//! Data-only frozen V3 inventory and its compact journal-bound reference.
//! Neither value authorizes native transfer or closure.
use super::*;
use sha2::{Digest, Sha256};
use std::io::{self, Write};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenScratchV3 {
    pub version: u32,
    pub obligation_id: Id,
    pub member_id: Id,
    pub state: ScratchStateV3,
    pub files: Vec<FileObservedV1>,
    pub readers: Vec<ReaderLeaseV1>,
}

/// Compact disk record; inventory must be recovered from the original journal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenScratchRefV1 {
    pub version: u32,
    pub obligation_id: Id,
    pub member_id: Id,
    pub state: ScratchStateV3,
    pub file_count: u32,
    pub reader_count: u32,
    pub inventory_sha256: String,
}

/// Recount the full inventory and bootstrap charges; this is not native proof.
pub(crate) fn validate_frozen_snapshot(
    plan: &ScratchPlanV3,
    snapshot: &FrozenScratchV3,
) -> Result<(), CustodyError> {
    BootstrapFold::validate_frozen_snapshot(plan, snapshot)
}

struct HashWriter(Sha256);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Stream the ordered file/reader inventory into SHA-256; never materialize
/// the complete JSON or impose the SQL row limit on the in-memory snapshot.
pub(crate) fn frozen_snapshot_ref(
    plan: &ScratchPlanV3,
    snapshot: &FrozenScratchV3,
) -> Result<FrozenScratchRefV1, CustodyError> {
    validate_frozen_snapshot(plan, snapshot)?;
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, &(&snapshot.files, &snapshot.readers))
        .map_err(|_| codec::invalid())?;
    let reference = FrozenScratchRefV1 {
        version: 1,
        obligation_id: snapshot.obligation_id,
        member_id: snapshot.member_id,
        state: snapshot.state.clone(),
        file_count: u32::try_from(snapshot.files.len()).map_err(|_| codec::invalid())?,
        reader_count: u32::try_from(snapshot.readers.len()).map_err(|_| codec::invalid())?,
        inventory_sha256: hex::encode(writer.0.finalize()),
    };
    validate_frozen_snapshot_ref_shape(plan, &reference)?;
    Ok(reference)
}

/// Validate the compact row's shape. This does not recount file charges;
/// consumers must also match it to the original journal-restored inventory.
pub(crate) fn validate_frozen_snapshot_ref_shape(
    plan: &ScratchPlanV3,
    reference: &FrozenScratchRefV1,
) -> Result<(), CustodyError> {
    validate_plan(plan)?;
    validate_projection(plan, &reference.state)?;
    let state = &reference.state;
    let child = state.asset.directory.as_ref().ok_or_else(codec::invalid)?;
    if reference.version != 1
        || state.asset.lifecycle != ScratchLifecycleV1::Frozen
        || state.asset.totals.pending_jobs != 0
        || reference.file_count != state.asset.totals.resident_file_objects
        || reference.reader_count != state.asset.totals.readers
        || state.bootstrap.possible_child.is_some()
        || state.bootstrap.current.is_some()
        || usize::from(state.bootstrap.next_component) != plan.bootstrap().components.len()
        || !matches!(
            state.bootstrap.phase,
            BootstrapPhase::SinkReady | BootstrapPhase::Closed
        )
        || !matches!(child.native_object.device, NativeValue::Known { .. })
        || !matches!(child.native_object.inode, NativeValue::Known { .. })
        || !matches!(child.native_object.mount_id, NativeValue::Known { .. })
        || state.asset.totals.charged_storage_bytes
            < plan
                .bootstrap()
                .bounds
                .child_directory_reserve_bytes
                .checked_add(plan.bootstrap().bounds.parent_entry_reserve_bytes)
                .ok_or_else(codec::invalid)?
        || reference.inventory_sha256.len() != 64
        || !reference
            .inventory_sha256
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(codec::invalid());
    }
    match state.bootstrap.phase {
        BootstrapPhase::SinkReady => {
            let parent = state.bootstrap.parent.as_ref().ok_or_else(codec::invalid)?;
            if parent.kind != BootstrapDirectoryKind::ConfiguredParentV1
                || parent.observed.held_copy_ids.is_empty()
                || parent.observed.native_object.file_type != FileType::Directory
                || parent.observed.native_object.filesystem_domain != plan.filesystem_domain
                || parent.observed.native_object.device != child.native_object.device
                || parent.observed.native_object.mount_id != child.native_object.mount_id
                || state.asset.totals.memory_reserved_bytes
                    < plan.bootstrap().bounds.resident_memory_bytes
            {
                return Err(codec::invalid());
            }
            parent.observed.native_object.validate()?;
        }
        BootstrapPhase::Closed if state.bootstrap.parent.is_none() => {}
        _ => return Err(codec::invalid()),
    }
    codec::encode(reference)?;
    Ok(())
}

/// Only an exact match to the full journal-restored snapshot validates a ref.
pub(crate) fn validate_frozen_snapshot_ref(
    plan: &ScratchPlanV3,
    reference: &FrozenScratchRefV1,
    restored_full: &FrozenScratchV3,
) -> Result<(), CustodyError> {
    validate_frozen_snapshot_ref_shape(plan, reference)?;
    if *reference != frozen_snapshot_ref(plan, restored_full)? {
        return Err(codec::invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
