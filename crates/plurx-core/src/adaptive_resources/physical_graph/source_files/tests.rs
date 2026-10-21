use super::*;
use crate::adaptive_resources::{ResourceVector, WorkloadDemand};

fn graph(handles: u64, memory: u64) -> (ResourceLedger, OwnedPhysicalGraph) {
    let resources = ResourceVector {
        handles,
        peak_memory_bytes: memory,
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
    let graph = reservation.into_physical_graph().expect("graph");
    (ledger, graph)
}

fn source() -> tempfile::NamedTempFile {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new().expect("synthetic source");
    file.write_all(b"0123456789").expect("fixture bytes");
    file
}

#[tokio::test]
async fn source_reads_are_graph_bound_bounded_and_close_keeps_physical_hold() {
    let fixture = source();
    let (ledger, mut first) = graph(1, 64);
    let (_, mut second) = graph(1, 64);
    let source = first.open_source_file(fixture.path()).await.expect("open");
    assert_eq!(
        first.read_source_file_at(source, 3, 4).await.expect("read"),
        b"3456"
    );
    assert!(second.read_source_file_at(source, 0, 1).await.is_err());
    assert!(first.open_source_file(fixture.path()).await.is_err());
    assert!(first.read_source_file_at(source, 0, 65).await.is_err());
    assert!(first
        .read_source_file_at(source, u64::MAX, 1)
        .await
        .is_err());
    assert!(first.read_source_file_at(source, 0, 0).await.is_err());
    let sealed = first.seal();
    assert_eq!(sealed.snapshot().await.closed_source_files, 0);
    assert_eq!(
        sealed.close().await,
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    );
    assert_eq!(sealed.snapshot().await.closed_source_files, 1);
    assert_eq!(ledger.snapshot().expect("held").unknown_held, 1);
    assert_eq!(
        sealed.close().await,
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    );
}

#[tokio::test]
async fn source_buffer_budget_covers_all_cancelled_members_not_only_current_reader() {
    let fixture = source();
    let (_, mut graph) = graph(2, 6);
    let first = graph.open_source_file(fixture.path()).await.expect("first");
    let second = graph
        .open_source_file(fixture.path())
        .await
        .expect("second");
    assert!(graph.read_source_file_at(first, 0, 4).await.is_err());
    assert!(graph.read_source_file_at(second, 0, 4).await.is_err());
    assert_eq!(
        graph.read_source_file_at(first, 0, 3).await.expect("share"),
        b"012"
    );
    graph.seal().close().await;
}

/// A real thread holds the actual File cell, so a real spawned blocking read
/// cannot finish early. No fake completion sender or made-up closure proof.
async fn hold_file(
    graph: &OwnedPhysicalGraph,
    source: GraphSourceFile,
) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let file = Arc::clone(&graph.custody().members.lock().await.source_files[source.index].file);
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let _file = file.lock().expect("hold actual cell");
        started.send(()).expect("ready");
        wait.recv().expect("release");
    });
    ready.await.expect("thread holds actual File");
    (release, thread)
}

#[tokio::test]
async fn cancelled_source_read_retries_same_operation_without_second_io() {
    let fixture = source();
    let (_, mut graph) = graph(1, 64);
    let source = graph.open_source_file(fixture.path()).await.expect("open");
    let (release, thread) = hold_file(&graph, source).await;
    {
        let read = graph.read_source_file_at(source, 2, 3);
        tokio::pin!(read);
        std::future::poll_fn(|cx| {
            assert!(read.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        // Drop only the waiting future while actual I/O is still owned.
    }
    assert!(graph.read_source_file_at(source, 3, 3).await.is_err());
    {
        let members = graph.custody().members.lock().await;
        assert_eq!(members.source_files.len(), 1);
        assert_eq!(
            members.source_files[0]
                .pending
                .as_ref()
                .expect("same task")
                .kind,
            OperationKind::Read {
                offset: 2,
                length: 3
            }
        );
    }
    release.send(()).expect("release actual operation");
    thread.join().expect("joined holder");
    assert_eq!(
        graph
            .read_source_file_at(source, 2, 3)
            .await
            .expect("same read"),
        b"234"
    );
    graph.seal().close().await;
}

#[tokio::test]
async fn cancelled_source_closure_and_owner_drop_retain_actual_file_and_pending_io() {
    let fixture = source();
    let (ledger, mut graph) = graph(1, 64);
    let source = graph.open_source_file(fixture.path()).await.expect("open");
    let (release, thread) = hold_file(&graph, source).await;
    {
        let read = graph.read_source_file_at(source, 0, 1);
        tokio::pin!(read);
        std::future::poll_fn(|cx| {
            assert!(read.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }
    let sealed = graph.seal();
    {
        let close = sealed.close();
        tokio::pin!(close);
        std::future::poll_fn(|cx| {
            assert!(close.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }
    assert_eq!(sealed.snapshot().await.closed_source_files, 0);
    drop(sealed);
    let held = ledger.retained_physical_graphs().expect("same owner");
    assert_eq!(held.len(), 1);
    assert_eq!(ledger.snapshot().expect("charged").held, 1);
    release.send(()).expect("release operation");
    thread.join().expect("holder joined");
    assert_eq!(
        held[0].close().await,
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    );
    assert_eq!(held[0].snapshot().await.closed_source_files, 1);
    assert_eq!(ledger.snapshot().expect("not proof").unknown_held, 1);
}

#[tokio::test]
async fn failed_source_open_consumes_bounded_member_without_no_acquisition_refund() {
    let directory = tempfile::tempdir().expect("owned directory");
    let (ledger, mut graph) = graph(1, 64);
    assert!(graph
        .open_source_file(&directory.path().join("absent"))
        .await
        .is_err());
    assert!(graph
        .open_source_file(&directory.path().join("other"))
        .await
        .is_err());
    let sealed = graph.seal();
    let snapshot = sealed.snapshot().await;
    assert_eq!(snapshot.source_files, 1);
    assert!(snapshot.acquired);
    assert_eq!(
        sealed.close().await,
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    );
    assert_eq!(sealed.snapshot().await.closed_source_files, 1);
    assert_eq!(ledger.snapshot().expect("conservative").held, 1);
}
