//! Actual retained I/O/task mixed-budget contracts. COMPILED NOT RUN.
use super::*;
use crate::adaptive_resources::runtime::DurableArm;
use std::time::Duration;

const WORK: ChildWork = ChildWork::background("mixed graph budget fixture");

fn mixed_graph(handles: u64, memory: u64, processes: u64) -> (ResourceLedger, OwnedPhysicalGraph) {
    let resources = ResourceVector {
        handles,
        peak_memory_bytes: memory,
        processes,
        ..ResourceVector::default()
    };
    let ledger = ResourceLedger::new(1, Metric::Known(resources)).expect("ledger");
    let (reservation, _) = ledger
        .reserve(WorkloadDemand {
            limit_epoch: 1,
            resources: Metric::Known(resources),
            trial: false,
        })
        .expect("reserve");
    let graph = reservation.into_physical_graph().expect("actual owner");
    (ledger, graph)
}

fn command() -> Command {
    let mut command = Command::new(std::env::current_exe().expect("fixture executable"));
    command
        .args(["--exact", "process::tests::child_main", "--nocapture"])
        .env("PLURX_CHILD_MODE", "sleep")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command
}

fn arm(graph: &OwnedPhysicalGraph) -> DurableArm {
    DurableArm::adapter_fixture(graph.custody().binding, graph.identity().generation)
}

