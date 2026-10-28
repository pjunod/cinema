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
        media_file_id.capacity(),
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
                peak_memory_bytes: memory,
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
    let journal =
        rusqlite::Connection::open(root.path().join("resource-custody/obligations.sqlite3"))
            .unwrap();
    assert_eq!(
        journal
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
    let mut statement = journal
        .prepare("SELECT adapter,phase FROM members ORDER BY adapter,member_id")
        .unwrap();
    let persisted = statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        persisted
            .iter()
            .filter(|(adapter, _)| adapter == "direct_read_domain")
            .count(),
        1
    );
    assert_eq!(
        persisted
            .iter()
            .filter(|(adapter, _)| adapter == "direct_source_file")
            .count(),
        2
    );
    assert!(persisted
        .iter()
        .filter(|(adapter, _)| adapter == "direct_source_file")
        .all(|(_, phase)| phase == "acquired"));
    drop(statement);
    drop(journal);
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

/// Runs a real short-lived daemon fixture in this test binary. process::exit
/// deliberately skips Rust Drop, leaving the journal's original holds while
/// the kernel closes the actual source descriptor and read-thread domain.
#[test]
fn restart_closes_only_positive_old_direct_domains_and_validates_close_history() {
    const CHILD_ROOT: &str = "PLURX_DIRECT_EXIT_FIXTURE_ROOT";
    if let Some(path) = std::env::var_os(CHILD_ROOT) {
        let root = PathBuf::from(path);
        let runtime = actual_runtime(&root);
        let source = root.join("source.bin");
        std::fs::write(&source, b"actual old daemon descriptor").unwrap();
        let (owner, _application) = runtime
            .prepare_direct_source(local_request(source, 42, 1), deadline())
            .expect("real admitted source and execution domain");
        assert_eq!(runtime.recovered.obligations.len(), 0);
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        executor.block_on(async {
            let owner = Arc::new(owner);
            owner
                .open(deadline())
                .await
                .expect("actual original source FD");
            let gate = DirectSourceReadGate::new();
            owner.install_read_gate(Arc::clone(&gate)).unwrap();
            let body = owner.begin_body().unwrap();
            let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
            let reader = Arc::clone(&owner);
            let _waiter =
                tokio::spawn(async move { reader.read_at(&body, 0, 1, permit, deadline()).await });
            tokio::time::timeout(Duration::from_secs(5), async {
                while !gate.entered() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("original registered native read is running at daemon exit");
            std::process::exit(99);
        });
        unreachable!("fixture exit skips all native and application Drop");
    }
    for current_boot_known in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "adaptive_resources::runtime::direct_source::tests::restart_closes_only_positive_old_direct_domains_and_validates_close_history", "--nocapture"])
            .env(CHILD_ROOT, root.path())
            .status().expect("actual old daemon subprocess");
        assert_eq!(status.code(), Some(99));
        let mut current =
            super::super::daemon_owner("fixture-node".into(), "fixture-cluster".into());
        if !current_boot_known {
            current.os_boot = crate::resource_custody::BootIdentity::Unknown {
                reason: "fixture current boot observation unavailable".into(),
            };
        }
        let (journal, recovered) = crate::resource_custody::CustodyJournal::open(
            super::super::tests::options(root.path(), current, InstallationContext::Existing),
        )
        .expect("strict original journal reconstruction");
        if !current_boot_known {
            assert_eq!(
                recovered.obligations.len(),
                2,
                "unknown boot retains source and CPU domain"
            );
            assert!(recovered.total.peak_memory_bytes > 0);
            assert!(recovered.total.cpu_ns_per_second > 0);
            drop(journal);
            continue;
        }
        assert!(recovered.obligations.is_empty());
        assert_eq!(recovered.total, ResourceVector::default());
        drop(journal);
        let db =
            rusqlite::Connection::open(root.path().join("resource-custody/obligations.sqlite3"))
                .unwrap();
        let (obligation, encoded): (Vec<u8>, String) = db
            .query_row(
                "SELECT obligation_id,settlement_json FROM obligations ORDER BY sequence LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let mut audit: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        let prior = audit["prior_sequence"].as_i64().unwrap();
        audit["prior_sequence"] = serde_json::json!(prior + 1);
        let corrupted = serde_json::to_string(&audit).unwrap();
        db.execute(
            "UPDATE obligations SET settlement_json=?2 WHERE obligation_id=?1",
            rusqlite::params![obligation, corrupted],
        )
        .unwrap();
        db.execute(
            "UPDATE members SET proof_json=?2 WHERE obligation_id=?1",
            rusqlite::params![obligation, corrupted],
        )
        .unwrap();
        drop(db);
        let mut next = super::super::daemon_owner("fixture-node".into(), "fixture-cluster".into());
        next.owner_id = Uuid::new_v4();
        assert!(
            crate::resource_custody::CustodyJournal::open(super::super::tests::options(
                root.path(),
                next,
                InstallationContext::Existing,
            ))
            .is_err(),
            "rewritten close history cannot become a valid empty reconstruction"
        );
    }
}

// These are negative constructor checks on the actual runtime and original
// request. No absence receipt is manufactured from a missing returned owner.
#[tokio::test]
async fn expired_invalid_and_overflow_direct_requests_are_before_reservation() {
    let root = tempfile::tempdir().expect("isolated runtime");
    let runtime = actual_runtime(root.path());
    let path = root.path().join("unopened.bin");
    std::fs::write(&path, b"actual unopened source").expect("actual source");
    let baseline = runtime.accounting().expect("before requests").used;
    let expired = runtime.prepare_direct_source(local_request(path.clone(), 1, 1), Instant::now());
    assert!(expired
        .err()
        .expect("expired request")
        .definitely_before_reservation());
    let mut invalid = local_request(path.clone(), 1, 1);
    invalid.path = PathBuf::from("relative.bin");
    assert!(runtime
        .prepare_direct_source(invalid, deadline())
        .err()
        .expect("invalid path")
        .definitely_before_reservation());
    // A request may retain more text capacity than its visible ID length.
    // This is the actual constructor boundary, not a synthetic close result:
    // increasing the captured allocation without increasing its demand must
    // refuse BEFORE tagging R or opening the native source/domain.
    let mut retained_capacity = local_request(path.clone(), 1, 1);
    retained_capacity.media_file_id.reserve(1024);
    assert!(retained_capacity.media_file_id.capacity() > retained_capacity.media_file_id.len());
    assert!(runtime
        .prepare_direct_source(retained_capacity, deadline())
        .err()
        .expect("retained original ID capacity exceeds its earlier demand")
        .definitely_before_reservation());
    let mut overflow = local_request(path, 1, 1);
    overflow.application_owner_bytes = u64::MAX;
    assert!(runtime
        .prepare_direct_source(overflow, deadline())
        .err()
        .expect("layout overflow")
        .definitely_before_reservation());
    assert_eq!(
        runtime.accounting().expect("no R/domain charge").used,
        baseline
    );
    assert!(runtime
        .direct_read_domain
        .lock()
        .expect("original domain slot")
        .is_none());
    assert!(
        !DirectSourceError(OperationError::State).definitely_before_reservation(),
        "generic post-reservation state remains uncertain"
    );
}
