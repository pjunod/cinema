use super::super::{open_source_playback_fence, PlaybackOpenAfterOpen, PLAYBACK_OPEN_AFTER_OPEN};
use crate::{
    serving_fence::{OwnedWorkInventory, OwnedWorkUnknown, ServingFence},
    transcode::{FragmentAdmission, TranscodeManager},
};
use plurx_core::{
    process::bounded::{self, OriginalWorkKeeper, OriginalWorkUncertainty},
    store::{SqliteStore, Store},
    transcode::{EncoderCaps, Pipeline},
};
use std::{
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

async fn admitted() -> (
    TranscodeManager,
    ServingFence,
    Arc<FragmentAdmission>,
    tempfile::TempDir,
) {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    store
        .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "2")
        .await
        .expect("software budget");
    let directory = crate::test_tempdir().expect("actual source root");
    let monitor = plurx_core::cluster::migration::status::ReplicationMonitor::sqlite();
    let serving = ServingFence::new(monitor.metrics_handle());
    serving.validation_set_ready(true).await;
    let manager = TranscodeManager::new(
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
    (manager, serving, admission, directory)
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

async fn assert_same_unknown(serving: &ServingFence) -> crate::serving_fence::RestartDrainStatus {
    let (_, inventory, status) = serving
        .restart_drain_status_with_owned_work(0, || async {
            panic!("original cleanup Unknown must preempt any fresh inventory sample")
        })
        .await;
    assert_eq!(
        inventory,
        OwnedWorkInventory::Unknown {
            reason: OwnedWorkUnknown::NativeCleanupUncertain {
                collector_dropped: true,
                leader_reap_failed: false,
                pipe_drain_failed: false,
                runtime_unavailable: false,
            },
        }
    );
    assert!(!status.drained);
    // Unknown retains this operation and blocks restart drain; open intake
    // still follows normal capacity and phase fences.
    assert!(!status.new_admissions_blocked);
    status
}

#[tokio::test]
async fn actual_open_timeout_retains_original_work_and_unknown_after_worker_exit() {
    let (manager, serving, admission, directory) = admitted().await;
    let path = directory.path().join("actual-source.mkv");
    std::fs::write(&path, b"real source bytes").expect("fixture source");
    let file = super::sampled_file(path);
    let expected = plurx_core::fs_secure::std_file_identity(
        &std::fs::File::open(&file.path).expect("fixture comparison File"),
    )
    .expect("actual source identity");
    let original = Arc::downgrade(&admission);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let release = Release(Arc::clone(&gate));
    let (opened_tx, opened_rx) = tokio::sync::oneshot::channel();
    let opened_tx = Mutex::new(Some(opened_tx));
    let (returned_tx, returned_rx) = tokio::sync::oneshot::channel();
    let returned_tx = Mutex::new(Some(returned_tx));
    let hook: PlaybackOpenAfterOpen = Arc::new(move |held| {
        // The actual no-follow native open has already happened in this same
        // original job. No metadata/path observation is used as close proof.
        let identity = plurx_core::fs_secure::std_file_identity(held).expect("original held File");
        let _ = opened_tx
            .lock()
            .expect("open witness")
            .take()
            .expect("one original open")
            .send((identity, std::thread::current().id()));
        let (released, changed) = &*gate;
        let (released, timeout) = changed
            .wait_timeout_while(
                released.lock().expect("opener gate"),
                Duration::from_secs(10),
                |released| !*released,
            )
            .expect("same original opener gate");
        assert!(
            *released && !timeout.timed_out(),
            "fixture must release same original worker"
        );
        assert_eq!(
            plurx_core::fs_secure::std_file_identity(held).expect("same still-owned File"),
            identity
        );
        let _ = returned_tx
            .lock()
            .expect("return witness")
            .take()
            .expect("one same return")
            .send(());
    });
    let keeper: Arc<dyn OriginalWorkKeeper> = admission.clone();
    let waiter = tokio::spawn(bounded::with_original_work_keeper(
        keeper,
        PLAYBACK_OPEN_AFTER_OPEN.scope(hook, async move {
            // This is the existing translation call-site's actual two-second
            // timeout pattern. The outer watchdog below can only FAIL.
            tokio::time::timeout(
                Duration::from_secs(2),
                open_source_playback_fence(&file, None),
            )
            .await
        }),
    ));
    let (held_identity, worker_thread) = tokio::time::timeout(Duration::from_secs(5), opened_rx)
        .await
        .expect("actual native open reached")
        .expect("actual File witness");
    assert_eq!(held_identity, expected);
    assert_ne!(worker_thread, std::thread::current().id());
    drop(admission);
    let outcome = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("FAILURE watchdog: existing caller deadline must return")
        .expect("original waiter joined");
    assert!(
        outcome.is_err(),
        "only actual inner timeout counts; worker is still gated"
    );
    assert!(
        original.upgrade().is_some(),
        "same original admission held by actual native job"
    );
    let (pool, heavy) = manager.original_legacy_drain_counts();
    assert!(pool.software_used > 0 && heavy);
    assert_eq!(assert_same_unknown(&serving).await.admissions_in_flight, 1);
    assert!(
        manager.admit_fragment().await.is_none(),
        "no replacement permit"
    );

    drop(release);
    tokio::time::timeout(Duration::from_secs(5), returned_rx)
        .await
        .expect("same native job released")
        .expect("original return witness");
    tokio::time::timeout(Duration::from_secs(5), async {
        while original.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("same actual job/result disposed original keeper");
    let (pool, heavy) = manager.original_legacy_drain_counts();
    assert_eq!(pool.software_used, 0);
    assert!(!heavy);
    assert_eq!(assert_same_unknown(&serving).await.admissions_in_flight, 0);
    assert!(
        manager.admit_fragment().await.is_none(),
        "counter0 cannot clear original Unknown"
    );
    // Job/File disposal grants NO native absence/refund proof. Persistence is
    // deliberate: a slow open can fence this process even after its job exits.
}

#[tokio::test]
async fn known_actual_open_disarms_only_own_observation_and_never_clears_prior_unknown() {
    let (_manager, serving, admission, directory) = admitted().await;
    let path = directory.path().join("known-source.mkv");
    std::fs::write(&path, b"same source").expect("real source");
    let file = super::sampled_file(path);
    let keeper: Arc<dyn OriginalWorkKeeper> = admission.clone();
    let source =
        bounded::with_original_work_keeper(keeper, open_source_playback_fence(&file, None))
            .await
            .expect("known actual File result");
    assert!(
        !serving.restart_drain_status(0).await.new_admissions_blocked,
        "known actual result must not manufacture lost-waiter uncertainty"
    );
    admission.retain_uncertainty(OriginalWorkUncertainty::CollectorDropped);
    let keeper: Arc<dyn OriginalWorkKeeper> = admission.clone();
    let again = bounded::with_original_work_keeper(
        keeper,
        open_source_playback_fence(&file, Some(source.object_version())),
    )
    .await
    .expect("same original admitted work can observe known result");
    assert_eq!(
        plurx_core::fs_secure::std_file_identity(&source.handle).expect("held identity"),
        plurx_core::fs_secure::std_file_identity(&again.handle)
            .expect("new known actual File identity")
    );
    assert_eq!(assert_same_unknown(&serving).await.admissions_in_flight, 1);
    drop((source, again, admission));
    assert_eq!(assert_same_unknown(&serving).await.admissions_in_flight, 0);
}

#[tokio::test]
async fn actual_open_task_panic_retains_same_original_unobserved_result_uncertainty() {
    let (_manager, serving, admission, directory) = admitted().await;
    let path = directory.path().join("panic-source.mkv");
    std::fs::write(&path, b"actually opened before panic").expect("real source");
    let file = super::sampled_file(path);
    let hook: PlaybackOpenAfterOpen = Arc::new(|held| {
        assert!(held
            .metadata()
            .expect("actual original File before panic")
            .is_file());
        panic!("fixture loses original native result after actual open");
    });
    let keeper: Arc<dyn OriginalWorkKeeper> = admission.clone();
    let result = bounded::with_original_work_keeper(
        keeper,
        PLAYBACK_OPEN_AFTER_OPEN.scope(hook, open_source_playback_fence(&file, None)),
    )
    .await;
    assert_eq!(
        result.err().expect("unobserved original result"),
        "Source file open task failed"
    );
    assert_same_unknown(&serving).await;
    drop(admission);
    assert_eq!(assert_same_unknown(&serving).await.admissions_in_flight, 0);
}
