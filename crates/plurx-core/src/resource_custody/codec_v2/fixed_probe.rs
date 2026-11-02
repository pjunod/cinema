//! Strict audit vocabulary for the selected fixed FFprobe planning role.
//! The plan and observations describe original owners; they never grant a
//! launch, stand in for a join, or authorize a memory refund.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FixedProbeTaskV1 {
    pub version: u32,
    pub root_member_id: Id,
    pub member_id: Id,
    pub source_member_id: Id,
    pub executable_member_id: Id,
    pub media_file_id: String,
    pub recipe_source_binding: String,
    pub sealed_probe_digest: String,
    pub stdout_owner_id: Id,
    pub diagnostic_owner_id: Id,
    pub inventory: IoInventory,
}

/// Cold P role. Unlike FixedProbeTaskV1, the exact ELF digest cannot exist in
/// the plan: the original P task discovers and seals that image after prepare.
/// The task's positive native completion is separately consumed by the fixed
/// physical adapter before these observed bytes can settle the role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FixedProbeDiscoverTaskV1 {
    pub version: u32,
    pub member_id: Id,
    pub source_member_id: Id,
    pub version_root_member_id: Id,
    pub document_root_member_id: Id,
    /// Durable close arms named before either original native root may enter.
    pub version_close_operation_id: Id,
    pub document_close_operation_id: Id,
    pub media_file_id: String,
    pub recipe_source_binding: String,
    pub configured_locator: String,
    pub image_owner_id: Id,
    pub image_close_operation_id: Id,
    pub snapshot_owner_id: Id,
    pub snapshot_close_operation_id: Id,
    pub source_child_owner_id: Id,
    pub source_child_close_operation_id: Id,
    pub version_stdout_owner_id: Id,
    pub version_stdout_close_operation_id: Id,
    pub stdout_owner_id: Id,
    pub stdout_close_operation_id: Id,
    pub diagnostic_owner_id: Id,
    pub diagnostic_close_operation_id: Id,
    pub inventory: IoInventory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FixedProbeDiscoverTaskObservedV1 {
    pub task: FixedProbeDiscoverTaskV1,
    pub executable_object_version: String,
    pub executable_sha256: String,
    pub build_digest: String,
}

/// One original File slot per possible FFmpeg image/dependency object. An
/// unentered slot must be proved never acquired; used slots close their SAME
/// File after a bounded hash. The fixed limit is admission policy, not a
/// claim about the host's actual dependency count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FixedPlanningEngineFileSlotV1 {
    pub owner_id: Id,
    pub close_operation_id: Id,
}

pub(crate) const FIXED_PLANNING_ENGINE_MAX_OBJECTS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FixedPlanningEngineTaskV1 {
    pub version: u32,
    pub member_id: Id,
    pub version_root_member_id: Id,
    pub dependency_root_member_id: Id,
    pub version_close_operation_id: Id,
    pub dependency_close_operation_id: Id,
    pub configured_ffmpeg_locator: String,
    pub dependency_tool_locator: String,
    pub file_slots: Vec<FixedPlanningEngineFileSlotV1>,
    pub version_stdout_owner_id: Id,
    pub version_stdout_close_operation_id: Id,
    pub version_diagnostic_owner_id: Id,
    pub version_diagnostic_close_operation_id: Id,
    pub dependency_stdout_owner_id: Id,
    pub dependency_stdout_close_operation_id: Id,
    pub dependency_diagnostic_owner_id: Id,
    pub dependency_diagnostic_close_operation_id: Id,
    pub hash_buffer_owner_id: Id,
    pub hash_buffer_close_operation_id: Id,
    pub inventory: IoInventory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FixedPlanningEngineTaskObservedV1 {
    pub task: FixedPlanningEngineTaskV1,
    pub used_file_slots: u16,
    pub media_digest: String,
}

/// Compact audit binding to the unchanged V1 plan. The digest preserves
/// the old full task equality without serializing the 256-slot plan twice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FixedPlanningEngineTaskObservedV2 {
    pub member_id: Id,
    pub plan_sha256: String,
    pub used_file_slots: u16,
    pub media_digest: String,
}

// Same streaming serde/SHA pattern as the frozen inventory reference. Enforce
// the existing row bound while hashing; never allocate the full plan JSON.
fn bounded_audit_sha256(value: &impl Serialize) -> Result<String, CustodyError> {
    use sha2::{Digest, Sha256};
    use std::io::{self, Write};
    struct HashWriter {
        hash: Sha256,
        bytes: usize,
    }
    impl Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let next = self
                .bytes
                .checked_add(bytes.len())
                .filter(|next| *next <= codec::JSON_LIMIT)
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
            self.hash.update(bytes);
            self.bytes = next;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter {
        hash: Sha256::new(),
        bytes: 0,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| codec::invalid())?;
    Ok(hex::encode(writer.hash.finalize()))
}

