use super::*;
use plurx_core::store::{SqliteStore, Store};
use plurx_core::transcode::{EncoderCaps, Pipeline};
use std::sync::{Condvar, Mutex};

// Release on assertion failure too: a broken lifetime must fail the fixture,
// never leave the runtime waiting forever for its blocking task to finish.
struct ReleaseInference(Arc<(Mutex<bool>, Condvar)>);
impl Drop for ReleaseInference {
    fn drop(&mut self) {
        let (released, changed) = &*self.0;
        *released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        changed.notify_all();
    }
}

#[tokio::test]
async fn dropped_inference_waiter_retains_same_admission_until_original_task_exits() {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    store
        .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "2")
        .await
        .expect("software budget");
    let directory = crate::test_tempdir().expect("scratch");
    let monitor = plurx_core::cluster::migration::status::ReplicationMonitor::sqlite();
    let serving = crate::serving_fence::ServingFence::new(monitor.metrics_handle());
    serving.validation_set_ready(true).await;
    let manager = crate::transcode::TranscodeManager::new(
        store,
        directory.path().into(),
        EncoderCaps::default(),
        Pipeline::Cpu,
    )
    .with_serving_authority(serving.authority());
    let admission = Arc::new(manager.admit_fragment().await.expect("original admission"));
    let original = Arc::downgrade(&admission);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let release = ReleaseInference(Arc::clone(&gate));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let task = spawn_admitted_inference(Arc::clone(&admission), move || {
        let _ = entered_tx.send(std::thread::current().id());
        let (released, changed) = &*gate;
        let (released, timeout) = changed
            .wait_timeout_while(
                released.lock().expect("inference gate"),
                Duration::from_secs(5),
                |released| !*released,
            )
            .expect("original blocking inference gate");
        assert!(
            *released && !timeout.timed_out(),
            "fixture must release the same original task"
        );
        let _ = finished_tx.send(());
        17_u32
    });
    // This is the only waiter for that exact original blocking task. Dropping
    // it mirrors cancellation of run's borrowed JoinHandle wait.
    let waiter = tokio::spawn(async move { task.await.expect("original inference joined") });
    let worker_thread = tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .expect("blocking inference entered")
        .expect("entry witness");
    assert_ne!(
        worker_thread,
        std::thread::current().id(),
        "actual blocking worker thread"
    );
    drop(admission);
    waiter.abort();
    assert!(waiter.await.expect_err("caller dropped").is_cancelled());
    assert!(
        original.upgrade().is_some(),
        "same original admission still owned by work"
    );
    let (pool, heavy) = manager.original_legacy_drain_counts();
    assert!(
        pool.software_used > 0 && heavy,
        "original physical permits stay held"
    );
    assert!(
        manager.admit_fragment().await.is_none(),
        "no replacement work can enter"
    );
    let token = serving
        .begin_restart_preparation_until(u64::try_from(now_ms()).expect("clock") + 60_000)
        .await
        .expect("restart fence");
    let pending = serving.restart_drain_status(0).await;
    assert_eq!(pending.admissions_in_flight, 1, "one same original guard");
    assert!(!pending.drained);

    drop(release);
    tokio::time::timeout(Duration::from_secs(5), finished_rx)
        .await
        .expect("same blocking task completed")
        .expect("completion witness");
    tokio::time::timeout(Duration::from_secs(5), async {
        while original.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("original task released its original guard");
    let (pool, heavy) = manager.original_legacy_drain_counts();
    assert_eq!(pool.software_used, 0);
    assert!(!heavy);
    assert_eq!(
        serving.restart_drain_status(0).await.admissions_in_flight,
        0
    );
    serving.cancel_planned_outage_preparation(token, 0).await;
    // This checks actual task/permit lifetime, not BERT correctness, complete
    // backend/thread-pool extinction, finite allocation or a native refund.
}
