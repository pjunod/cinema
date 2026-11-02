//! Pure, pre-entry P/M admission for the selected fixed Linux probe. The
//! configured program strings and scanner facts are plans, not open handles
//! or executable identity. Every native original owner is named before the
//! first per-start source or program lookup.
use super::super::NodeResourceRuntime;
use super::*;
use crate::adaptive_resources::runtime::operations::planning_collector::fixed_planning_native_profile;
use crate::adaptive_resources::runtime::operations::planning_settlement::planning_retained_structural_bytes;
use crate::adaptive_resources::runtime::operations::planning_tasks::planning_task_frame_bytes;
use crate::adaptive_resources::runtime::operations::plans::{
    FixedProbeDiscoverPlan, SourceCopyOwner, SourceCopyPlan, TypedGraphPlan, TypedMemberPlan,
};
use crate::adaptive_resources::runtime::operations::source_files::SourceOpenSpec;
use crate::adaptive_resources::{
    FamilyDemand, FamilyRole, FamilyRoleDemand, MemoryClaim, ResourceVector, WorkloadDemand,
};
use crate::process::observation::Metric;
use crate::resource_custody::{codec_v2 as v2, Adapter, MemberKind, PlannedMember, RecoveryMethod};
use std::time::Instant;

// Report admission quota, enforced against actual report capacities at
// install. Not an assertion of native peak RSS; P records UnknownPeak.
const REPORT_STORAGE_ALLOWANCE_BYTES: u64 = 256 * 1024 * 1024;

pub(in crate::adaptive_resources::runtime) struct PlanningTaskPlan {
    pub(in crate::adaptive_resources::runtime) source_member: Uuid,
    pub(in crate::adaptive_resources::runtime) source_spec: SourceOpenSpec,
    pub(in crate::adaptive_resources::runtime) probe_plan: v2::FixedProbeDiscoverTaskV1,
    pub(in crate::adaptive_resources::runtime) engine_plan: v2::FixedPlanningEngineTaskV1,
}

// All callers in this module precede the single initialized preparation
// below. Its returned errors must retain their own post-reservation meaning.
fn invalid() -> OperationError {
    OperationError::BeforeReservation(super::super::operations::PreReservationRefusal::Admission(
        crate::adaptive_resources::AdmissionRefusal::InvalidBundle,
    ))
}
fn before_reservation_error(error: OperationError) -> OperationError {
    match error {
        OperationError::BeforeReservation(_) => error,
        _ => invalid(),
    }
}
fn id() -> Result<v2::Id, OperationError> {
    v2::Id::new(Uuid::new_v4()).map_err(|_| invalid())
}
fn add(values: impl IntoIterator<Item = u64>) -> Result<u64, OperationError> {
    values.into_iter().try_fold(0u64, |sum, value| {
        sum.checked_add(value).ok_or_else(invalid)
    })
}
fn multiply(value: u64, count: u64) -> Result<u64, OperationError> {
    value.checked_mul(count).ok_or_else(invalid)
}
fn allocation(owner_id: v2::Id, memory_bytes: u64, handles: u64) -> v2::OwnerAllocation {
    v2::OwnerAllocation {
        owner_id,
        memory_bytes,
        handles,
    }
}
fn slot(member_id: v2::Id, owned_by: Option<v2::Id>, depends_on: Vec<v2::Id>) -> v2::MemberSlot {
    v2::MemberSlot {
        member_id,
        owned_by,
        depends_on,
        protected_paths: vec![],
    }
}

