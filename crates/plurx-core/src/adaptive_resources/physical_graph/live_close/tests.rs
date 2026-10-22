//! Real I/O/task fixtures authored for post-complete-review execution only.
use super::*;
use crate::adaptive_resources::{CleanupAuthority, ResourceVector, WorkloadDemand};

const WORK: ChildWork = ChildWork::background("restricted live graph closure fixture");

fn resources() -> ResourceVector {
    ResourceVector {
        cpu_ns_per_second: 1,
        peak_memory_bytes: pipe_drains::TASK_BYTES + 1024,
        handles: 4,
        processes: 1,
        ..ResourceVector::default()
    }
}

fn fixture(cost: ResourceVector) -> (ResourceLedger, CleanupAuthority, OwnedPhysicalGraph) {
    let ledger = ResourceLedger::new(1, Metric::Known(cost)).expect("ledger");
    let (reservation, cleanup) = ledger
        .reserve(WorkloadDemand {
            resources: Metric::Known(cost),
            limit_epoch: 1,
            trial: false,
        })
        .expect("reserve");
    let graph = reservation
        .into_physical_graph()
        .expect("exclusive custody");
    (ledger, cleanup, graph)
}

fn child(mode: &str, pipes: bool) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", "process::tests::child_main", "--nocapture"])
        .env("PLURX_CHILD_MODE", mode)
        .stdin(std::process::Stdio::null())
        .stdout(if pipes {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stderr(if pipes {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        });
    command
}

#[tokio::test]
async fn unacquired_graph_has_no_vacuous_live_close_receipt() {
    let (ledger, _, graph) = fixture(resources());
    let sealed = graph.seal();
    assert!(sealed.try_live_close_receipt().await.is_err());
    assert_eq!(sealed.close().await, GraphCloseOutcome::NoAcquisition);
    assert_eq!(ledger.snapshot().expect("snapshot").held, 0);
}

#[cfg(not(windows))]
#[tokio::test]
async fn actual_unix_wait_and_pipe_disposal_cannot_mint_live_receipt() {
    let (ledger, _, mut graph) = fixture(resources());
    graph
        .spawn_root(&mut child("echo", true), WORK)
        .await
        .expect("root");
    let sealed = graph.seal();
    assert_eq!(
        sealed
            .try_live_close_receipt()
            .await
            .expect_err("no native containment")
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(ledger.snapshot().expect("retained").unknown_held, 1);
    assert!(!sealed.custody.members.lock().await.receipt_issued);
}

#[tokio::test]
async fn generic_writer_join_does_not_requalify_unaudited_handoff() {
    let (ledger, _, mut graph) = fixture(resources());
    let root = graph
        .spawn_root(&mut child("echo", true), WORK)
        .await
        .expect("root");
    graph
        .spawn_root_writer(root, |pipes| async move {
            drop(pipes);
        })
        .await
        .expect("generic task");
    let sealed = graph.seal();
    sealed.close().await;
    assert_eq!(sealed.snapshot().await.joined_writers, 1);
    assert!(sealed.try_live_close_receipt().await.is_err());
    assert_eq!(ledger.snapshot().expect("held").held, 1);
}

#[tokio::test]
async fn panicking_writer_factory_cannot_erase_export_window() {
    let (ledger, _, mut graph) = fixture(resources());
    let root = graph
        .spawn_root(&mut child("sleep", true), WORK)
        .await
        .expect("root");
    let failure = tokio::spawn(async move {
        graph
            .spawn_root_writer(root, |_pipes| -> std::future::Ready<()> {
                panic!("factory failed before task registration")
            })
            .await
    })
    .await
    .expect_err("real factory panic");
    assert!(failure.is_panic());
    let mut retained = ledger
        .retained_physical_graphs()
        .expect("same graph retained");
    let sealed = retained.pop().expect("sealed by real owner Drop");
    {
        let members = sealed.custody.members.lock().await;
        assert!(members.unaudited_handoff);
        assert!(
            members.writers.is_empty(),
            "no JoinHandle existed to record the escape"
        );
    }
    assert!(sealed.try_live_close_receipt().await.is_err());
    sealed.close().await;
    assert_eq!(ledger.snapshot().expect("no refund").held, 1);
}

#[tokio::test]
async fn incomplete_launch_inventory_refuses_whole_graph_receipt() {
    let (ledger, _, mut graph) = fixture(resources());
    let directory = tempfile::tempdir().expect("owned fixture");
    assert!(graph
        .spawn_root(&mut Command::new(directory.path().join("absent")), WORK)
        .await
        .is_err());
    let sealed = graph.seal();
    assert!(sealed.try_live_close_receipt().await.is_err());
    sealed.close().await;
    assert_eq!(ledger.snapshot().expect("uncertain acquisition").held, 1);
}

#[tokio::test]
async fn local_process_cleanup_cannot_refund_external_resource_dimensions() {
    for dimension in 0..3 {
        let mut cost = resources();
        match dimension {
            0 => cost.hardware_slots = 1,
            1 => cost.tuner_slots = 1,
            _ => cost.scratch_bytes = 1,
        }
        let (ledger, _, mut graph) = fixture(cost);
        graph
            .spawn_root(&mut child("echo", false), WORK)
            .await
            .expect("root");
        let sealed = graph.seal();
        assert_eq!(
            sealed
                .try_live_close_receipt()
                .await
                .expect_err("external owner has no adapter")
                .kind(),
            io::ErrorKind::Unsupported
        );
        sealed.close().await;
        assert_eq!(ledger.snapshot().expect("full charge held").used, cost);
    }
}

#[tokio::test]
async fn local_inventory_requires_real_source_operation_and_close_join() {
    let (_, _, mut graph) = fixture(resources());
    let directory = tempfile::tempdir().expect("owned source");
    let path = directory.path().join("source");
    std::fs::write(&path, b"owned source bytes").expect("fixture bytes");
    graph
        .open_source_file(&path)
        .await
        .expect("actual graph-owned file");
    let sealed = graph.seal();
    {
        let members = sealed.custody.members.lock().await;
        assert_eq!(
            check_local_owners_closed(sealed.custody.identity.generation, &members)
                .expect_err("still open")
                .kind(),
            io::ErrorKind::WouldBlock
        );
    }
    sealed.close().await;
    let members = sealed.custody.members.lock().await;
    check_local_owners_closed(sealed.custody.identity.generation, &members)
        .expect("actual source close joined");
    assert!(
        !members.receipt_issued,
        "local handles alone are not whole graph proof"
    );
}

#[tokio::test]
async fn returned_source_buffer_cannot_outlive_untracked_graph_refund() {
    let (ledger, _, mut graph) = fixture(resources());
    let directory = tempfile::tempdir().expect("owned source");
    let path = directory.path().join("source");
    let expected = b"retained copied bytes";
    std::fs::write(&path, expected).expect("fixture");
    let source = graph.open_source_file(&path).await.expect("source");
    let bytes = graph
        .read_source_file_at(source, 0, expected.len())
        .await
        .expect("copied output");
    graph
        .spawn_root(&mut child("echo", false), WORK)
        .await
        .expect("root");
    let sealed = graph.seal();
    sealed.close().await;
    assert!(sealed.custody.members.lock().await.unaudited_handoff);
    assert_eq!(bytes, expected);
    assert!(sealed.try_live_close_receipt().await.is_err());
    drop(bytes);
    assert!(
        sealed.try_live_close_receipt().await.is_err(),
        "no unaudited caller-drop acknowledgement"
    );
    assert_eq!(ledger.snapshot().expect("held").held, 1);
}

#[tokio::test]
async fn audited_task_abort_is_not_joined_local_inventory() {
    let (_, _, mut graph) = fixture(resources());
    let root = graph
        .spawn_root(&mut child("sleep", true), WORK)
        .await
        .expect("root");
    // Unit-only arm fixture exercises real pipes/tasks, NOT durable commit proof.
    let arm = super::super::super::runtime::DurableArm::adapter_fixture(
        graph.custody().binding,
        graph.identity().generation,
    );
    graph
        .spawn_durable_pipe_drain(root, Uuid::new_v4(), arm)
        .await
        .expect("audited real task");
    let sealed = graph.seal();
    {
        let members = sealed.custody.members.lock().await;
        members.writers[0].task.abort();
        assert_eq!(
            check_local_owners_closed(sealed.custody.identity.generation, &members)
                .expect_err("request not join")
                .kind(),
            io::ErrorKind::WouldBlock
        );
    }
    sealed.close().await;
    let members = sealed.custody.members.lock().await;
    check_local_owners_closed(sealed.custody.identity.generation, &members)
        .expect("actual waits and audited join");
    assert!(!members.receipt_issued);
}

#[cfg(windows)]
async fn native_receipt(sealed: &SealedPhysicalGraph) -> PhysicalCloseReceipt {
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match sealed.try_live_close_receipt().await {
            Ok(receipt) => return receipt,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(error) => panic!("original native live graph did not close: {error}"),
        }
    }
}

#[cfg(windows)]
#[tokio::test]
async fn native_job_and_unclaimed_pipes_mint_only_one_bound_receipt() {
    let (ledger, cleanup, mut graph) = fixture(resources());
    graph
        .spawn_root(&mut child("echo", true), WORK)
        .await
        .expect("contained root");
    let sealed = graph.seal();
    let receipt = native_receipt(&sealed).await;
    assert_eq!(receipt.custody_binding(), sealed.custody.binding);
    {
        let members = sealed.custody.members.lock().await;
        assert!(members.receipt_issued);
        assert!(
            members.roots.is_empty(),
            "native Child and Job owners dropped, not just pipes"
        );
    }
    assert!(sealed.try_live_close_receipt().await.is_err());
    assert_eq!(
        ledger.snapshot().expect("proof alone is not refund").held,
        1
    );
    cleanup.close(receipt).expect("original matching owner");
    assert_eq!(ledger.snapshot().expect("one refund").held, 0);
}

#[cfg(windows)]
#[tokio::test]
async fn native_job_audited_task_and_source_compose_without_synthetic_proof() {
    let (ledger, cleanup, mut graph) = fixture(resources());
    let directory = tempfile::tempdir().expect("owned fixture");
    let path = directory.path().join("source");
    std::fs::write(&path, b"source").expect("bytes");
    graph
        .open_source_file(&path)
        .await
        .expect("original source");
    let root = graph
        .spawn_root(&mut child("sleep", true), WORK)
        .await
        .expect("contained root");
    let arm = super::super::super::runtime::DurableArm::adapter_fixture(
        graph.custody().binding,
        graph.identity().generation,
    );
    graph
        .spawn_durable_pipe_drain(root, Uuid::new_v4(), arm)
        .await
        .expect("real audited pipe task");
    let sealed = graph.seal();
    let receipt = native_receipt(&sealed).await;
    let snapshot = sealed.snapshot().await;
    assert_eq!(
        snapshot.roots, 0,
        "waited native root/Job handles also disposed"
    );
    assert_eq!(snapshot.joined_writers, 1);
    assert_eq!(snapshot.closed_source_files, 1);
    assert_eq!(ledger.snapshot().expect("not yet refunded").held, 1);
    cleanup
        .close(receipt)
        .expect("complete original owner proof");
    assert_eq!(ledger.snapshot().expect("refunded once").held, 0);
}

#[cfg(windows)]
#[tokio::test]
async fn live_native_receipt_cannot_bypass_durable_settlement() {
    let (ledger, cleanup, mut graph) = fixture(resources());
    graph
        .spawn_root(&mut child("echo", false), WORK)
        .await
        .expect("contained root");
    let sealed = graph.seal();
    // Fixture tag only; this is NOT a journal transaction/restart receipt.
    ledger
        .inner
        .lock()
        .expect("ledger")
        .entries
        .get_mut(&sealed.custody.binding.reservation)
        .expect("entry")
        .durable = Some(Uuid::new_v4());
    let receipt = native_receipt(&sealed).await;
    let rejected = cleanup
        .close(receipt)
        .expect_err("journal must commit first");
    assert_eq!(rejected.reason, AdmissionRefusal::InvalidTransition);
    assert_eq!(rejected.receipt.custody_binding(), sealed.custody.binding);
    assert_eq!(ledger.snapshot().expect("full hold").held, 1);
    assert!(
        sealed.try_live_close_receipt().await.is_err(),
        "original proof returned, not reminted"
    );
}