/// Same serde wire-size bound, without allocating a serialized row or digest.
/// Borrowed runtime preflight must not clone a 256-slot engine manifest.
pub(crate) fn validate_bounded_audit_size(value: &impl Serialize) -> Result<(), CustodyError> {
    use std::io::{self, Write};
    struct SizeWriter(usize);
    impl Write for SizeWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|n| *n <= codec::JSON_LIMIT)
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(&mut SizeWriter(0), value).map_err(|_| codec::invalid())
}

pub(crate) fn engine_task_plan_sha256(
    task: &FixedPlanningEngineTaskV1,
) -> Result<String, CustodyError> {
    bounded_audit_sha256(task)
}

/// Preflight the whole emitted settlement before consuming native witnesses.
/// This grants no close authority and keeps the existing 64 KiB row limit.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn validate_planning_settlement_size(value: &Settlement) -> Result<(), CustodyError> {
    bounded_audit_sha256(value).map(|_| ())
}

pub(crate) fn validate_engine_task_observed_v2(
    plan: &FixedPlanningEngineTaskV1,
    observed: &FixedPlanningEngineTaskObservedV2,
) -> Result<(), CustodyError> {
    if observed.member_id != plan.member_id
        || observed.plan_sha256 != engine_task_plan_sha256(plan)?
        || usize::from(observed.used_file_slots) > plan.file_slots.len()
        || observed.used_file_slots == 0
        || observed.media_digest.len() != 64
        || !observed
            .media_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(codec::invalid());
    }
    Ok(())
}

pub(crate) fn validate_engine_task(
    task: &FixedPlanningEngineTaskV1,
    manifest: &Manifest,
) -> Result<(), CustodyError> {
    if task.version != 1
        || task.version_root_member_id == task.dependency_root_member_id
        || task.version_close_operation_id == task.dependency_close_operation_id
        || task.file_slots.len() != FIXED_PLANNING_ENGINE_MAX_OBJECTS
        || task.configured_ffmpeg_locator.is_empty()
        || task.dependency_tool_locator.is_empty()
        || task.configured_ffmpeg_locator.contains('\0')
        || task.dependency_tool_locator.contains('\0')
    {
        return Err(codec::invalid());
    }
    codec::long_text(&task.configured_ffmpeg_locator)?;
    codec::long_text(&task.dependency_tool_locator)?;
    for root in [task.version_root_member_id, task.dependency_root_member_id] {
        if !manifest.members.iter().any(|member| {
            matches!(member,
            MemberPlan::LegacyV1 { planned }
                if planned.member_id == root.uuid()
                    && planned.kind == MemberKind::Root
                    && planned.adapter == super::super::Adapter::ProcessRoot)
        }) {
            return Err(codec::invalid());
        }
    }
    let file_ids: BTreeSet<_> = task.file_slots.iter().map(|slot| slot.owner_id).collect();
    let close_ids: BTreeSet<_> = task
        .file_slots
        .iter()
        .map(|slot| slot.close_operation_id)
        .chain([
            task.version_close_operation_id,
            task.dependency_close_operation_id,
            task.version_stdout_close_operation_id,
            task.version_diagnostic_close_operation_id,
            task.dependency_stdout_close_operation_id,
            task.dependency_diagnostic_close_operation_id,
            task.hash_buffer_close_operation_id,
        ])
        .collect();
    if file_ids.len() != task.file_slots.len() || close_ids.len() != task.file_slots.len() + 7 {
        return Err(codec::invalid());
    }
    task.inventory.validate()?;
    let pending: BTreeSet<_> = task
        .inventory
        .pending_io
        .iter()
        .map(|owner| owner.owner_id)
        .collect();
    let readers: BTreeSet<_> = task
        .inventory
        .readers
        .iter()
        .map(|owner| owner.owner_id)
        .collect();
    if pending != file_ids
        || task.inventory.pending_io.len() != task.file_slots.len()
        || readers
            != [
                task.version_stdout_owner_id,
                task.version_diagnostic_owner_id,
                task.dependency_stdout_owner_id,
                task.dependency_diagnostic_owner_id,
            ]
            .into_iter()
            .collect()
        || task.inventory.readers.len() != 4
        || task.inventory.buffers.len() != 1
        || task.inventory.buffers[0].owner_id != task.hash_buffer_owner_id
    {
        return Err(codec::invalid());
    }
    Ok(())
}

