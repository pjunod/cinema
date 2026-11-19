use super::*;
use plurx_core::process::fixed_probe::fixtures::{
    build_static_probe, HeldProbeWorker, JoinedProbeFixture,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const TEST_FACT_WORK: crate::process_control::ChildWork =
    crate::process_control::ChildWork::background("decode-fact test probe");

fn isolated(source: Arc<std::fs::File>) -> DecodeFactSource {
    new_source(
        source,
        Arc::new(tokio::sync::Semaphore::new(1)),
        TEST_FACT_WORK,
    )
}

#[tokio::test]
async fn strict_native_listener_accounts_alias_seek_and_scalar_reads_and_denies_bypasses() {
    let root = crate::test_tempdir().expect("create strict source test directory");
    for mode in 20..=23 {
        let (_store, _job, active) = crate::background_jobs::tests::active().await;
        let budget = active.fence().source_budget();
        let probe = root.path().join(format!("strict-reader-{mode}"));
        build_static_probe(&probe, mode);
        let identity =
            DecodeProbeIdentity::discover(probe.to_str().expect("temporary probe path is UTF-8"))
                .await
                .expect("discover strict native reader probe");
        let media = root.path().join(format!("source-{mode}.bin"));
        std::fs::write(&media, b"production-bound source").expect("write strict source fixture");
        let source = Arc::new(std::fs::File::open(media).expect("open strict source fixture"));
        let facts = crate::background_jobs::budgeted(Arc::clone(&budget), async {
            DecodeFactCache::new()
                .get_or_probe(
                    &identity,
                    isolated(source),
                    None,
                    ProbeStreamSelection::Absolute(4),
                    Duration::from_secs(5),
                    None,
                )
                .await
        })
        .await;
        if mode == 21 || mode == 22 {
            let error = facts.expect_err("read refusal cannot become codec evidence");
            assert!(matches!(error, DecodeFactError::SourceService(_)));
            assert_eq!(
                error.fallback_reason(),
                DecodePlanFallbackReason::IdentityIo
            );
        } else {
            assert_eq!(
                facts
                    .expect("supported strict reader returns codec facts")
                    .codec(),
                Some("h264")
            );
        }
        let charged = budget.current_reserved_test_bytes().await;
        match mode {
            10 => assert_eq!(
                charged,
                Some(5),
                "dup, reopen, pread and seek aliases all spend service"
            ),
            11 | 12 => assert_eq!(
                charged, None,
                "refused scalar/vector/mmap/copy operations issue no source read"
            ),
            13 => assert_eq!(
                charged,
                Some(1048576 + 32768),
                "short/EOF reads never refund the full immutable request"
            ),
            _ => unreachable!(),
        }
        active.finish().await;
    }
}

#[cfg(target_os = "linux")]
async fn strict_quota_stopped_thread_fixture(retain_daemon_activity: bool) {
    use plurx_core::{
        process::bounded, store::background_jobs_resources::BackgroundSourceServiceGrant,
    };
    let (_store, _job, active) = crate::background_jobs::tests::active().await;
    let budget = active.fence().source_budget();
    let now = crate::state::clock_ms();
    budget
        .install_test_service_grant(BackgroundSourceServiceGrant {
            receipt_id: uuid::Uuid::new_v4().to_string(),
            epoch_id: now / 5000,
            expires_at_ms: now + 60_000,
            byte_allowance: Some(0),
            resource_keys: vec!["source_io".into()],
        })
        .await;
    let root = crate::test_tempdir().expect("create strict source test directory");
    let probe = root.path().join("quota-blocked-threaded-parser");
    build_static_probe(&probe, 24);
    let identity =
        DecodeProbeIdentity::discover(probe.to_str().expect("temporary quota probe path is UTF-8"))
            .await
            .expect("discover strict quota probe");
    let media = root.path().join("source.bin");
    std::fs::write(&media, b"production-bound source").expect("write strict source fixture");
    let source = Arc::new(std::fs::File::open(media).expect("open strict source fixture"));
    let control = bounded::SourcePauseController::new(Duration::from_secs(10));
    let cancel = tokio_util::sync::CancellationToken::new();
    let activity = if retain_daemon_activity {
        Some(
            control
                .enlist(&cancel)
                .await
                .expect("register daemon source activity"),
        )
    } else {
        None
    };
    let task = tokio::spawn({
        let budget = Arc::clone(&budget);
        let control = control.clone();
        let cancel = cancel.clone();
        async move {
            bounded::pausable(
                control,
                crate::background_jobs::budgeted(budget, async {
                    DecodeFactCache::new()
                        .get_or_probe(
                            &identity,
                            isolated(source),
                            None,
                            ProbeStreamSelection::Absolute(4),
                            Duration::from_secs(8),
                            Some(&cancel),
                        )
                        .await
                }),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while budget.pacing_waiters() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual native broker is waiting for finite quota");
    control.request_pause();
    if let Some(activity) = activity {
        control.wait_active_budget(Duration::from_millis(350)).await;
        assert_eq!(control.observation().phase,bounded::SourcePausePhase::Stopping,
            "a retained daemon read prevents aggregate paused-time credit despite stopped native threads");
        drop(activity);
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        while control.observation().phase != bounded::SourcePausePhase::Paused {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("every actual parser thread and daemon read acknowledged stopped");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        !task.is_finished(),
        "verified quota hold must preserve the same parser operation"
    );
    let stopped_cancel = tokio::time::Instant::now();
    cancel.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("stopped parser cancellation settles within three seconds")
            .expect("stopped parser task joins without panic"),
        Err(DecodeFactError::Cancelled)
    );
    assert!(
        stopped_cancel.elapsed() < Duration::from_secs(3),
        "cancel must join stopped group and wake the existing broker quota callback"
    );
    assert_eq!(
        budget.pacing_waiters(),
        0,
        "no detached callback retains a budget future"
    );
    active.finish().await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn strict_native_quota_cancellation_joins_all_stopped_threads_and_broker() {
    strict_quota_stopped_thread_fixture(false).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn strict_native_group_hold_never_excludes_outstanding_daemon_read_time() {
    strict_quota_stopped_thread_fixture(true).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires exact packaged static parser PLURX_TEST_STRICT_FFPROBE and generated PLURX_TEST_STRICT_MEDIA"]
async fn packaged_static_ffprobe_uses_supported_bounded_source_read_syscalls() {
    let probe =
        std::env::var("PLURX_TEST_STRICT_FFPROBE").expect("supply packaged static parser artifact");
    // Existing discovery validates static ELF, immutable executable snapshot,
    // exact build digest and fresh bounded -version before collection.
    let identity = DecodeProbeIdentity::discover(&probe)
        .await
        .expect("qualify exact packaged parser identity");
    let media = PathBuf::from(
        std::env::var_os("PLURX_TEST_STRICT_MEDIA").expect("generated qualification media"),
    );
    let (_store, _job, active) = crate::background_jobs::tests::active().await;
    let budget = active.fence().source_budget();
    let source = Arc::new(
        plurx_core::fs_secure::open_read_nofollow(&media)
            .await
            .expect("open packaged source without following symlinks")
            .into_std()
            .await,
    );
    let facts = crate::background_jobs::budgeted(Arc::clone(&budget), async {
        DecodeFactCache::new()
            .get_or_probe(
                &identity,
                isolated(source),
                None,
                ProbeStreamSelection::FirstPlayable,
                Duration::from_secs(10),
                None,
            )
            .await
    })
    .await
    .expect("real packaged parser needs only permitted scalar reads and anonymous allocation");
    assert!(facts.codec().is_some());
    assert!(
        budget.current_reserved_test_bytes().await.unwrap_or(0) > 0,
        "actual parser source reads must enter shared service accounting"
    );
    active.finish().await;
}

// This is the explicit post-child-reap boundary, not another parser process.
// Both fixture threads are joined before assertions; the existing real-probe
// regression below retains end-to-end child/deadline/abandoned-waiter coverage.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn joined_raw_probe_retires_after_service_error_without_losing_the_error() {
    struct ReleaseWorker(std::sync::mpsc::Sender<()>);
    impl Drop for ReleaseWorker {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let (_store, _job, active) = crate::background_jobs::tests::active().await;
    let budget = active.fence().source_budget();
    let root = crate::test_tempdir().expect("post-reap source fixture");
    let source = Arc::new(std::fs::File::create(root.path().join("source")).expect("source"));
    let operation = budget
        .enroll_preparation(source)
        .expect("audited operation");
    let retirement = budget
        .raw_retirement()
        .expect("raw enrollment")
        .expect("completion ledger");
    let stop = Arc::new(AtomicBool::new(false));
    let read_cancel = tokio_util::sync::CancellationToken::new();
    let worker_stop = Arc::clone(&stop);
    let worker_cancel = read_cancel.clone();
    let cleanup_cancel = read_cancel.clone();
    let runtime = tokio::runtime::Handle::current();
    let (acknowledge, acknowledged) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let release = ReleaseWorker(release);
    let worker = std::thread::spawn(move || {
        runtime.block_on(worker_cancel.cancelled());
        let _ = acknowledge.send((
            worker_stop.load(Ordering::Acquire),
            worker_cancel.is_cancelled(),
        ));
        let _ = released.recv();
        Err(std::io::Error::other("joined worker IO sentinel"))
    });
    let expected = DecodeFactError::SourceService("stored service sentinel".into());
    let mut supervisor = JoinedProbeFixture::new(
        Box::new(retirement),
        stop,
        read_cancel,
        worker,
        expected.clone(),
    );
    // The controller records the still-held worker's state before releasing
    // it. A channel error also drops the release guard; no assertion can strand
    // a thread, and scope joins the controller before returning observations.
    let (first, after_first, second, after_second, held) = std::thread::scope(|scope| {
        let observing = Arc::clone(&budget);
        let controller = scope.spawn(move || {
            let acknowledgement = acknowledged.recv_timeout(Duration::from_secs(5));
            if acknowledgement.is_err() {
                // Failed observation is not settlement. Unblock the fixture
                // worker, then still require the actual finish/join below.
                cleanup_cancel.cancel();
            }
            let held = observing.completion_observation();
            drop(release);
            (acknowledgement, held)
        });
        let first = supervisor.finish_joined();
        let after_first = budget.completion_observation();
        let second = supervisor.finish_joined();
        let after_second = budget.completion_observation();
        (first, after_first, second, after_second, controller.join())
    });
    drop(supervisor);
    let next = budget.raw_retirement();
    let next_enrolled = next
        .as_ref()
        .map(|value| value.is_some())
        .map_err(Clone::clone);
    let during_next = budget.completion_observation();
    if let Ok(Some(retirement)) = next {
        // No child or worker was launched by this fresh registration.
        retirement.joined();
    }
    let after_next = budget.completion_observation();
    drop(operation);
    let retired = budget.completion_observation();
    active.finish().await;

    let (acknowledgement, held) = held.expect("joined fixture controller");
    assert_eq!(
        acknowledgement.expect("worker stop acknowledgement"),
        (true, true)
    );
    assert_eq!(held, Some((1, 1, false)), "worker is still physically held");
    assert_eq!(
        first,
        Err(expected),
        "stored service error keeps priority over worker IO error"
    );
    assert_eq!(
        after_first,
        Some((1, 0, false)),
        "error does not retain joined raw custody"
    );
    assert_eq!(second, Ok(()));
    assert_eq!(
        after_second, after_first,
        "retirement is recorded exactly once"
    );
    assert_eq!(
        next_enrolled,
        Ok(true),
        "the ledger still accepts the next registration"
    );
    assert_eq!(during_next, Some((1, 1, false)));
    assert_eq!(after_next, Some((1, 0, false)));
    assert_eq!(retired, Some((0, 0, false)));
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracked_raw_probe_retirement_survives_timeout_and_abandoned_collector() {
    let root = crate::test_tempdir().expect("probe fixture");
    let probe_path = root.path().join("static-ffprobe");
    build_static_probe(&probe_path, 0);
    let mut probe = DecodeProbeIdentity::discover(probe_path.to_str().expect("probe path"))
        .await
        .expect("actual production probe");
    let release_worker = HeldProbeWorker::hold(&mut probe);
    let (_store, _job, active) = crate::background_jobs::tests::active().await;
    let budget = active.fence().source_budget();
    let mut original = tempfile::tempfile().expect("held original");
    std::io::Write::write_all(&mut original, b"source").expect("real source bytes");
    let source = Arc::new(original);
    let operation = budget
        .enroll_preparation(Arc::clone(&source))
        .expect("audited operation");
    let held = DecodeFactSource::new(
        Arc::clone(&source),
        Arc::new(tokio::sync::Semaphore::new(1)),
        crate::process_control::ChildWork::background("tracked raw collector regression"),
    )
    .with_source_scope(None, Some(Arc::clone(&budget)));
    let query = tokio::spawn(async move {
        DecodeFactCache::new()
            .get_or_probe(
                &probe,
                held,
                None,
                ProbeStreamSelection::LegacyVideoOrdinal(0),
                Duration::from_millis(200),
                None,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !release_worker.started() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual supervisor owns source-bearing child");
    assert_eq!(budget.completion_observation(), Some((1, 1, false)));
    // Abandon the caller while the existing owned collector handles deadline
    // and actual child/worker cleanup. Its retirement guard must not disappear.
    query.abort();
    assert!(query.await.expect_err("abandoned waiter").is_cancelled());
    drop((operation, source));
    assert_eq!(
        budget.completion_observation(),
        Some((0, 1, false)),
        "the detached collector still owns retirement"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !release_worker.cleaning_up() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owned collector reaches actual deadline cleanup");
    assert_eq!(
        budget.completion_observation(),
        Some((0, 1, false)),
        "worker remains charged during deadline child reap"
    );
    drop(release_worker);
    tokio::time::timeout(Duration::from_secs(5), async {
        while budget.completion_observation().expect("completion owner").1 != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "explicit child reap and worker join retire the collector: {error:?}; final completion (operations, raw, veto) = {:?}",
            budget.completion_observation()
        )
    });
    assert_eq!(budget.completion_observation(), Some((0, 0, false)));
    active.finish().await;
}
