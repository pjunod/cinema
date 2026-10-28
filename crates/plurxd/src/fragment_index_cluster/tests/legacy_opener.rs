//! Actual legacy open/fstat/result custody. These bodies are authored only;
//! original-keeper disposal never grants native closure or clears Unknown.
use super::super::{
    open_source_fence, LegacyOpenAfterMetadata, PlaybackOpenAfterOpen, LEGACY_OPEN_AFTER_METADATA,
    PLAYBACK_OPEN_AFTER_OPEN,
};
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
        .expect("original software budget");
    let directory = crate::test_tempdir().expect("actual legacy source root");
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

async fn assert_unknown(serving: &ServingFence, admissions: u64) {
    let (_, inventory, status) = serving
        .restart_drain_status_with_owned_work(0, || async {
            panic!("same original Unknown must precede a fresh inventory sample")
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
    // Unknown retains this operation and blocks restart drain; open intake
    // still follows normal capacity and phase fences.
    assert!(!status.new_admissions_blocked);
    assert!(!status.drained);
    assert_eq!(status.admissions_in_flight, admissions);
}

async fn lost_legacy_waiter(after_metadata: bool) {
    let (manager, serving, admission, directory) = admitted().await;
    let path = directory.path().join("actual-legacy-source.mkv");
    std::fs::write(&path, b"actual legacy source bytes").expect("real source");
    let file = super::sampled_file(path);
    let expected_size = file.size as u64;
    let expected = plurx_core::fs_secure::std_file_identity(
        &std::fs::File::open(&file.path).expect("comparison File"),
    )
    .expect("actual source identity");
    let original = Arc::downgrade(&admission);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let release = Release(Arc::clone(&gate));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered_tx = Mutex::new(Some(entered_tx));
    let (returned_tx, returned_rx) = tokio::sync::oneshot::channel();
    let returned_tx = Mutex::new(Some(returned_tx));
    let observe: PlaybackOpenAfterOpen = Arc::new(move |held| {
        let identity =
            plurx_core::fs_secure::std_file_identity(held).expect("actual same held File");
        entered_tx
            .lock()
            .expect("native witness")
            .take()
            .expect("one original native phase")
            .send((identity, std::thread::current().id()))
            .expect("original waiter observes real phase");
        let (released, changed) = &*gate;
        let (released, timeout) = changed
            .wait_timeout_while(
                released.lock().expect("same legacy worker gate"),
                Duration::from_secs(10),
                |released| !*released,
            )
            .expect("bounded original worker gate");
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
            .expect("same one return")
            .send(());
    });
    let keeper: Arc<dyn OriginalWorkKeeper> = admission.clone();
    let waiter = tokio::spawn(bounded::with_original_work_keeper(keeper, async move {
        if after_metadata {
            let hook: LegacyOpenAfterMetadata = Arc::new(move |held, metadata| {
                assert!(metadata.is_file());
                assert_eq!(metadata.len(), expected_size);
                // File and metadata came from the same actual worker. This
                // gate is before return, not a claimed ready/closed receipt.
                observe(held);
            });
            LEGACY_OPEN_AFTER_METADATA
                .scope(hook, open_source_fence(&file, None))
                .await
        } else {
            PLAYBACK_OPEN_AFTER_OPEN
                .scope(observe, open_source_fence(&file, None))
                .await
        }
    }));
    let (identity, worker) = tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .expect("actual native open/fstat phase reached")
        .expect("real File witness");
    assert_eq!(identity, expected);
    assert_ne!(worker, std::thread::current().id());
    drop(admission);
    // Drop the actual awaiting opener only after the real native File exists.
    // The blocking worker remains gated and owns that original File.
    waiter.abort();
    assert!(waiter
        .await
        .err()
        .expect("same original awaiting future cancelled")
        .is_cancelled());
    assert!(
        original.upgrade().is_some(),
        "actual native job lost original admission"
    );
    let (pool, heavy) = manager.original_legacy_drain_counts();
    assert!(pool.software_used > 0 && heavy);
    assert_unknown(&serving, 1).await;
    assert!(
        manager.admit_fragment().await.is_none(),
        "no replacement admission"
    );

    drop(release);
    tokio::time::timeout(Duration::from_secs(5), returned_rx)
        .await
        .expect("same worker released")
        .expect("actual native phase returns");
    // The genuine File/Metadata result now has no consumer. The existing
    // Retained carrier disposes its value before its same original keeper.
    tokio::time::timeout(Duration::from_secs(5), async {
        while original.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("same job and unclaimed result dispose original keeper");
    let (pool, heavy) = manager.original_legacy_drain_counts();
    assert_eq!(pool.software_used, 0);
    assert!(!heavy);
    assert_unknown(&serving, 0).await;
    assert!(
        manager.admit_fragment().await.is_none(),
        "worker/result disposal cannot clear Unknown"
    );
}

#[tokio::test]
async fn actual_legacy_open_cancellation_keeps_original_job_and_unknown_after_exit() {
    lost_legacy_waiter(false).await;
}

#[tokio::test]
async fn actual_unclaimed_legacy_file_and_metadata_result_keeps_original_custody() {
    lost_legacy_waiter(true).await;
}

#[tokio::test]
async fn known_legacy_native_error_disarms_only_own_observation_and_never_clears_unknown() {
    let (_manager, serving, admission, directory) = admitted().await;
    let path = directory.path().join("missing-legacy-source.mkv");
    std::fs::write(&path, b"scanner observed this actual source").expect("source");
    let file = super::sampled_file(path.clone());
    std::fs::remove_file(&path).expect("actual native NotFound");
    let keeper: Arc<dyn OriginalWorkKeeper> = admission.clone();
    let error = bounded::with_original_work_keeper(keeper, open_source_fence(&file, None))
        .await
        .err()
        .expect("known actual open failure");
    assert!(error.starts_with(&format!("opening {}:", path.display())));
    assert!(
        !serving.restart_drain_status(0).await.new_admissions_blocked,
        "known native Err incorrectly became an unobserved result"
    );
    admission.retain_uncertainty(OriginalWorkUncertainty::CollectorDropped);
    let keeper: Arc<dyn OriginalWorkKeeper> = admission.clone();
    assert!(
        bounded::with_original_work_keeper(keeper, open_source_fence(&file, None))
            .await
            .is_err()
    );
    assert_unknown(&serving, 1).await;
    std::fs::write(&path, b"known replacement observed under existing policy")
        .expect("actual source");
    let keeper: Arc<dyn OriginalWorkKeeper> = admission.clone();
    let accepted = bounded::with_original_work_keeper(keeper, open_source_fence(&file, None))
        .await
        .expect("known actual File and metadata result");
    assert!(accepted
        .handle
        .metadata()
        .expect("same accepted File")
        .is_file());
    assert_unknown(&serving, 1).await;
    drop((accepted, admission));
    assert_unknown(&serving, 0).await;
}

#[tokio::test]
async fn unix_legacy_reopen_still_follows_links_and_conditionally_checks_scanner_identity() {
    assert!(
        bounded::original_keeper().is_none(),
        "unadmitted original path stays unadmitted"
    );
    let directory = crate::test_tempdir().expect("legacy policy root");
    let target = directory.path().join("actual-target.mkv");
    let link = directory.path().join("legacy-link.mkv");
    std::fs::write(&target, b"legacy path follows this real regular target")
        .expect("actual target");
    std::os::unix::fs::symlink(&target, &link).expect("real legacy symlink");
    let file = super::sampled_file(link);
    let held = open_source_fence(&file, None)
        .await
        .expect("preserved Unix follow-link policy");
    let mut stale = file.clone();
    stale.size += 1;
    stale.mtime += 1;
    let unchecked = open_source_fence(&stale, None)
        .await
        .expect("without expected version legacy path does not check scanner facts");
    assert_eq!(unchecked.object_version(), held.object_version());
    let error = open_source_fence(&stale, Some(held.object_version()))
        .await
        .err()
        .expect("expected-version path checks scanner facts");
    assert_eq!(
        error,
        "source no longer matches the scanner's size/mtime identity"
    );
    let reopened = held
        .reopen(&file)
        .await
        .expect("same legacy source can reopen");
    assert_eq!(
        plurx_core::fs_secure::std_file_identity(&held.handle).expect("original identity"),
        plurx_core::fs_secure::std_file_identity(&reopened.handle)
            .expect("actual reopened identity")
    );
    assert_eq!(
        open_source_fence(&file, Some("changed-version"))
            .await
            .err()
            .expect("changed identity refused"),
        "source changed after cluster index resolution"
    );
}
