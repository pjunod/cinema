use super::*;
use plurx_core::{
    process::bounded::OriginalWorkUncertainty,
    store::{SqliteStore, Store},
    transcode::{EncoderCaps, Pipeline},
};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    sync::{Condvar, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

async fn admitted() -> (
    crate::transcode::TranscodeManager,
    Arc<FragmentAdmission>,
    tempfile::TempDir,
) {
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
    let admission = Arc::new(
        manager
            .admit_fragment()
            .await
            .expect("same original admission"),
    );
    (manager, admission, directory)
}

struct Release(Arc<(Mutex<bool>, Condvar)>);
impl Drop for Release {
    fn drop(&mut self) {
        let (released, changed) = &*self.0;
        *released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        changed.notify_all();
    }
}

#[tokio::test]
async fn cancelled_hash_waiter_retains_original_admission_until_actual_file_worker_exits() {
    let (manager, admission, directory) = admitted().await;
    let original = Arc::downgrade(&admission);
    let bytes = vec![73_u8; 3 * 128 * 1024];
    let expected = hex::encode(Sha256::digest(&bytes));
    let path = directory.path().join("unique-model.bin");
    std::fs::write(&path, &bytes).expect("real hash fixture");
    let mut file = File::open(path).expect("original File");
    let cancel = CancellationToken::new();
    let hash_cancel = cancel.clone();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let release = Release(Arc::clone(&gate));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let waiter_admission = Arc::clone(&admission);
    let waiter = tokio::spawn(async move {
        scope(Some(&waiter_admission), async move {
            spawn_blocking(move || {
                let mut entered_tx = Some(entered_tx);
                let result = crate::subtitle_models::artifacts::hash_held_lifetime_fixture(
                    &mut file,
                    &expected,
                    bytes.len() as u64,
                    &hash_cancel,
                    || {
                        if let Some(entered_tx) = entered_tx.take() {
                            let _ = entered_tx.send(std::thread::current().id());
                            let (released, changed) = &*gate;
                            let (released, timeout) = changed
                                .wait_timeout_while(
                                    released.lock().expect("hash gate"),
                                    Duration::from_secs(5),
                                    |released| !*released,
                                )
                                .expect("actual hash worker gate");
                            assert!(
                                *released && !timeout.timed_out(),
                                "fixture must release original worker"
                            );
                        }
                    },
                );
                let _ = finished_tx.send(result.is_err_and(|error| error == "worker_interrupted"));
                (file, ()) // the original File also remains in an unclaimed result
            })
            .await
            .expect("original hash join")
        })
        .await
    });
    let worker = tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .expect("actual first chunk read")
        .expect("entry witness");
    assert_ne!(worker, std::thread::current().id());
    cancel.cancel();
    drop(admission);
    waiter.abort();
    assert!(waiter.await.expect_err("cancelled caller").is_cancelled());
    assert!(
        original.upgrade().is_some(),
        "same original admission survives caller Drop"
    );
    let (pool, heavy) = manager.original_legacy_drain_counts();
    assert!(
        pool.software_used > 0 && heavy,
        "original permits held by actual worker"
    );
    assert!(
        manager.admit_fragment().await.is_none(),
        "cannot replace still-running admitted work"
    );
    drop(release);
    assert!(tokio::time::timeout(Duration::from_secs(5), finished_rx)
        .await
        .expect("same worker finished")
        .expect("completion witness"));
    tokio::time::timeout(Duration::from_secs(5), async {
        while original.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual task/result dropped original admission");
    let (pool, heavy) = manager.original_legacy_drain_counts();
    assert_eq!(pool.software_used, 0);
    assert!(!heavy);
    // This asserts actual File/task/permit lifetime only. No native-group
    // extinction, restart readiness, finite peak or refund is inferred.
}

type Events = Arc<Mutex<Vec<&'static str>>>;
struct ObservedFile {
    file: Option<File>,
    events: Events,
}
impl Drop for ObservedFile {
    fn drop(&mut self) {
        drop(self.file.take());
        self.events.lock().expect("file events").push("file");
    }
}
struct ObservedKeeper {
    original: Arc<FragmentAdmission>,
    events: Events,
}
impl OriginalWorkKeeper for ObservedKeeper {
    fn retain_uncertainty(&self, reason: OriginalWorkUncertainty) {
        self.original.retain_uncertainty(reason);
    }
}
impl Drop for ObservedKeeper {
    fn drop(&mut self) {
        self.events.lock().expect("keeper events").push("keeper");
    }
}

#[tokio::test]
async fn unentered_whole_input_drops_actual_file_before_same_keeper() {
    let (_manager, admission, directory) = admitted().await;
    let original = Arc::downgrade(&admission);
    let events = Arc::new(Mutex::new(Vec::new()));
    let file = ObservedFile {
        file: Some(File::create(directory.path().join("input")).expect("actual File")),
        events: Arc::clone(&events),
    };
    let keeper: Arc<dyn OriginalWorkKeeper> = Arc::new(ObservedKeeper {
        original: admission,
        events: Arc::clone(&events),
    });
    let input =
        bounded::with_original_work_keeper(
            keeper,
            async move { Retained::new(move || drop(file)) },
        )
        .await;
    assert!(original.upgrade().is_some());
    assert!(events.lock().expect("events").is_empty());
    // Drop the exact prebuilt queued input without ever invoking its closure.
    drop(input);
    assert_eq!(&*events.lock().expect("events"), &["file", "keeper"]);
    assert!(original.upgrade().is_none());
}

#[tokio::test]
async fn completed_unconsumed_file_result_retains_same_keeper_until_result_disposal() {
    let (manager, admission, directory) = admitted().await;
    let original = Arc::downgrade(&admission);
    let events = Arc::new(Mutex::new(Vec::new()));
    let path = directory.path().join("returned");
    let result_events = Arc::clone(&events);
    let keeper: Arc<dyn OriginalWorkKeeper> = Arc::new(ObservedKeeper {
        original: admission,
        events: Arc::clone(&events),
    });
    // Deliberately keep the actual ready JoinHandle unconsumed. This is a
    // result-lifetime test, so awaiting it here would erase the tested state.
    struct Unconsumed<T>(tokio::task::JoinHandle<Retained<T>>);
    let Unconsumed(task) = bounded::with_original_work_keeper(keeper, async move {
        Unconsumed(spawn_retained(move || ObservedFile {
            file: Some(File::create(path).expect("worker File")),
            events: result_events,
        }))
    })
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !task.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual blocking result ready");
    assert!(
        original.upgrade().is_some(),
        "job completion must not release live unclaimed result"
    );
    let (pool, heavy) = manager.original_legacy_drain_counts();
    assert!(pool.software_used > 0 && heavy);
    assert!(events.lock().expect("events").is_empty());
    drop(task);
    tokio::time::timeout(Duration::from_secs(5), async {
        while original.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("same completed result disposed");
    assert_eq!(&*events.lock().expect("events"), &["file", "keeper"]);
}

#[tokio::test]
async fn accepted_cached_file_does_not_keep_cpu_admission_after_original_scope_ends() {
    let (_manager, admission, directory) = admitted().await;
    let original = Arc::downgrade(&admission);
    let path = directory.path().join("accepted");
    let file = scope(Some(&admission), async move {
        spawn_blocking(move || File::create(path).expect("actual cached File"))
            .await
            .expect("existing worker join")
    })
    .await;
    drop(admission);
    assert!(
        original.upgrade().is_none(),
        "accepted cache value carries no permanent CPU guard"
    );
    assert!(file.metadata().expect("accepted original File").is_file());
}
