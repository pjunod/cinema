//! Original parent closer survives cancelled waits. Compile-only until review.
use super::*;
use std::fs::File;
use std::sync::atomic::{AtomicUsize, Ordering};

fn held_parent(path: &Path) -> SourceCell {
    let mut cell = source_cell(
        path,
        filesystem_domain(v2::NativeValue::Unknown {
            reason: "native namespace is irrelevant to the retained closer fixture".into(),
        }),
    );
    cell.spec = None;
    cell.result = Some(SourceResult {
        file: Some(Arc::new(File::open(path).expect("original parent File"))),
        observed: None,
        direct_open_error: None,
        verified_sha256: None,
        verified_elf: false,
        verified_sha256_cached: false,
    });
    cell
}

fn direct_plan(cell: &mut SourceCell, path: &Path) {
    let v2::MemberPlan::SourceV1 { slot, source } = cell.plan.clone() else {
        panic!("source fixture");
    };
    let metadata = std::fs::metadata(path).expect("source metadata");
    cell.plan = v2::MemberPlan::DirectSourceV1 {
        slot,
        source: v2::DirectSourcePlanV1 {
            version: 1,
            holder_id: source.holder_id,
            member_id: source.member_id,
            media_file_id: source.source_association.media_file_id,
            recipe_source_binding: source.source_association.recipe_source_binding,
            provenance: v2::DirectSourceProvenanceV1::Local {
                authenticated_user_id: 1,
                file_id: 1,
            },
            read_domain_obligation_id: id(),
            absolute_path: path.to_string_lossy().into_owned(),
            scanner_size: metadata.len() as i64,
            scanner_mtime: scanner_mtime(&metadata),
            expectation: v2::DirectSourceExpectationV1::ScannerExact,
            access: source.access,
            filesystem_domain: source.filesystem_domain,
            planned_copies: source.planned_copies,
            recovery: source.recovery,
        },
    };
}

/// Register the SAME File behind a controllable wait. The task reports success
/// only after actually closing its original descriptor. No replacement close
/// or synthetic successful outcome is used by the fixture.
fn register_original_close(
    cell: &mut SourceCell,
) -> (
    tokio::sync::oneshot::Sender<()>,
    Arc<AtomicUsize>,
    tokio::task::Id,
) {
    let file = cell.result.as_mut().unwrap().file.take().unwrap();
    let file = Arc::try_unwrap(file).expect("sole original parent owner");
    let (enter, registered) = tokio::sync::oneshot::channel();
    let (release, wait_release) = tokio::sync::oneshot::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let actual_calls = Arc::clone(&calls);
    let task = tokio::spawn(async move {
        if registered.await.is_err() || wait_release.await.is_err() {
            return ParentCloseOutcome::NotEntered;
        }
        let fd = file.into_raw_fd();
        actual_calls.fetch_add(1, Ordering::SeqCst);
        // SAFETY: this task exclusively owns the original File, converts it
        // once, and performs exactly one close of that descriptor.
        if unsafe { libc::close(fd) } == 0 {
            ParentCloseOutcome::Closed
        } else {
            ParentCloseOutcome::Failed(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO),
            )
        }
    });
    let original_task = task.id();
    cell.parent_close = ParentCloseState::Registered {
        task,
        enter: Some(enter),
    };
    (release, calls, original_task)
}

fn assert_original_pending(cell: &SourceCell, task_id: tokio::task::Id, operation: Uuid) {
    let ParentCloseState::Registered { task, enter } = &cell.parent_close else {
        panic!("cancelled waiter must retain its original closer");
    };
    assert_eq!(task.id(), task_id);
    assert!(enter.is_none(), "original start latch was sent once");
    assert_eq!(cell.parent_close_operation_id(), operation);
    assert!(cell.result.as_ref().unwrap().file.is_none());
}

