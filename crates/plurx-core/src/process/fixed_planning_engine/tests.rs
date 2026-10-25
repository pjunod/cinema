//! Synthetic token invariants only: no process, file, native closure or acceptance runs.
use super::*;
use crate::resource_custody::codec_v2 as v2;
use std::os::unix::process::ExitStatusExt;

pub(crate) enum SyntheticFault {
    None,
    UnusedCount,
    RootIdentity,
    PipeOverbound,
    HashBufferOverbound,
    FileOwner,
}

/// Test-only constructor under the private token owner. It is absent from
/// production and carries no claim of real native execution or closure.
pub(crate) fn synthetic_completion(
    used: usize,
    fault: SyntheticFault,
) -> (FixedEngineNativeCompletion, FixedPlanningEngineTaskV1) {
    assert!((1..=FIXED_PLANNING_ENGINE_MAX_OBJECTS).contains(&used));
    let id = || v2::Id::new(Uuid::new_v4()).unwrap();
    let mut task = FixedPlanningEngineTaskV1 {
        version: 1,
        member_id: id(),
        version_root_member_id: id(),
        dependency_root_member_id: id(),
        version_close_operation_id: id(),
        dependency_close_operation_id: id(),
        configured_ffmpeg_locator: "ffmpeg".into(),
        dependency_tool_locator: "ldd".into(),
        file_slots: (0..FIXED_PLANNING_ENGINE_MAX_OBJECTS)
            .map(|_| v2::FixedPlanningEngineFileSlotV1 {
                owner_id: id(),
                close_operation_id: id(),
            })
            .collect(),
        version_stdout_owner_id: id(),
        version_stdout_close_operation_id: id(),
        version_diagnostic_owner_id: id(),
        version_diagnostic_close_operation_id: id(),
        dependency_stdout_owner_id: id(),
        dependency_stdout_close_operation_id: id(),
        dependency_diagnostic_owner_id: id(),
        dependency_diagnostic_close_operation_id: id(),
        hash_buffer_owner_id: id(),
        hash_buffer_close_operation_id: id(),
        inventory: v2::IoInventory {
            pending_io: vec![],
            buffers: vec![],
            readers: vec![],
        },
    };
    let allocation = |owner_id| v2::OwnerAllocation {
        owner_id,
        memory_bytes: 4096,
        handles: 1,
    };
    task.inventory.pending_io = task
        .file_slots
        .iter()
        .map(|slot| allocation(slot.owner_id))
        .collect();
    task.inventory.buffers = vec![allocation(task.hash_buffer_owner_id)];
    task.inventory.readers = [
        task.version_stdout_owner_id,
        task.version_diagnostic_owner_id,
        task.dependency_stdout_owner_id,
        task.dependency_diagnostic_owner_id,
    ]
    .into_iter()
    .map(allocation)
    .collect();
    let pipe = |owner_id: v2::Id, close_operation_id: v2::Id| FixedEnginePipeClosed {
        owner_id: owner_id.uuid(),
        close_operation_id: close_operation_id.uuid(),
        drained_bytes: MAX_COMMAND_OUTPUT_BYTES,
    };
    let root = |close: v2::Id, stdout, stderr| FixedEngineRootClosed {
        identity: ProcessIdentity {
            pid: 42,
            start_marker: 7,
        },
        close_operation_id: close.uuid(),
        exit_status: std::process::ExitStatus::from_raw(0),
        stdout,
        stderr,
        session_closed: SessionClosed::OriginalGroupAlreadyAbsent,
        pidfd_closed: PidfdClosed,
        inherited_producer_file_owner_id: Some(task.file_slots[0].owner_id.uuid()),
    };
    let files = task
        .file_slots
        .iter()
        .take(used)
        .enumerate()
        .map(|(index, slot)| FixedEngineFileClosed {
            owner_id: slot.owner_id.uuid(),
            close_operation_id: slot.close_operation_id.uuid(),
            path: PathBuf::from(format!("/fixture/object-{index:03}")),
            object_version: "1:2:42:10:0:10:0".into(),
            sha256: [0; 32],
        })
        .collect::<Vec<_>>();
    let mut native = FixedEngineNativeCompletion {
        unused_slots: FixedEngineUnusedSlots {
            count: u16::try_from(task.file_slots.len() - used).unwrap(),
        },
        producer_path: files[0].path.clone(),
        producer_object_version: format!("s1:{}", files[0].object_version),
        producer_sha256: hex::encode(files[0].sha256),
        dependencies: files
            .iter()
            .map(|file| (file.path.clone(), file.object_version.clone()))
            .collect(),
        media_digest: "a".repeat(64),
        build_line: "synthetic token fixture".into(),
        boot_id: None,
        version_root: root(
            task.version_close_operation_id,
            pipe(
                task.version_stdout_owner_id,
                task.version_stdout_close_operation_id,
            ),
            pipe(
                task.version_diagnostic_owner_id,
                task.version_diagnostic_close_operation_id,
            ),
        ),
        dependency_root: root(
            task.dependency_close_operation_id,
            pipe(
                task.dependency_stdout_owner_id,
                task.dependency_stdout_close_operation_id,
            ),
            pipe(
                task.dependency_diagnostic_owner_id,
                task.dependency_diagnostic_close_operation_id,
            ),
        ),
        closed_files: files,
        hash_buffer: FixedEngineHashBufferClosed {
            owner_id: task.hash_buffer_owner_id.uuid(),
            close_operation_id: task.hash_buffer_close_operation_id.uuid(),
            capacity_bytes: HASH_BUFFER_BYTES,
        },
    };
    match fault {
        SyntheticFault::None => {}
        SyntheticFault::UnusedCount => {
            native.unused_slots.count = native.unused_slots.count.saturating_add(1)
        }
        SyntheticFault::RootIdentity => native.version_root.identity.start_marker = 0,
        SyntheticFault::PipeOverbound => {
            native.version_root.stdout.drained_bytes = MAX_COMMAND_OUTPUT_BYTES + 1
        }
        SyntheticFault::HashBufferOverbound => {
            native.hash_buffer.capacity_bytes = HASH_BUFFER_BYTES + 1
        }
        SyntheticFault::FileOwner => native.closed_files[0].owner_id = Uuid::new_v4(),
    }
    (native, task)
}

#[test]
fn synthetic_verified_tokens_reject_missing_identity_and_overbound_storage() {
    let (native, _) = synthetic_completion(2, SyntheticFault::None);
    assert!(native.version_root.verified());
    assert!(native.dependency_root.verified());
    assert!(native.hash_buffer.verified());
    assert_eq!(
        usize::from(native.unused_slots.count()),
        FIXED_PLANNING_ENGINE_MAX_OBJECTS - 2
    );
    let (native, _) = synthetic_completion(2, SyntheticFault::RootIdentity);
    assert!(!native.version_root.verified());
    let (native, _) = synthetic_completion(2, SyntheticFault::PipeOverbound);
    assert!(!native.version_root.verified());
    assert!(!native.version_root.stdout.verified());
    let (native, _) = synthetic_completion(2, SyntheticFault::HashBufferOverbound);
    assert!(!native.hash_buffer.verified());
}