pub(in crate::adaptive_resources::runtime) fn prepare(
    runtime: &Arc<NodeResourceRuntime>,
    request: FixedPlanningProbeRequest,
    deadline: Instant,
) -> Result<FixedPlanningProbeOwner, OperationError> {
    if !cfg!(target_os = "linux")
        || Instant::now() >= deadline
        || !request.source.path.is_absolute()
        || request.source.scanner_size < 0
        || request.source.media_file_id.is_empty()
        || request.recipe_source_binding.is_empty()
        || request.probe_program_locator.is_empty()
        || request.ffmpeg_program.is_empty()
        || request.application_metadata_bytes == 0
        || request.source.media_file_id.len() > 4096
        || request.recipe_source_binding.len() > 4096
        || request.probe_program_locator.len() > 4096
        || request.ffmpeg_program.len() > 4096
        || request.probe_program_locator.contains('\0')
        || request.ffmpeg_program.contains('\0')
    {
        return Err(invalid());
    }
    let path = request.source.path.to_str().ok_or_else(invalid)?;
    if path.len() > 4096 {
        return Err(invalid());
    }
    // This observation was captured at NodeResourceRuntime startup. A
    // per-request /proc stat here would itself be uncharged native planning.
    let mount_namespace = runtime.mount_namespace_inode.ok_or_else(invalid)?;
    let profile = fixed_planning_native_profile();
    let source_member = id()?;
    let probe_version_root = id()?;
    let probe_document_root = id()?;
    let engine_version_root = id()?;
    let engine_dependency_root = id()?;
    let source_parent_copy = id()?;
    let source_child_copy = id()?;
    let source_acquisition = id()?;
    let document_root_acquisition = id()?;
    let source = FixedProbeDiscoverPlan {
        holder_id: id()?,
        media_file_id: request.source.media_file_id.clone(),
        recipe_source_binding: request.recipe_source_binding.clone(),
        absolute_path: path.to_owned(),
        scanner_size: request.source.scanner_size,
        scanner_mtime: request.source.scanner_mtime,
        filesystem_domain: v2::FilesystemDomain {
            platform: v2::FilesystemPlatform::Linux,
            domain_id: id()?,
            daemon_owner_id: v2::Id::new(runtime.owner.owner_id).map_err(|_| invalid())?,
            os_boot: runtime.owner.os_boot.clone(),
            mount_namespace: v2::NativeValue::Known {
                value: mount_namespace,
            },
        },
        copies: vec![
            SourceCopyPlan {
                copy_id: source_parent_copy,
                owner: SourceCopyOwner::LocalSourceCell,
                acquisition_operation_id: source_acquisition,
                parent_copy_id: None,
            },
            SourceCopyPlan {
                copy_id: source_child_copy,
                owner: SourceCopyOwner::LocalRoot {
                    root_member_id: probe_document_root,
                },
                acquisition_operation_id: document_root_acquisition,
                parent_copy_id: Some(source_parent_copy),
            },
        ],
    };
    let source_spec = SourceOpenSpec {
        path: request.source.path.clone(),
        scanner_size: request.source.scanner_size,
        scanner_mtime: request.source.scanner_mtime,
        expected_sha256: None,
    };
    let probe_task_member = id()?;
    let image_owner = id()?;
    let snapshot_owner = id()?;
    let source_child_owner = id()?;
    let version_stdout_owner = id()?;
    let document_stdout_owner = id()?;
    let document_stderr_owner = id()?;
    let probe_plan = v2::FixedProbeDiscoverTaskV1 {
        version: 1,
        member_id: probe_task_member,
        source_member_id: source_member,
        version_root_member_id: probe_version_root,
        document_root_member_id: probe_document_root,
        version_close_operation_id: id()?,
        document_close_operation_id: id()?,
        media_file_id: request.source.media_file_id.clone(),
        recipe_source_binding: request.recipe_source_binding.clone(),
        configured_locator: request.probe_program_locator.clone(),
        image_owner_id: image_owner,
        image_close_operation_id: id()?,
        snapshot_owner_id: snapshot_owner,
        snapshot_close_operation_id: id()?,
        source_child_owner_id: source_child_owner,
        source_child_close_operation_id: id()?,
        version_stdout_owner_id: version_stdout_owner,
        version_stdout_close_operation_id: id()?,
        stdout_owner_id: document_stdout_owner,
        stdout_close_operation_id: id()?,
        diagnostic_owner_id: document_stderr_owner,
        diagnostic_close_operation_id: id()?,
        inventory: v2::IoInventory {
            pending_io: vec![
                allocation(image_owner, 0, 1),
                allocation(snapshot_owner, profile.max_probe_image_bytes, 1),
                allocation(source_child_owner, 0, 1),
            ],
            buffers: vec![],
            readers: vec![
                allocation(version_stdout_owner, 0, 1),
                allocation(document_stdout_owner, 0, 1),
                allocation(document_stderr_owner, 0, 1),
            ],
        },
    };
    let engine_task_member = id()?;
    let engine_version_stdout = id()?;
    let engine_version_stderr = id()?;
    let engine_dependency_stdout = id()?;
    let engine_dependency_stderr = id()?;
    let hash_buffer = id()?;
    let file_slots = (0..v2::FIXED_PLANNING_ENGINE_MAX_OBJECTS)
        .map(|_| {
            Ok(v2::FixedPlanningEngineFileSlotV1 {
                owner_id: id()?,
                close_operation_id: id()?,
            })
        })
        .collect::<Result<Vec<_>, OperationError>>()?;
    let engine_plan = v2::FixedPlanningEngineTaskV1 {
        version: 1,
        member_id: engine_task_member,
        version_root_member_id: engine_version_root,
        dependency_root_member_id: engine_dependency_root,
        version_close_operation_id: id()?,
        dependency_close_operation_id: id()?,
        configured_ffmpeg_locator: request.ffmpeg_program.clone(),
        dependency_tool_locator: "ldd".to_owned(),
        file_slots,
        version_stdout_owner_id: engine_version_stdout,
        version_stdout_close_operation_id: id()?,
        version_diagnostic_owner_id: engine_version_stderr,
        version_diagnostic_close_operation_id: id()?,
        dependency_stdout_owner_id: engine_dependency_stdout,
        dependency_stdout_close_operation_id: id()?,
        dependency_diagnostic_owner_id: engine_dependency_stderr,
        dependency_diagnostic_close_operation_id: id()?,
        hash_buffer_owner_id: hash_buffer,
        hash_buffer_close_operation_id: id()?,
        inventory: v2::IoInventory {
            pending_io: Vec::new(),
            buffers: vec![allocation(hash_buffer, profile.engine_hash_buffer_bytes, 0)],
            readers: vec![
                allocation(engine_version_stdout, 0, 1),
                allocation(engine_version_stderr, 0, 1),
                allocation(engine_dependency_stdout, 0, 1),
                allocation(engine_dependency_stderr, 0, 1),
            ],
        },
    };
    let mut engine_plan = engine_plan;
    engine_plan.inventory.pending_io = engine_plan
        .file_slots
        .iter()
        .map(|file| allocation(file.owner_id, 0, 1))
        .collect();

    // M charges the report quota (checked against actual capacities at
    // install), the application partition, and the structural bound of the
    // bookkeeping that survives P's refund (asserted before P settles). None
    // of these pretends to bound native parser or process RSS.
    let application_bytes =
        u64::try_from(request.application_metadata_bytes).map_err(|_| invalid())?;
    let book_structural_bytes = planning_retained_structural_bytes().ok_or_else(invalid)?;
    let metadata_bytes = add([
        REPORT_STORAGE_ALLOWANCE_BYTES,
        book_structural_bytes,
        application_bytes,
    ])?;
    usize::try_from(metadata_bytes).map_err(|_| invalid())?;
    let metadata_member = id()?;
    let metadata = v2::FixedProbeMetadataV1 {
        version: 1,
        member_id: metadata_member,
        metadata_id: id()?,
        retained_memory_bytes: metadata_bytes,
    };

    // File handles are charged for all prearmed slots, including dependency
    // slots that will later prove never entered. Kernel task peaks and native
    // producer memory remain explicitly unknown under P's serialized claim.
    let p_handles = add([
        profile.retained_parent_file_slots,
        profile.sealed_image_file_slots,
        profile.inherited_source_file_slots,
        profile.engine_file_slots,
        profile.engine_inherited_child_file_slots,
        profile.pipe_reader_slots,
        multiply(profile.original_root_slots, 2)?,
    ])?;
    let p_memory = add([
        profile.max_probe_image_bytes,
        profile.max_probe_version_stdout_bytes,
        profile.max_probe_document_stdout_bytes,
        profile.max_probe_document_stderr_bytes,
        multiply(profile.max_engine_command_output_bytes, 4)?,
        profile.engine_hash_buffer_bytes,
        profile.retained_custody_bytes,
        super::super::operations::planning_settlement::registered_planning_preflight_bytes()
            .ok_or_else(invalid)?,
        u64::try_from(request.requested_heap_bytes().ok_or_else(invalid)?)
            .map_err(|_| invalid())?,
        u64::try_from(std::mem::size_of::<FixedPlanningProbeRequest>()).map_err(|_| invalid())?,
        u64::try_from(planning_task_frame_bytes().ok_or_else(invalid)?).map_err(|_| invalid())?,
        u64::try_from(request.source.path.capacity()).map_err(|_| invalid())?,
        u64::try_from(request.source.media_file_id.capacity()).map_err(|_| invalid())?,
    ])?;
    let epoch = runtime
        .ledger
        .snapshot()
        .map_err(|_| invalid())?
        .limit_epoch;
    let roles = [
        FamilyRoleDemand {
            role: FamilyRole::Video,
            demand: WorkloadDemand {
                resources: Metric::Known(ResourceVector {
                    cpu_ns_per_second: 2_000_000_000,
                    peak_memory_bytes: p_memory,
                    handles: p_handles,
                    processes: profile.original_root_slots,
                    ..ResourceVector::default()
                }),
                limit_epoch: epoch,
                trial: false,
            },
            retained: None,
        },
        FamilyRoleDemand {
            role: FamilyRole::Companion,
            demand: WorkloadDemand {
                resources: Metric::Known(ResourceVector {
                    peak_memory_bytes: metadata_bytes,
                    ..ResourceVector::default()
                }),
                limit_epoch: epoch,
                trial: false,
            },
            retained: None,
        },
    ];
    let root = |member_id: v2::Id, locator: &str| {
        TypedMemberPlan::Legacy(PlannedMember {
            member_id: member_id.uuid(),
            kind: MemberKind::Root,
            adapter: Adapter::ProcessRoot,
            adapter_version: 1,
            domain: runtime.owner.daemon_process.domain.clone(),
            locator: locator.to_owned(),
            recovery: RecoveryMethod::LiveOnly {
                reason: "original fixed planning root requires live task custody".to_owned(),
            },
            owned_by: None,
            depends_on: vec![],
            maximum_cardinality: 1,
            protected_paths: vec![],
        })
    };
    let plans = vec![
        TypedGraphPlan {
            attempt_id: None,
            members: vec![
                root(probe_version_root, &request.probe_program_locator),
                root(probe_document_root, &request.probe_program_locator),
                root(engine_version_root, &request.ffmpeg_program),
                root(engine_dependency_root, "ldd"),
                TypedMemberPlan::FixedProbeDiscoverSource {
                    slot: v2::MemberSlot {
                        member_id: source_member,
                        owned_by: None,
                        depends_on: vec![],
                        protected_paths: vec![path.to_owned()],
                    },
                    source,
                },
                TypedMemberPlan::FixedProbeDiscoverTask {
                    slot: slot(
                        probe_task_member,
                        Some(probe_document_root),
                        vec![source_member, probe_version_root, probe_document_root],
                    ),
                    task: probe_plan.clone(),
                },
                TypedMemberPlan::FixedPlanningEngineTask {
                    slot: slot(
                        engine_task_member,
                        Some(engine_dependency_root),
                        vec![engine_version_root, engine_dependency_root],
                    ),
                    task: engine_plan.clone(),
                },
            ],
        },
        TypedGraphPlan {
            attempt_id: None,
            members: vec![TypedMemberPlan::FixedProbeMetadata {
                slot: slot(metadata_member, None, vec![]),
                metadata,
            }],
        },
    ];
    // Build the ORIGINAL registration exactly once, before reservation. It
    // owns the existing request/catalog/spec, plans and observation IDs; no
    // native task, source/program lookup or replacement ID is created here.
    let application_bytes = request.application_metadata_bytes;
    let tasks = super::super::operations::planning_tasks::PreparedPlanningTasks::new(
        PlanningTaskPlan {
            source_member: source_member.uuid(),
            source_spec,
            probe_plan,
            engine_plan,
        },
        request,
    );
    let family = runtime.new_family().map_err(before_reservation_error)?;
    match runtime.prepare_typed_families_claiming_initialized(
        &[FamilyDemand {
            identity: family,
            roles: &roles,
        }],
        plans,
        Some(&[
            MemoryClaim::UnknownProducerPeak,
            MemoryClaim::PlanningMetadata,
        ]),
        deadline,
        |preparation| {
            let (release, cleanup_requested, known_closed) = runtime.initialize_private_planning(
                preparation,
                application_bytes,
                book_structural_bytes,
                metadata_bytes,
                tasks,
            );
            // Complete the existing Drop-armed selector before publication.
            // Later unwinding signals this SAME original maintenance owner.
            FixedPlanningProbeOwner {
                runtime: Arc::clone(runtime),
                preparation: Some(preparation.clone()),
                release,
                cleanup_requested,
                known_closed,
            }
        },
    ) {
        Ok((_preparation, owner)) => Ok(owner),
        Err(error) => {
            let _ = runtime.remove_unused_family(family);
            Err(error)
        }
    }
}

#[cfg(test)]
pub(in crate::adaptive_resources::runtime) fn original_preparation_for_test(
    owner: &FixedPlanningProbeOwner,
) -> OwnedPreparation {
    owner
        .preparation
        .as_ref()
        .expect("original constructor selector")
        .clone()
}

#[cfg(all(test, target_os = "linux"))]
#[path = "plan/tests.rs"]
mod tests;