pub(crate) fn validate_engine_task_observed(
    plan: &FixedPlanningEngineTaskV1,
    observed: &FixedPlanningEngineTaskObservedV1,
) -> Result<(), CustodyError> {
    if observed.task != *plan
        || usize::from(observed.used_file_slots) > plan.file_slots.len()
        || observed.used_file_slots == 0
        || observed.media_digest.len() != 64
        || !observed
            .media_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(codec::invalid());
    }
    Ok(())
}

pub(crate) fn validate_discover_task(
    task: &FixedProbeDiscoverTaskV1,
    manifest: &Manifest,
) -> Result<(), CustodyError> {
    if task.version != 1
        || task.version_root_member_id == task.document_root_member_id
        || task.version_close_operation_id == task.document_close_operation_id
        || task.configured_locator.contains('\0')
    {
        return Err(codec::invalid());
    }
    for text in [
        &task.media_file_id,
        &task.recipe_source_binding,
        &task.configured_locator,
    ] {
        codec::long_text(text)?;
        if text.is_empty() {
            return Err(codec::invalid());
        }
    }
    for wanted in [task.version_root_member_id, task.document_root_member_id] {
        if !manifest.members.iter().any(|member| {
            matches!(member,
            MemberPlan::LegacyV1 { planned }
                if planned.member_id == wanted.uuid()
                    && planned.kind == MemberKind::Root
                    && planned.adapter == super::super::Adapter::ProcessRoot)
        }) {
            return Err(codec::invalid());
        }
    }
    let source = manifest.members.iter().any(|member| {
        matches!(member,
        MemberPlan::FixedProbeDiscoverSourceV1 { slot, source }
            if slot.member_id == task.source_member_id
                && source.media_file_id == task.media_file_id
                && source.recipe_source_binding == task.recipe_source_binding
                && source.planned_copies.iter().any(|copy| matches!(copy.owner,
                    CopyOwner::OwnedRoot { root_member_id, .. }
                        if root_member_id == task.document_root_member_id)))
    });
    if !source {
        return Err(codec::invalid());
    }
    task.inventory.validate()?;
    let close_ids: BTreeSet<_> = [
        task.version_close_operation_id,
        task.document_close_operation_id,
        task.image_close_operation_id,
        task.snapshot_close_operation_id,
        task.source_child_close_operation_id,
        task.version_stdout_close_operation_id,
        task.stdout_close_operation_id,
        task.diagnostic_close_operation_id,
    ]
    .into_iter()
    .collect();
    if close_ids.len() != 8 {
        return Err(codec::invalid());
    }
    let actual: BTreeSet<_> = task
        .inventory
        .pending_io
        .iter()
        .chain(&task.inventory.readers)
        .map(|owner| owner.owner_id)
        .collect();
    let required: BTreeSet<_> = [
        task.image_owner_id,
        task.snapshot_owner_id,
        task.source_child_owner_id,
        task.version_stdout_owner_id,
        task.stdout_owner_id,
        task.diagnostic_owner_id,
    ]
    .into_iter()
    .collect();
    if task.inventory.pending_io.len() != 3
        || !task.inventory.buffers.is_empty()
        || task.inventory.readers.len() != 3
        || actual != required
    {
        return Err(codec::invalid());
    }
    Ok(())
}