#[tokio::test]
async fn cancelled_direct_parent_close_resumes_original_job_and_never_closes_twice() {
    let directory = tempfile::tempdir().expect("durable fixture");
    let path = directory.path().join("source");
    std::fs::write(&path, b"actual original parent").unwrap();
    let mut cell = held_parent(&path);
    direct_plan(&mut cell, &path);
    let operation = cell.parent_close_operation_id();
    let (release, calls, original_task) = register_original_close(&mut cell);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), cell.close_direct_parent())
            .await
            .is_err()
    );
    assert_original_pending(&cell, original_task, operation);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    release.send(()).expect("release original closer");
    cell.close_direct_parent()
        .await
        .expect("resume original closer");
    assert!(cell.parent_closed());
    assert_eq!(cell.parent_close_operation_id(), operation);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    cell.close_direct_parent()
        .await
        .expect("idempotent positive closure");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn direct_parent_failed_or_unknown_closer_cannot_become_success_from_empty_file_slot() {
    let directory = tempfile::tempdir().expect("durable fixture");
    let path = directory.path().join("source");
    std::fs::write(&path, b"terminal close state fixture").unwrap();
    let mut cell = held_parent(&path);
    direct_plan(&mut cell, &path);
    drop(cell.result.as_mut().unwrap().file.take());
    let operation = cell.parent_close_operation_id();
    cell.parent_close = ParentCloseState::Failed(libc::EINTR);
    for _ in 0..2 {
        assert!(cell.close_direct_parent().await.is_err());
        assert!(matches!(
            cell.parent_close,
            ParentCloseState::Failed(libc::EINTR)
        ));
        assert_eq!(cell.parent_close_operation_id(), operation);
    }
    cell.parent_close = ParentCloseState::Unknown;
    for _ in 0..2 {
        assert!(cell.close_direct_parent().await.is_err());
        assert!(matches!(cell.parent_close, ParentCloseState::Unknown));
        assert_eq!(cell.parent_close_operation_id(), operation);
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancelled_never_root_parent_close_resumes_original_job_under_original_graph_proof() {
    use crate::adaptive_resources::{Metric, ResourceLedger, ResourceVector, WorkloadDemand};
    let directory = tempfile::tempdir().expect("durable fixture");
    let path = directory.path().join("source");
    std::fs::write(&path, b"original unspawned source").unwrap();
    let mut cell = held_parent(&path);
    let resources = ResourceVector {
        cpu_ns_per_second: 1,
        peak_memory_bytes: 1024,
        handles: 4,
        processes: 1,
        scratch_bytes: 1,
        ..ResourceVector::default()
    };
    let ledger = ResourceLedger::new(1, Metric::Known(resources)).unwrap();
    let (reservation, _cleanup) = ledger
        .reserve(WorkloadDemand {
            resources: Metric::Known(resources),
            limit_epoch: 1,
            trial: false,
        })
        .unwrap();
    let graph = reservation.into_physical_graph().unwrap();
    let identity = graph.identity();
    let root_member = id();
    let task_member = id();
    let acquisition = id();
    let v2::MemberPlan::SourceV1 { source, .. } = &mut cell.plan else {
        panic!("source fixture");
    };
    let graph_id = v2::Id::new(identity.generation).unwrap();
    let owner_id = v2::Id::new(identity.process_instance).unwrap();
    source.filesystem_domain.daemon_owner_id = owner_id;
    source.recovery = v2::SourceRecovery::LocalSourceHandlesV1 {
        graph_id,
        owner_id,
        filesystem_domain: source.filesystem_domain.clone(),
    };
    source.planned_copies[0].owner = v2::CopyOwner::DaemonSourceCell {
        owner_id,
        graph_id,
        member_id: source.member_id,
    };
    source.planned_copies.push(v2::SourceCopy {
        copy_id: id(),
        owner: v2::CopyOwner::OwnedRoot {
            graph_id,
            root_member_id: root_member,
        },
        acquisition_operation_id: acquisition,
        parent_copy_id: Some(source.planned_copies[0].copy_id),
    });
    let graph = graph.seal();
    let proof = graph
        .try_finite_vod_never_root_closure(
            root_member.uuid(),
            task_member.uuid(),
            acquisition.uuid(),
        )
        .await
        .expect("actual original sealed graph proves no root acquisition");
    let operation = cell.parent_close_operation_id();
    let (release, calls, original_task) = register_original_close(&mut cell);
    assert!(tokio::time::timeout(
        Duration::from_millis(10),
        cell.close_parent_before_root(&proof)
    )
    .await
    .is_err());
    assert_original_pending(&cell, original_task, operation);
    release.send(()).expect("release original closer");
    cell.close_parent_before_root(&proof)
        .await
        .expect("resume original closer");
    cell.close_parent_before_root(&proof)
        .await
        .expect("same proof, no replacement close");
    assert!(cell.parent_closed());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let _ = cell
        .finite_never_root_closed_fact(&proof)
        .expect("actual parent close plus never-acquired child");
    cell.parent_close = ParentCloseState::Unknown;
    assert!(cell.close_parent_before_root(&proof).await.is_err());
}
