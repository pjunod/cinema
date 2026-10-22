//! Growing finite output audit. The plan contains slots/bounds, never future
//! native objects; the compact projection is checked against durable event rows.
//! These serde values are audit data, NOT native acquisition/closure authority.
pub(crate) mod bootstrap;
mod fold;
mod output_view;
pub(crate) use fold::ScratchFold;
use output_view::{validate_outputs_limits, OutputPlanView};
#[cfg(test)]
pub(in crate::resource_custody) mod tests;
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct FileSlotId(pub Id);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnedDirectoryRefV1 {
    pub owning_obligation: Id,
    pub member_id: Id,
    pub directory_id: Id,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum DirectoryModeV1 {
    CreateExclusive,
    BorrowExisting {
        asset_owner_ref: OwnedDirectoryRefV1,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectoryPlanV2 {
    pub protected_absolute_path: String,
    pub parent_binding: OwnedDirectoryRefV1,
    pub child_component: String,
    pub mode: DirectoryModeV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutputNamingV1 {
    FilmSegmentV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutputNamesV1 {
    pub init: bool,
    pub first_segment: u32,
    pub segment_end_exclusive: u64,
    pub naming: OutputNamingV1,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum OutputKey {
    Init,
    Segment { index: u32 },
}
impl OutputNamesV1 {
    pub(crate) fn contains(&self, key: &OutputKey) -> bool {
        match key {
            OutputKey::Init => self.init,
            OutputKey::Segment { index } => {
                *index >= self.first_segment && u64::from(*index) < self.segment_end_exclusive
            }
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum AllocationAccountingV1 {
    LogicalAndReportedAllocationV1 {
        allocation_unit_bytes: u64,
        per_object_reserve_bytes: u64,
        directory_reserve_bytes: u64,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScratchLimitsV1 {
    pub resident_file_objects: u32,
    pub resident_logical_bytes: u64,
    pub reserved_storage_bytes: u64,
    pub one_file_logical_bytes: u64,
    pub one_io_payload_bytes: u64,
    pub pending_native_jobs: u16,
    pub live_handles: u32,
    pub reader_leases: u32,
    pub resident_memory_bytes: u64,
    pub allocation: AllocationAccountingV1,
}
impl ScratchLimitsV1 {
    pub(crate) fn storage_for(&self, end: u64) -> Result<u64, CustodyError> {
        let AllocationAccountingV1::LogicalAndReportedAllocationV1 {
            allocation_unit_bytes: unit,
            per_object_reserve_bytes,
            ..
        } = self.allocation;
        end.checked_add(unit.checked_sub(1).ok_or_else(codec::invalid)?)
            .and_then(|n| n.checked_div(unit))
            .and_then(|n| n.checked_mul(unit))
            .and_then(|n| n.checked_add(per_object_reserve_bytes))
            .ok_or_else(codec::invalid)
    }
    pub(crate) fn directory_charge(&self) -> u64 {
        let AllocationAccountingV1::LogicalAndReportedAllocationV1 {
            directory_reserve_bytes,
            ..
        } = self.allocation;
        directory_reserve_bytes
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScratchPlanV2 {
    pub version: u32,
    pub asset_id: Id,
    pub rendition_id: String,
    pub generation_epoch: u64,
    pub filesystem_domain: FilesystemDomain,
    pub directory: DirectoryPlanV2,
    pub outputs: OutputNamesV1,
    pub limits: ScratchLimitsV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectoryObservedV1 {
    pub directory_id: Id,
    pub native_object: NativeObject,
    pub acquisition_io_id: Id,
    pub held_copy_ids: Vec<Id>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssetEventRefV1 {
    pub operation_id: Id,
    pub revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScratchLifecycleV1 {
    Open,
    Freezing,
    Frozen,
    RetainedReadOnly,
    Closing,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssetTotalsV1 {
    pub resident_file_objects: u32,
    pub logical_bytes: u64,
    pub charged_storage_bytes: u64,
    pub memory_reserved_bytes: u64,
    pub handles_reserved: u32,
    pub readers: u32,
    pub pending_jobs: u16,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScratchStateV2 {
    pub version: u32,
    pub asset_id: Id,
    pub directory: Option<DirectoryObservedV1>,
    pub head: Option<AssetEventRefV1>,
    pub revision: u64,
    pub lifecycle: ScratchLifecycleV1,
    pub totals: AssetTotalsV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileObservedV1 {
    pub file_id: Id,
    pub slot_id: FileSlotId,
    pub creation_io_id: Id,
    pub native_object: NativeObject,
    pub temporary: bool,
    pub published: Option<OutputKey>,
    pub logical_len: u64,
    pub reported_allocated_bytes: NativeValue<u64>,
    pub charged_bytes: u64,
    pub held_copy_ids: Vec<Id>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ByteRangeV1 {
    pub start: u64,
    pub end_exclusive: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReaderLeaseV1 {
    pub lease_id: Id,
    pub asset_id: Id,
    pub file_id: Id,
    pub copy_id: Id,
    pub owner_id: Id,
    pub memory_cap: u64,
    pub read_range: ByteRangeV1,
    pub cursor: u64,
    pub acquisition_io_id: Id,
}
/// Concrete compound operations, not arbitrary callbacks or unbounded writes.
/// These audit variants do not supply their native/runtime ownership adapters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum AssetIoV1 {
    CreateDirectory {
        copy_id: Id,
    },
    PublishNew {
        slot_id: FileSlotId,
        output: OutputKey,
        logical_end: u64,
        payload_capacity: u64,
        copy_id: Id,
    },
    RemoveOwned {
        file_id: Id,
    },
    SyncFiles {
        file_ids: Vec<Id>,
    },
    OpenReader {
        lease: ReaderLeaseV1,
    },
    ReadRange {
        lease_id: Id,
        range: ByteRangeV1,
    },
    CloseReader {
        lease_id: Id,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum NativeOutcomeV1 {
    Completed,
    Failed { error_class: String },
    NeverEntered,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum AssetObservationV1 {
    Directory {
        observed: DirectoryObservedV1,
    },
    File {
        observed: FileObservedV1,
        content_binding: Option<String>,
    },
    Removed {
        file_id: Id,
    },
    Synced,
    ReaderOpened {
        lease: ReaderLeaseV1,
        native_object: NativeObject,
    },
    ReaderRead {
        lease_id: Id,
        bytes_read: u64,
    },
    ReaderClosed {
        lease_id: Id,
    },
    NoAcquisition,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(crate) enum AssetEventV1 {
    ArmIo {
        io_id: Id,
        operation: AssetIoV1,
    },
    ObserveIo {
        io_id: Id,
        native_join_operation_id: Id,
        outcome: NativeOutcomeV1,
        observation: AssetObservationV1,
    },
    Fence {
        terminal_epoch: u64,
    },
    Freeze {
        exact_head_before_freeze: AssetEventRefV1,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssetChangeV2 {
    pub scratch_member_id: Id,
    pub asset_id: Id,
    pub generation_epoch: u64,
    pub previous: Option<AssetEventRefV1>,
    pub event: AssetEventV1,
}

pub(crate) fn validate_plan(plan: &ScratchPlanV2) -> Result<(), CustodyError> {
    codec::text(&plan.rendition_id)?;
    codec::long_text(&plan.directory.protected_absolute_path)?;
    component(&plan.directory.child_component)?;
    plan.filesystem_domain.validate()?;
    let path = std::path::Path::new(&plan.directory.protected_absolute_path);
    if plan.version != 2
        || !path.is_absolute()
        || path.components().any(|c| {
            !matches!(
                c,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
        || path.file_name().and_then(|s| s.to_str())
            != Some(plan.directory.child_component.as_str())
    {
        return Err(codec::invalid());
    }
    validate_outputs_limits(plan.generation_epoch, &plan.outputs, &plan.limits)
}
fn component(value: &str) -> Result<(), CustodyError> {
    codec::text(value)?;
    if value == "." || value == ".." || value.contains(['/', '\\', '\0']) {
        return Err(codec::invalid());
    }
    Ok(())
}
pub(crate) fn validate_projection(
    plan: &ScratchPlanV2,
    state: &ScratchStateV2,
) -> Result<(), CustodyError> {
    validate_output_projection(plan.output_view(), state)
}
fn validate_output_projection(
    plan: OutputPlanView<'_>,
    state: &ScratchStateV2,
) -> Result<(), CustodyError> {
    let t = &state.totals;
    let l = &plan.limits;
    if state.version != 2
        || state.asset_id != plan.asset_id
        || (state.revision == 0) != state.head.is_none()
        || state
            .head
            .as_ref()
            .is_some_and(|h| h.revision != state.revision)
        || t.resident_file_objects > l.resident_file_objects
        || t.logical_bytes > l.resident_logical_bytes
        || t.charged_storage_bytes > l.reserved_storage_bytes
        || t.memory_reserved_bytes > l.resident_memory_bytes
        || t.handles_reserved > l.live_handles
        || t.readers > l.reader_leases
        || t.pending_jobs > l.pending_native_jobs
    {
        return Err(codec::invalid());
    }
    if let Some(dir) = &state.directory {
        if dir.native_object.file_type != FileType::Directory
            || &dir.native_object.filesystem_domain != plan.filesystem_domain
            || dir.held_copy_ids.is_empty()
            || dir.held_copy_ids.len() > 16
            || dir.held_copy_ids.iter().collect::<BTreeSet<_>>().len() != dir.held_copy_ids.len()
        {
            return Err(codec::invalid());
        }
        dir.native_object.validate()?;
    }
    Ok(())
}
