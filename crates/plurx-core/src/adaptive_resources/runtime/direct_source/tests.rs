//! Actual native direct-source contracts. Authored for final qualification;
//! these fixtures never construct a physical close receipt or substitute FD.

use super::*;
use crate::resource_custody::InstallationContext;
use std::time::Duration;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn actual_runtime(root: &std::path::Path) -> Arc<NodeResourceRuntime> {
    let runtime = NodeResourceRuntime::open(super::super::tests::options(
        root,
        super::super::daemon_owner("fixture-node".into(), "fixture-cluster".into()),
        InstallationContext::FirstInstallation,
    ))
    .expect("actual journal and daemon lock");
    runtime
        .update_capacity(
            1,
            Metric::Known(ResourceVector {
                cpu_ns_per_second: 2_000_000_000,
                peak_memory_bytes: 128 * 1024 * 1024,
                handles: 16,
                processes: 0,
                hardware_slots: 0,
                tuner_slots: 0,
                scratch_bytes: 0,
            }),
        )
        .expect("observed fixture capacity");
    Arc::new(runtime)
}

fn local_request(path: PathBuf, file_id: i64, chunks: usize) -> DirectSourceRequest {
    let path_len = path.to_str().expect("fixture UTF-8").len();
    let media_file_id = file_id.to_string();
    let memory = DirectSourceRequest::minimum_memory_bytes(
        path_len,
        media_file_id.len(),
        1,
        4096,
        chunks,
        0,
    )
    .expect("fixed direct footprint");
    DirectSourceRequest {
        binding: DirectSourceBinding::Local {
            authenticated_user_id: 7,
            file_id,
        },
        path,
        media_file_id,
        // Local authorization permits a stale scanner row. Only the original
        // admitted FD establishes S1, while the HTTP layer checks stored size.
        scanner_size: 0,
        scanner_mtime: 0,
        demand: WorkloadDemand {
            resources: Metric::Known(ResourceVector {
                cpu_ns_per_second: 0,
                peak_memory_bytes: memory + 4096,
                handles: 1,
                ..ResourceVector::default()
            }),
            limit_epoch: 1,
            trial: false,
        },
        max_bodies: 1,
        chunk_bytes: 4096,
        outstanding_chunks: chunks,
        application_owner_bytes: 0,
    }
}

#[tokio::test]
async fn actual_direct_owners_share_one_cpu_domain_until_last_known_close_and_app_drop() {
    let root = tempfile::tempdir().expect("actual isolated journal");
    let first = root.path().join("first.bin");
    let second = root.path().join("second.bin");
    std::fs::write(&first, b"first").expect("fixture file");
    std::fs::write(&second, b"second").expect("fixture file");
    let runtime = actual_runtime(root.path());
    let (first_owner, first_app) = runtime
        .prepare_direct_source(local_request(first, 1, 1), deadline())
        .expect("first atomic domain and source prepare");
    let (second_owner, second_app) = runtime
        .prepare_direct_source(local_request(second, 2, 1), deadline())
        .expect("second exact-domain source prepare");
    assert_eq!(
        runtime.accounting().expect("held").used.cpu_ns_per_second,
        1_000_000_000
    );
    assert_eq!(runtime.accounting().expect("held").used.handles, 2);
    first_owner
        .open(deadline())
        .await
        .expect("first original FD");
    second_owner
        .open(deadline())
        .await
        .expect("second original FD");
    let mut first_ticket = first_owner
        .prepare_retirement(deadline())
        .await
        .expect("first native close");
    drop(first_owner);
    assert!(first_ticket
        .finish_retirement(Instant::now() + Duration::from_millis(20))
        .await
        .is_err());
    assert_eq!(
        runtime.accounting().expect("app still held").used.handles,
        2
    );
    drop(first_app);
    first_ticket
        .finish_retirement(deadline())
        .await
        .expect("first known settlement");
    assert_eq!(
        runtime
            .accounting()
            .expect("domain and second source")
            .used
            .cpu_ns_per_second,
        1_000_000_000
    );
    assert_eq!(runtime.accounting().expect("second source").used.handles, 1);
    let mut second_ticket = second_owner
        .prepare_retirement(deadline())
        .await
        .expect("second native close");
    drop(second_owner);
    drop(second_app);
    second_ticket
        .finish_retirement(deadline())
        .await
        .expect("last source and domain settlement");
    second_ticket
        .finish_retirement(deadline())
        .await
        .expect("known finish is idempotent");
    assert_eq!(
        runtime.accounting().expect("all refunded").used,
        ResourceVector::default()
    );
}