async fn pending_read(graph: &mut OwnedPhysicalGraph, source: GraphSourceFile, length: usize) {
    let read = graph.read_source_file_at(source, 0, length);
    tokio::pin!(read);
    std::future::poll_fn(|cx| {
        assert!(read.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    // Only the waiter is dropped. The real task and its allocation stay owned.
}

#[tokio::test]
async fn root_pipe_headroom_is_reserved_before_any_native_launch() {
    let (ledger, mut graph) = mixed_graph(2, 65536, 1);
    assert!(graph.spawn_root(&mut command(), WORK).await.is_err());
    let sealed = graph.seal();
    let snapshot = sealed.snapshot().await;
    assert_eq!(snapshot.roots, 0);
    assert!(!snapshot.acquired);
    assert_eq!(sealed.close().await, GraphCloseOutcome::NoAcquisition);
    assert_eq!(ledger.snapshot().expect("never acquired").held, 0);
}

#[tokio::test]
async fn source_first_cannot_borrow_the_same_handles_again_for_root_pipes() {
    let fixture = source();
    let (ledger, mut graph) = mixed_graph(3, 65536, 1);
    graph
        .open_source_file(fixture.path())
        .await
        .expect("source");
    assert!(graph.spawn_root(&mut command(), WORK).await.is_err());
    let sealed = graph.seal();
    assert_eq!(sealed.snapshot().await.roots, 0);
    assert_eq!(sealed.snapshot().await.source_files, 1);
    sealed.close().await;
    assert_eq!(ledger.snapshot().expect("source remains charged").held, 1);
}

#[tokio::test]
async fn root_first_source_acquisition_observes_combined_handle_ceiling() {
    let fixture = source();
    let (_, mut graph) = mixed_graph(4, 65536, 1);
    graph.spawn_root(&mut command(), WORK).await.expect("root");
    graph
        .open_source_file(fixture.path())
        .await
        .expect("fourth slot");
    assert!(graph.open_source_file(fixture.path()).await.is_err());
    let sealed = graph.seal();
    assert_eq!(sealed.snapshot().await.source_files, 1);
    assert_eq!(
        sealed.close().await,
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    );
}

#[tokio::test]
async fn cancelled_read_allocation_blocks_drain_until_exact_result_is_consumed() {
    let fixture = source();
    let (ledger, mut graph) = mixed_graph(5, 20000, 1);
    let root = graph.spawn_root(&mut command(), WORK).await.expect("root");
    let source = graph
        .open_source_file(fixture.path())
        .await
        .expect("source");
    let (release, thread) = hold_file(&graph, source).await;
    pending_read(&mut graph, source, 4000).await;
    let refused = graph
        .spawn_durable_pipe_drain(root, Uuid::new_v4(), arm(&graph))
        .await;
    release.send(()).expect("release actual I/O");
    thread.join().expect("holder joined");
    assert!(refused.is_err());
    assert!(graph.custody().members.lock().await.writers.is_empty());
    assert_eq!(
        graph
            .read_source_file_at(source, 0, 4000)
            .await
            .expect("same read"),
        b"0123456789"
    );
    graph
        .spawn_durable_pipe_drain(root, Uuid::new_v4(), arm(&graph))
        .await
        .expect("consumed source result no longer retained");
    graph.seal().close().await;
    assert_eq!(ledger.snapshot().expect("no physical refund").held, 1);
}

#[tokio::test]
async fn drain_first_read_refuses_before_creating_a_blocking_io_task() {
    let fixture = source();
    let (_, mut graph) = mixed_graph(5, 20000, 1);
    let root = graph.spawn_root(&mut command(), WORK).await.expect("root");
    let source = graph
        .open_source_file(fixture.path())
        .await
        .expect("source");
    graph
        .spawn_durable_pipe_drain(root, Uuid::new_v4(), arm(&graph))
        .await
        .expect("fixed drain buffers fit");
    assert!(graph.read_source_file_at(source, 0, 4000).await.is_err());
    assert!(
        graph.custody().members.lock().await.source_files[source.index]
            .pending
            .is_none()
    );
    assert_eq!(
        graph
            .read_source_file_at(source, 0, 3000)
            .await
            .expect("combined headroom"),
        b"0123456789"
    );
    graph.seal().close().await;
}

#[tokio::test]
async fn completed_unconsumed_read_still_occupies_its_allocation() {
    let fixture = source();
    let (_, mut graph) = mixed_graph(5, 20000, 1);
    let root = graph.spawn_root(&mut command(), WORK).await.expect("root");
    let source = graph
        .open_source_file(fixture.path())
        .await
        .expect("source");
    let (release, thread) = hold_file(&graph, source).await;
    pending_read(&mut graph, source, 4000).await;
    release.send(()).expect("release actual I/O");
    thread.join().expect("holder joined");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if graph.custody().members.lock().await.source_files[source.index]
                .pending
                .as_ref()
                .expect("unconsumed result")
                .task
                .is_finished()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual blocking task completed");
    assert!(graph
        .spawn_durable_pipe_drain(root, Uuid::new_v4(), arm(&graph))
        .await
        .is_err());
    assert_eq!(
        graph.custody().members.lock().await.source_files[source.index].retained_read_bytes(),
        4000
    );
    graph
        .read_source_file_at(source, 0, 4000)
        .await
        .expect("consume original Vec");
    graph
        .spawn_durable_pipe_drain(root, Uuid::new_v4(), arm(&graph))
        .await
        .expect("only consumption makes room");
    graph.seal().close().await;
}

#[tokio::test]
async fn multiple_drains_and_pending_source_reads_share_one_memory_charge() {
    let fixture = source();
    let (_, mut graph) = mixed_graph(8, pipe_drains::TASK_BYTES * 2 + 2000, 2);
    let first = graph
        .spawn_root(&mut command(), WORK)
        .await
        .expect("first root");
    let second = graph
        .spawn_root(&mut command(), WORK)
        .await
        .expect("second root");
    let source = graph
        .open_source_file(fixture.path())
        .await
        .expect("seventh handle");
    graph
        .spawn_durable_pipe_drain(first, Uuid::new_v4(), arm(&graph))
        .await
        .expect("first drain");
    let (release, thread) = hold_file(&graph, source).await;
    pending_read(&mut graph, source, 4000).await;
    let refused = graph
        .spawn_durable_pipe_drain(second, Uuid::new_v4(), arm(&graph))
        .await;
    release.send(()).expect("release I/O");
    thread.join().expect("holder joined");
    assert!(refused.is_err());
    graph
        .read_source_file_at(source, 0, 4000)
        .await
        .expect("consume read");
    graph
        .spawn_durable_pipe_drain(second, Uuid::new_v4(), arm(&graph))
        .await
        .expect("both buffers now fit");
    graph.seal().close().await;
}

#[tokio::test]
async fn generic_writer_handle_is_not_a_finite_memory_estimate_for_source_acquisition() {
    let fixture = source();
    let (_, mut graph) = mixed_graph(5, 65536, 1);
    let root = graph.spawn_root(&mut command(), WORK).await.expect("root");
    graph
        .spawn_root_writer(root, |pipes| async move {
            drop(pipes);
        })
        .await
        .expect("legacy non-certifying task");
    assert!(graph.open_source_file(fixture.path()).await.is_err());
    let sealed = graph.seal();
    assert_eq!(sealed.snapshot().await.source_files, 0);
    sealed.close().await;
}

#[tokio::test]
async fn combined_budget_overflow_refuses_without_mutating_acquisition_state() {
    let (ledger, graph) = mixed_graph(8, 65536, 1);
    let members = graph.custody().members.lock().await;
    for delta in [
        budgets::BudgetDelta {
            roots: u64::MAX,
            ..Default::default()
        },
        budgets::BudgetDelta {
            drains: u64::MAX,
            ..Default::default()
        },
    ] {
        assert!(budgets::check(&ledger, graph.custody(), &members, delta).is_err());
    }
    assert!(!members.acquired);
    drop(members);
    assert_eq!(graph.seal().close().await, GraphCloseOutcome::NoAcquisition);
}