pub(crate) fn validate_discover_task_observed(
    plan: &FixedProbeDiscoverTaskV1,
    observed: &FixedProbeDiscoverTaskObservedV1,
) -> Result<(), CustodyError> {
    if observed.task != *plan || observed.executable_object_version.is_empty() {
        return Err(codec::invalid());
    }
    codec::long_text(&observed.executable_object_version)?;
    for digest in [&observed.executable_sha256, &observed.build_digest] {
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(codec::invalid());
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FixedProbeMetadataV1 {
    pub version: u32,
    pub member_id: Id,
    pub metadata_id: Id,
    /// Retained upper bound for the actual document, facts, key and aliases.
    /// A charge is not evidence that an allocation has been released.
    pub retained_memory_bytes: u64,
}

pub(crate) fn validate_task(
    task: &FixedProbeTaskV1,
    manifest: &Manifest,
) -> Result<(), CustodyError> {
    if task.version != 1 || task.stdout_owner_id == task.diagnostic_owner_id {
        return Err(codec::invalid());
    }
    codec::long_text(&task.media_file_id)?;
    codec::long_text(&task.recipe_source_binding)?;
    codec::long_text(&task.sealed_probe_digest)?;
    if task.sealed_probe_digest.len() != 64
        || !task
            .sealed_probe_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(codec::invalid());
    }
    let root = manifest.members.iter().any(|member| {
        matches!(member, MemberPlan::LegacyV1 { planned }
            if planned.member_id == task.root_member_id.uuid()
                && planned.kind == MemberKind::Root
                && planned.adapter == super::super::Adapter::ProcessRoot)
    });
    let source = manifest.members.iter().any(|member| {
        matches!(member, MemberPlan::FixedProbeDiscoverSourceV1 { slot, source }
            if slot.member_id == task.source_member_id
                && source.media_file_id == task.media_file_id
                && source.recipe_source_binding == task.recipe_source_binding
                && source.planned_copies.iter().any(|copy| matches!(copy.owner,
                    CopyOwner::OwnedRoot { root_member_id, .. }
                        if root_member_id == task.root_member_id)))
    });
    let executable = manifest.members.iter().any(|member| {
        matches!(member, MemberPlan::SourceV1 { slot, .. }
            if slot.member_id == task.executable_member_id)
    });
    if !root || !source || !executable || task.source_member_id == task.executable_member_id {
        return Err(codec::invalid());
    }
    let owners: BTreeSet<_> = task
        .inventory
        .pending_io
        .iter()
        .chain(&task.inventory.buffers)
        .chain(&task.inventory.readers)
        .map(|owner| owner.owner_id)
        .collect();
    if task.inventory.pending_io.len() != 1
        || !task.inventory.buffers.is_empty()
        || task.inventory.readers.len() != 2
        || owners.len() != 3
        || !owners.contains(&task.stdout_owner_id)
        || !owners.contains(&task.diagnostic_owner_id)
    {
        return Err(codec::invalid());
    }
    Ok(())
}

pub(crate) fn validate_metadata(plan: &FixedProbeMetadataV1) -> Result<(), CustodyError> {
    if plan.version != 1 || plan.retained_memory_bytes == 0 {
        return Err(codec::invalid());
    }
    Ok(())
}

/// Scanner-bound selected source: no S1 exists until the original admitted
/// native open. The observed SourceObserved carries that exact S1 afterward.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FixedProbeDiscoverSourceV1 {
    pub version: u32,
    pub holder_id: Id,
    pub member_id: Id,
    pub media_file_id: String,
    pub recipe_source_binding: String,
    pub absolute_path: String,
    pub scanner_size: i64,
    pub scanner_mtime: i64,
    pub filesystem_domain: FilesystemDomain,
    pub planned_copies: Vec<SourceCopy>,
    pub recovery: SourceRecovery,
}

pub(crate) fn validate_discover_source(
    source: &FixedProbeDiscoverSourceV1,
    manifest: &Manifest,
) -> Result<(), CustodyError> {
    if source.version != 1
        || source.scanner_size < 0
        || source.planned_copies.len() != 2
        || !std::path::Path::new(&source.absolute_path).is_absolute()
    {
        return Err(codec::invalid());
    }
    for text in [
        &source.media_file_id,
        &source.recipe_source_binding,
        &source.absolute_path,
    ] {
        codec::long_text(text)?;
    }
    source.filesystem_domain.validate()?;
    validate_copies(&source.planned_copies)?;
    let SourceRecovery::LocalSourceHandlesV1 {
        owner_id,
        graph_id,
        filesystem_domain,
    } = &source.recovery;
    if *owner_id != manifest.owner_id
        || *graph_id != manifest.graph_id
        || source.filesystem_domain.daemon_owner_id != manifest.owner_id
        || filesystem_domain != &source.filesystem_domain
    {
        return Err(codec::invalid());
    }
    let parent = &source.planned_copies[0];
    let child = &source.planned_copies[1];
    if !matches!(parent.owner, CopyOwner::DaemonSourceCell {
        owner_id: copy_owner,
        graph_id: copy_graph,
        member_id,
    } if copy_owner == *owner_id && copy_graph == *graph_id && member_id == source.member_id)
        || parent.parent_copy_id.is_some()
        || child.parent_copy_id != Some(parent.copy_id)
        || !matches!(child.owner, CopyOwner::OwnedRoot { graph_id: copy_graph, .. }
            if copy_graph == *graph_id)
    {
        return Err(codec::invalid());
    }
    Ok(())
}
