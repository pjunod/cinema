//! Authored fixtures: compile now, execute only after complete-candidate review.
use super::*;
use crate::adaptive_resources::{ResourceVector, WorkloadDemand};

const WORK: ChildWork = ChildWork::background("original live Job closure fixture");

fn fixture() -> (ResourceLedger, OwnedPhysicalGraph) {
    let resources = ResourceVector {
        processes: 2,
        handles: 6,
        ..ResourceVector::default()
    };
    let ledger = ResourceLedger::new(1, Metric::Known(resources)).expect("ledger");
    let (reservation, _) = ledger
        .reserve(WorkloadDemand {
            resources: Metric::Known(resources),
            limit_epoch: 1,
            trial: false,
        })
        .expect("reserve");
    let graph = reservation.into_physical_graph().expect("exclusive graph");
    (ledger, graph)
}

fn child(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", "process::tests::child_main", "--nocapture"])
        .env("PLURX_CHILD_MODE", mode)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

#[tokio::test]
async fn empty_graph_never_mints_vacuous_live_process_evidence() {
    let (ledger, graph) = fixture();
    let sealed = graph.seal();
    assert!(sealed.try_live_process_closure().await.is_err());
    assert_eq!(sealed.close().await, GraphCloseOutcome::NoAcquisition);
    assert_eq!(ledger.snapshot().expect("snapshot").held, 0);
}

#[tokio::test]
async fn live_job_query_requires_actual_wait_not_termination_request() {
    let (ledger, mut graph) = fixture();
    let root = graph
        .spawn_root(&mut child("sleep"), WORK)
        .await
        .expect("root");
    let sealed = graph.seal();
    sealed.start_kill_root(root).await.expect("request only");
    assert_eq!(
        sealed
            .try_live_process_closure()
            .await
            .expect_err("not waited")
            .kind(),
        io::ErrorKind::WouldBlock
    );
    sealed.close().await;
    assert_eq!(ledger.snapshot().expect("not refunded").held, 1);
}

#[tokio::test]
async fn incomplete_late_launch_cannot_hide_behind_original_job_zero() {
    let (ledger, mut graph) = fixture();
    let root = graph
        .spawn_root(&mut child("echo"), WORK)
        .await
        .expect("root");
    let missing = tempfile::tempdir().expect("owned missing executable parent");
    let mut invalid = Command::new(missing.path().join("never-created-helper"));
    assert!(graph.spawn_root(&mut invalid, WORK).await.is_err());
    let sealed = graph.seal();
    sealed
        .wait_root(root)
        .await
        .expect("real successful root wait");
    let error = sealed
        .try_live_process_closure()
        .await
        .expect_err("ambiguous inventory");
    assert_eq!(
        error.to_string(),
        "sealed complete root inventory unavailable"
    );
    sealed.close().await;
    assert_eq!(ledger.snapshot().expect("unknown held").unknown_held, 1);
}

#[cfg(not(windows))]
#[tokio::test]
async fn unix_root_wait_is_not_original_job_or_descendant_extinction() {
    let (ledger, mut graph) = fixture();
    let root = graph
        .spawn_root(&mut child("echo"), WORK)
        .await
        .expect("root");
    let sealed = graph.seal();
    sealed.wait_root(root).await.expect("actual wait");
    assert_eq!(
        sealed
            .try_live_process_closure()
            .await
            .expect_err("unsupported")
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(
        sealed.close().await,
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    );
    assert_eq!(ledger.snapshot().expect("not refunded").held, 1);
}

#[cfg(windows)]
#[tokio::test]
async fn original_job_zero_is_graph_bound_process_only_evidence_not_refund() {
    let (ledger, mut graph) = fixture();
    let root = graph
        .spawn_root(&mut child("echo"), WORK)
        .await
        .expect("root");
    let sealed = graph.seal();
    sealed.wait_root(root).await.expect("actual wait");
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    let proof = loop {
        match sealed.try_live_process_closure().await {
            Ok(proof) => break proof,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(error) => panic!("no positive original Job query: {error}"),
        }
    };
    assert!(proof.matches(&sealed));
    assert_eq!(proof.roots(), &[root]);
    let (_, other) = fixture();
    assert!(!proof.matches(&other.seal()));
    assert_eq!(ledger.snapshot().expect("proof alone not refund").held, 1);
    assert_eq!(
        sealed.close().await,
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    );
    assert_eq!(ledger.snapshot().expect("no whole graph receipt").held, 1);
}

#[cfg(windows)]
#[tokio::test]
async fn exited_leader_job_descendant_remains_active_until_original_job_cleanup() {
    let (ledger, mut graph) = fixture();
    let directory = tempfile::tempdir().expect("owned fixture directory");
    let mut command = child("live-job-descendant");
    command.env(
        "PLURX_CHILD_PID_FILE",
        directory.path().join("descendant.pid"),
    );
    let root = graph
        .spawn_root(&mut command, WORK)
        .await
        .expect("contained root");
    let sealed = graph.seal();
    assert!(sealed
        .wait_root(root)
        .await
        .expect("leader exits")
        .success());
    assert_eq!(
        sealed
            .try_live_process_closure()
            .await
            .expect_err("descendant still active")
            .kind(),
        io::ErrorKind::WouldBlock
    );
    // close must request the retained Job even though the leader is waited.
    // This return remains whole-graph UnknownHeld, not a fabricated receipt.
    sealed.close().await;
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match sealed.try_live_process_closure().await {
            Ok(proof) => {
                assert!(proof.matches(&sealed));
                assert_eq!(proof.roots(), &[root]);
                break;
            }
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(error) => panic!("retained Job did not become empty: {error}"),
        }
    }
    assert_eq!(ledger.snapshot().expect("not whole-graph refund").held, 1);
}