#[tokio::test]
async fn cancelled_waiter_keeps_original_read_job_and_full_chunk_credit_until_join() {
    let root = tempfile::tempdir().expect("actual isolated journal");
    let path = root.path().join("read.bin");
    std::fs::write(&path, b"read content").expect("fixture file");
    let runtime = actual_runtime(root.path());
    let (owner, app) = runtime
        .prepare_direct_source(local_request(path, 3, 1), deadline())
        .expect("actual source prepare");
    let owner = Arc::new(owner);
    owner.open(deadline()).await.expect("original FD");
    let gate = DirectSourceReadGate::new();
    owner
        .install_read_gate(Arc::clone(&gate))
        .expect("original-job gate");
    let body = owner.begin_body().expect("counted body");
    let external = Arc::new(Semaphore::new(1));
    let permit = Arc::clone(&external)
        .acquire_owned()
        .await
        .expect("external native-read permit");
    let waiter_owner = Arc::clone(&owner);
    let waiter =
        tokio::spawn(async move { waiter_owner.read_at(&body, 0, 1, permit, deadline()).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !gate.entered() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("real registered read entered");
    waiter.abort();
    let _ = waiter.await;
    assert_eq!(
        external.available_permits(),
        0,
        "original job still owns permit"
    );
    assert!(owner
        .prepare_retirement(Instant::now() + Duration::from_millis(20))
        .await
        .is_err());
    assert_eq!(runtime.accounting().expect("no refund").used.handles, 1);
    gate.release();
    let mut ticket = owner
        .prepare_retirement(deadline())
        .await
        .expect("same original job joined");
    assert!(gate.complete() && gate.file_closed());
    assert_eq!(external.available_permits(), 1);
    drop(owner);
    drop(app);
    ticket
        .finish_retirement(deadline())
        .await
        .expect("known typed close");
    assert_eq!(
        runtime.accounting().expect("all refunded").used,
        ResourceVector::default()
    );
}

#[tokio::test]
async fn one_byte_chunks_consume_full_configured_slot_cardinality() {
    let root = tempfile::tempdir().expect("actual isolated journal");
    let path = root.path().join("chunks.bin");
    std::fs::write(&path, b"ab").expect("fixture file");
    let runtime = actual_runtime(root.path());
    let (owner, app) = runtime
        .prepare_direct_source(local_request(path, 4, 1), deadline())
        .expect("actual source prepare");
    owner.open(deadline()).await.expect("original FD");
    let body = owner.begin_body().expect("counted body");
    let external = Arc::new(Semaphore::new(1));
    let first = owner
        .read_at(
            &body,
            0,
            1,
            Arc::clone(&external).acquire_owned().await.unwrap(),
            deadline(),
        )
        .await
        .expect("one-byte read with full slot charge");
    assert!(
        owner
            .read_at(
                &body,
                1,
                1,
                Arc::clone(&external).acquire_owned().await.unwrap(),
                deadline()
            )
            .await
            .is_err(),
        "a second one-byte read cannot borrow the first full slot"
    );
    drop(first);
    drop(body);
    let mut ticket = owner
        .prepare_retirement(deadline())
        .await
        .expect("actual close");
    drop(owner);
    drop(app);
    ticket
        .finish_retirement(deadline())
        .await
        .expect("known settlement");
    assert_eq!(
        runtime.accounting().expect("all refunded").used,
        ResourceVector::default()
    );
}
