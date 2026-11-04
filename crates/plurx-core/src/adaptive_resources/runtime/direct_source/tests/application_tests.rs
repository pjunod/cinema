//! Actual direct descriptors, original driver joins and rich payload drops.
use super::*;
use crate::adaptive_resources::owner_inventory::{OwnerKind, OwnerPhase};
use std::sync::atomic::AtomicUsize;

struct Payload {
    _owner: Arc<DirectSourceOwner>,
    drops: Arc<AtomicUsize>,
}
impl Drop for Payload {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
fn tracked_request(path: PathBuf, id: i64) -> DirectSourceRequest {
    let mut request = local_request(path, id, 1);
    request.application_owner_bytes = 4096;
    if let Metric::Known(resources) = &mut request.demand.resources {
        resources.peak_memory_bytes += request.application_owner_bytes;
    }
    request
}
fn references(runtime: &NodeResourceRuntime) -> usize {
    runtime
        .affected_owner_inventory()
        .snapshot()
        .unwrap()
        .records()
        .filter(|row| matches!(row.kind, OwnerKind::Reference { .. }))
        .count()
}

#[tokio::test]
async fn original_worker_join_keeps_reference_until_every_rich_payload_is_destroyed() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    std::fs::write(&path, b"original source").unwrap();
    let runtime = actual_runtime(root.path());
    let (owner, lease) = runtime
        .prepare_direct_source(tracked_request(path, 31), deadline())
        .unwrap();
    let owner = Arc::new(owner);
    assert!(
        owner.open(deadline()).await.is_err(),
        "entry needs original registered worker and payload"
    );
    let drops = Arc::new(AtomicUsize::new(0));
    let file = Arc::new(
        lease
            .wrap(Payload {
                _owner: Arc::clone(&owner),
                drops: Arc::clone(&drops),
            })
            .unwrap(),
    );
    let actor = Arc::new(
        lease
            .wrap(Payload {
                _owner: Arc::clone(&owner),
                drops: Arc::clone(&drops),
            })
            .unwrap(),
    );
    let escaped_actor = Arc::clone(&actor);
    drop(lease);
    let (send, receive) = oneshot::channel();
    let original = Arc::clone(&owner);
    let worker = owner
        .spawn_application_worker(async move {
            receive.await.unwrap();
            original.prepare_retirement(deadline()).await
        })
        .unwrap();
    owner.open(deadline()).await.unwrap();
    let charged = runtime.accounting().unwrap().used;
    assert_eq!(references(&runtime), 1);
    send.send(()).unwrap();
    let mut ticket = worker.await.unwrap();
    drop(owner);
    drop(file);
    drop(actor);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(ticket
        .finish_retirement(Instant::now() + Duration::from_millis(25))
        .await
        .is_err());
    assert_eq!(runtime.accounting().unwrap().used, charged);
    assert_eq!(
        references(&runtime),
        1,
        "actual cached actor alias still owns rich allocations"
    );
    drop(escaped_actor);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
    ticket.finish_retirement(deadline()).await.unwrap();
    assert_eq!(references(&runtime), 0);
    assert_eq!(
        runtime
            .affected_owner_inventory()
            .snapshot()
            .unwrap()
            .records()
            .count(),
        0
    );
    assert_eq!(
        runtime.accounting().unwrap().used,
        ResourceVector::default()
    );
}

#[tokio::test]
async fn cancelled_application_waiter_rejoins_same_original_handle() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    std::fs::write(&path, b"original source").unwrap();
    let runtime = actual_runtime(root.path());
    let (owner, lease) = runtime
        .prepare_direct_source(tracked_request(path, 32), deadline())
        .unwrap();
    let owner = Arc::new(owner);
    let payload = lease
        .wrap(Payload {
            _owner: Arc::clone(&owner),
            drops: Arc::new(AtomicUsize::new(0)),
        })
        .unwrap();
    drop(lease);
    let (send, receive) = oneshot::channel();
    let original = Arc::clone(&owner);
    let mut worker = owner
        .spawn_application_worker(async move {
            receive.await.unwrap();
            original.prepare_retirement(deadline()).await
        })
        .unwrap();
    owner.open(deadline()).await.unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(25), &mut worker)
        .await
        .is_err());
    drop(worker);
    assert_eq!(references(&runtime), 1);
    let worker = owner.resume_application_worker().unwrap();
    assert!(!worker.is_finished());
    send.send(()).unwrap();
    let mut ticket = worker.await.unwrap();
    drop(owner);
    drop(payload);
    ticket.finish_retirement(deadline()).await.unwrap();
    assert_eq!(
        runtime.accounting().unwrap().used,
        ResourceVector::default()
    );
    assert_eq!(references(&runtime), 0);
}

#[tokio::test]
async fn original_worker_panic_keeps_unknown_reference_and_forbids_successor() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    std::fs::write(&path, b"original source").unwrap();
    let runtime = actual_runtime(root.path());
    let (owner, lease) = runtime
        .prepare_direct_source(tracked_request(path, 33), deadline())
        .unwrap();
    let owner = Arc::new(owner);
    let payload = lease
        .wrap(Payload {
            _owner: Arc::clone(&owner),
            drops: Arc::new(AtomicUsize::new(0)),
        })
        .unwrap();
    drop(lease);
    let worker = owner
        .spawn_application_worker(async { panic!("actual original driver panic") })
        .unwrap();
    assert!(worker.await.is_err());
    drop(payload);
    assert!(owner.resume_application_worker().is_err());
    assert!(owner
        .spawn_application_worker(async { panic!("successor must not run") })
        .is_err());
    let census = runtime.affected_owner_inventory().snapshot().unwrap();
    assert!(census
        .records()
        .any(|row| matches!(row.kind, OwnerKind::Reference { .. })
            && row.phase == OwnerPhase::UnknownHeld));
    assert!(runtime.accounting().unwrap().used.peak_memory_bytes > 0);
}

#[tokio::test]
async fn rich_payload_destructor_panic_never_certifies_application_close() {
    struct Panics;
    impl Drop for Panics {
        fn drop(&mut self) {
            panic!("actual payload destructor panic");
        }
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    std::fs::write(&path, b"original source").unwrap();
    let runtime = actual_runtime(root.path());
    let (owner, lease) = runtime
        .prepare_direct_source(tracked_request(path, 34), deadline())
        .unwrap();
    let owner = Arc::new(owner);
    let payload = lease.wrap(Panics).unwrap();
    drop(lease);
    let original = Arc::clone(&owner);
    let worker = owner
        .spawn_application_worker(async move { original.prepare_retirement(deadline()).await })
        .unwrap();
    let mut ticket = worker.await.unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload))).is_err());
    assert!(ticket.finish_retirement(deadline()).await.is_err());
    let census = runtime.affected_owner_inventory().snapshot().unwrap();
    assert!(census
        .records()
        .any(|row| matches!(row.kind, OwnerKind::Reference { .. })
            && row.phase == OwnerPhase::UnknownHeld));
    assert!(runtime.accounting().unwrap().used.peak_memory_bytes > 0);
}

#[test]
fn executor_shutdown_destroys_original_payload_without_application_mutex_deadlock() {
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        std::fs::write(&path, b"original source").unwrap();
        let runtime = actual_runtime(root.path());
        let (owner, lease) = runtime
            .prepare_direct_source(tracked_request(path, 35), deadline())
            .unwrap();
        let owner = Arc::new(owner);
        let drops = Arc::new(AtomicUsize::new(0));
        let payload = lease
            .wrap(Payload {
                _owner: Arc::clone(&owner),
                drops: Arc::clone(&drops),
            })
            .unwrap();
        drop(lease);
        let closed = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let handle = closed.handle().clone();
        drop(closed);
        let context = handle.enter();
        let original = owner
            .spawn_application_worker(async move {
                let _payload = payload;
                std::future::pending::<Result<DirectSourceRetirementTicket, DirectSourceError>>()
                    .await
            })
            .unwrap();
        drop(context);
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "closed executor destroys the actual original future synchronously"
        );
        let waiter = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(waiter.block_on(original).is_err());
        assert!(owner.resume_application_worker().is_err());
        let census = runtime.affected_owner_inventory().snapshot().unwrap();
        assert!(census
            .records()
            .any(|row| matches!(row.kind, OwnerKind::Reference { .. })
                && row.phase == OwnerPhase::UnknownHeld));
        assert!(runtime.accounting().unwrap().used.peak_memory_bytes > 0);
        send.send(()).unwrap();
    });
    receive
        .recv_timeout(Duration::from_secs(10))
        .expect("FAILURE watchdog: original future destruction deadlocked under application mutex");
    worker.join().expect("actual shutdown fixture joined");
}
