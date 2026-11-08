use super::*;
use crate::serving_fence::ServingFence;
use plurx_core::cluster::migration::status::ReplicationMonitor;
use tokio::io::AsyncWriteExt;

async fn fixture(
    id: &str,
) -> (
    Arc<CaptureOwners>,
    CaptureLease,
    ServingFence,
    tempfile::TempDir,
) {
    let fence = ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
    let admission = fence
        .try_restart_admission()
        .await
        .expect("same original admission");
    let root = CaptureOwners::new();
    let lease = root
        .reserve(id, 1, admission)
        .expect("original capture slot");
    (
        root,
        lease,
        fence,
        tempfile::tempdir().expect("original recording root"),
    )
}
async fn drained(root: &CaptureOwners) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            root.drive();
            if root
                .entries
                .lock()
                .expect("fixture operation succeeds: drained")
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("same original native operations and close joined");
}

#[tokio::test]
async fn unobserved_original_open_keeps_file_and_same_admission() {
    let (root, lease, fence, dir) = fixture("unobserved-open").await;
    let gate = Arc::new(BlockingGate::default());
    *lease.entry.native.hooks.after_open.lock().expect(
        "fixture operation succeeds: unobserved original open keeps file and same admission",
    ) = Some(Arc::clone(&gate));
    let entry = Arc::downgrade(&lease.entry);
    // Same production acquisition operation; its original handle is installed
    // and its original file is published before this gate can be entered.
    lease
        .entry
        .native
        .start_open(dir.path().join("attempt.part"), false)
        .expect(
            "fixture operation succeeds: unobserved original open keeps file and same admission",
        );
    gate.entered().await;
    let original = lease
        .entry
        .native
        .state
        .lock()
        .expect(
            "fixture operation succeeds: unobserved original open keeps file and same admission",
        )
        .original
        .as_ref()
        .expect(
            "fixture operation succeeds: unobserved original open keeps file and same admission",
        )
        .id();
    drop(lease); // ingress cancelled before observing its acquired File
    root.drive();
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
    let retained = entry.upgrade().expect("actual original entry retained");
    assert_eq!(
        retained
            .native
            .state
            .lock()
            .expect("fixture operation succeeds: unobserved original open keeps file and same admission")
            .original
            .as_ref()
            .expect("fixture operation succeeds: unobserved original open keeps file and same admission")
            .id(),
        original
    );
    drop(retained);
    gate.release();
    drop(entry); // original Arc header cannot retire under an escaped Weak
    drained(&root).await;
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 0);
    root.shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect(
            "fixture operation succeeds: unobserved original open keeps file and same admission",
        );
}

#[tokio::test]
async fn sync_observer_drop_keeps_same_original_native_job_until_close() {
    let (root, lease, fence, dir) = fixture("same-sync").await;
    let mut writer = lease.open(dir.path().join("attempt.part")).await.expect(
        "fixture operation succeeds: sync observer drop keeps same original native job until close",
    );
    writer
        .write_all(b"complete original recording")
        .await
        .expect(
        "fixture operation succeeds: sync observer drop keeps same original native job until close",
    );
    let gate = Arc::new(BlockingGate::default());
    *lease.entry.native.hooks.sync.lock().expect(
        "fixture operation succeeds: sync observer drop keeps same original native job until close",
    ) = Some(Arc::clone(&gate));
    let native = Arc::downgrade(&lease.entry.native);
    {
        let sync = writer.sync_all();
        tokio::pin!(sync);
        assert!(sync.as_mut().now_or_never().is_none());
    } // original job is not the observational future
    gate.entered().await;
    let original = lease
        .entry
        .native
        .state
        .lock()
        .expect("fixture operation succeeds: sync observer drop keeps same original native job until close")
        .original
        .as_ref()
        .expect("fixture operation succeeds: sync observer drop keeps same original native job until close")
        .id();
    drop(writer);
    drop(lease);
    root.drive();
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
    let held = native.upgrade().expect(
        "fixture operation succeeds: sync observer drop keeps same original native job until close",
    );
    assert_eq!(
        held.state.lock().expect("fixture operation succeeds: sync observer drop keeps same original native job until close").original.as_ref().expect("fixture operation succeeds: sync observer drop keeps same original native job until close").id(),
        original
    );
    assert_eq!(held.hooks.close_calls.load(Ordering::Acquire), 0);
    drop(held);
    drop(native);
    gate.release();
    drained(&root).await;
    assert_eq!(
        std::fs::read(dir.path().join("attempt.part")).expect("fixture operation succeeds: sync observer drop keeps same original native job until close"),
        b"complete original recording"
    );
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 0);
    root.shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("fixture operation succeeds: sync observer drop keeps same original native job until close");
}

#[tokio::test]
async fn post_open_panic_retains_actual_file_original_handle_and_admission() {
    let (root, lease, fence, dir) = fixture("published-panic").await;
    lease
        .entry
        .native
        .hooks
        .panic_after_open
        .store(true, Ordering::Release);
    lease
        .entry
        .native
        .start_open(dir.path().join("attempt.part"), false)
        .expect("fixture operation succeeds: post open panic retains actual file original handle and admission");
    drop(lease);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            root.drive();
            let failed = root.entries.lock().expect("fixture operation succeeds: post open panic retains actual file original handle and admission")[0]
                .native
                .state
                .lock()
                .expect("fixture operation succeeds: post open panic retains actual file original handle and admission")
                .failure
                .is_some();
            if failed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixture operation succeeds: post open panic retains actual file original handle and admission");
    for _ in 0..3 {
        root.drive();
    }
    {
        let entries = root.entries.lock().expect("fixture operation succeeds: post open panic retains actual file original handle and admission");
        let s = entries[0].native.state.lock().expect("fixture operation succeeds: post open panic retains actual file original handle and admission");
        assert!(
            s.original.is_some(),
            "SAME completed failed original handle retained"
        );
        assert!(
            s.file
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some(),
            "actual acquired File retained after worker panic"
        );
    }
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
    assert!(root.shutdown(tokio::time::Instant::now()).await.is_err());
    // Explicit fixture teardown consumes the actual original File. Production
    // does not turn an unknown failed join into a positive close receipt.
    let entries = root.entries.lock().expect("fixture operation succeeds: post open panic retains actual file original handle and admission");
    let s = entries[0].native.state.lock().expect("fixture operation succeeds: post open panic retains actual file original handle and admission");
    drop(
        s.file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .map(close_original),
    );
}

#[tokio::test]
async fn uncertain_original_close_is_never_retried_or_refunded() {
    let (root, lease, fence, dir) = fixture("uncertain-close").await;
    let writer = lease.open(dir.path().join("attempt.part")).await.expect(
        "fixture operation succeeds: uncertain original close is never retried or refunded",
    );
    lease
        .entry
        .native
        .hooks
        .fail_close
        .store(true, Ordering::Release);
    drop(writer);
    drop(lease);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            root.drive();
            if root.entries.lock().expect(
                "fixture operation succeeds: uncertain original close is never retried or refunded",
            )[0]
            .native
            .state
            .lock()
            .expect(
                "fixture operation succeeds: uncertain original close is never retried or refunded",
            )
            .close_uncertainty
            .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixture operation succeeds: uncertain original close is never retried or refunded");
    for _ in 0..5 {
        root.drive();
    }
    {
        let entries = root.entries.lock().expect(
            "fixture operation succeeds: uncertain original close is never retried or refunded",
        );
        assert_eq!(
            entries[0].native.hooks.close_calls.load(Ordering::Acquire),
            1
        );
        assert!(entries[0]
            .native
            .state
            .lock()
            .expect(
                "fixture operation succeeds: uncertain original close is never retried or refunded"
            )
            .original
            .is_some());
    }
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
    assert!(!root.permits_finalization("uncertain-close", 1));
}

#[tokio::test]
async fn storage_marker_and_attempt_file_have_separate_original_positive_closes() {
    let (root, lease, fence, dir) = fixture("identity-and-attempt").await;
    let identity = lease
        .storage_identity(dir.path().to_str().expect("fixture operation succeeds: storage marker and attempt file have separate original positive closes"))
        .await
        .expect("fixture operation succeeds: storage marker and attempt file have separate original positive closes");
    assert_eq!(identity.len(), 32);
    assert_eq!(
        lease
            .entry
            .identity
            .hooks
            .close_calls
            .load(Ordering::Acquire),
        1
    );
    assert!(lease.entry.identity.state.lock().expect("fixture operation succeeds: storage marker and attempt file have separate original positive closes").closed);
    let mut writer = lease.open(dir.path().join("attempt.part")).await.expect("fixture operation succeeds: storage marker and attempt file have separate original positive closes");
    writer.write_all(b"attempt").await.expect("fixture operation succeeds: storage marker and attempt file have separate original positive closes");
    let native = Arc::clone(&lease.entry.native);
    drop(writer);
    drop(lease);
    // An escaped actual NativeFile alias keeps the original entry/admission;
    // file closure alone cannot refund its still-owned original metadata.
    root.drive();
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
    drop(native);
    drained(&root).await;
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 0);
    root.shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("fixture operation succeeds: storage marker and attempt file have separate original positive closes");
}

#[tokio::test]
async fn same_root_finalization_fence_blocks_capture_admission_without_side_effects() {
    let root = CaptureOwners::new();
    let fence = ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
    let guard = root
        .begin_finalization(
            "sealed-recording",
            1,
            fence.try_restart_admission().await.expect("fixture operation succeeds: same root finalization fence blocks capture admission without side effects"),
        )
        .expect("fixture operation succeeds: same root finalization fence blocks capture admission without side effects");
    let admission = fence.try_restart_admission().await.expect("fixture operation succeeds: same root finalization fence blocks capture admission without side effects");
    assert!(root.reserve("sealed-recording", 2, admission).is_err());
    assert!(root.entries.lock().expect("fixture operation succeeds: same root finalization fence blocks capture admission without side effects").is_empty());
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
    drop(guard);
    root.drive();
    let admission = fence.try_restart_admission().await.expect("fixture operation succeeds: same root finalization fence blocks capture admission without side effects");
    let accepted = root.reserve("sealed-recording", 2, admission).expect("fixture operation succeeds: same root finalization fence blocks capture admission without side effects");
    drop(accepted);
    drained(&root).await;
    root.shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("fixture operation succeeds: same root finalization fence blocks capture admission without side effects");
}

#[tokio::test]
async fn losing_the_last_external_root_preserves_the_actual_original_graph() {
    let (root, lease, fence, dir) = fixture("lost-root").await;
    let gate = Arc::new(BlockingGate::default());
    *lease.entry.native.hooks.after_open.lock().expect("fixture operation succeeds: losing the last external root preserves the actual original graph") = Some(Arc::clone(&gate));
    let entry = Arc::downgrade(&lease.entry);
    lease
        .entry
        .native
        .start_open(dir.path().join("attempt.part"), false)
        .expect("fixture operation succeeds: losing the last external root preserves the actual original graph");
    gate.entered().await;
    let external = Arc::downgrade(&root);
    drop(lease);
    drop(root);
    tokio::time::timeout(Duration::from_secs(5), async {
        while external.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixture operation succeeds: losing the last external root preserves the actual original graph");
    let held = entry
        .upgrade()
        .expect("same actual root entries preserved after root Drop");
    assert!(held.native.state.lock().expect("fixture operation succeeds: losing the last external root preserves the actual original graph").original.is_some());
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
    gate.release();
    // Fixture can finish the SAME original operation and native close; no
    // replacement root/receipt is manufactured or released by production Drop.
    tokio::time::timeout(
        Duration::from_secs(5),
        std::future::poll_fn(|cx| held.native.poll_close(cx)),
    )
    .await
    .expect("fixture operation succeeds: losing the last external root preserves the actual original graph");
    assert_eq!(held.native.hooks.close_calls.load(Ordering::Acquire), 1);
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
}

#[tokio::test]
async fn full_original_owner_table_refuses_before_any_native_acquisition() {
    let root = CaptureOwners::new();
    let fence = ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
    let mut leases = Vec::new();
    for n in 0..CAPTURE_SLOTS {
        let admission = fence.try_restart_admission().await.expect("fixture operation succeeds: full original owner table refuses before any native acquisition");
        leases.push(
            root.reserve(&format!("original-{n}"), 1, admission)
                .expect("fixture operation succeeds: full original owner table refuses before any native acquisition"),
        );
    }
    let admission = fence.try_restart_admission().await.expect("fixture operation succeeds: full original owner table refuses before any native acquisition");
    assert!(root.reserve("refused", 1, admission).is_err());
    assert!(root.entries.lock().expect("fixture operation succeeds: full original owner table refuses before any native acquisition").iter().all(|e| e
        .native
        .state
        .lock()
        .expect("fixture operation succeeds: full original owner table refuses before any native acquisition")
        .original
        .is_none()
        && e.identity.state.lock().expect("fixture operation succeeds: full original owner table refuses before any native acquisition").original.is_none()));
    assert_eq!(
        fence.restart_drain_status(0).await.admissions_in_flight,
        CAPTURE_SLOTS as u64
    );
    drop(leases);
    drained(&root).await;
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 0);
    root.shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("fixture operation succeeds: full original owner table refuses before any native acquisition");
}

async fn recording_with_pending_original_close() -> (
    Arc<super::super::super::LiveTvManager>,
    super::super::super::LiveTvConfig,
    super::super::super::DvrConfig,
    plurx_core::dvr::DvrRecording,
    Arc<BlockingGate>,
    tempfile::TempDir,
) {
    use plurx_core::live_tv_resource::{Capture, Command, Outcome, Record};
    let (manager, live, dvr, now, target, dir) =
        super::super::tests::oversized_scheduler_fixture().await;
    manager
        .store
        .put_settings(&[
            ("live_tv.enabled", "1"),
            ("live_tv.config_generation", "7"),
            (plurx_core::store::keys::DVR_ROOT, dvr.root.as_str()),
            (plurx_core::store::keys::DVR_ENABLED, "1"),
        ])
        .await
        .expect("fixture operation succeeds: recording with pending original close");
    manager
        .dvr_expand(&live, &dvr, now)
        .await
        .expect("fixture operation succeeds: recording with pending original close");
    let row = manager
        .store
        .get_dvr_recording_for_airing("63.1", target)
        .await
        .expect("fixture operation succeeds: recording with pending original close")
        .expect("fixture operation succeeds: recording with pending original close");
    let base = dir.path().join("pending-original");
    let admission = manager
        .serving
        .serving
        .try_restart_admission()
        .await
        .expect("fixture operation succeeds: recording with pending original close");
    let lease = manager
        .capture_owners
        .reserve(&row.id, 1, admission)
        .expect("fixture operation succeeds: recording with pending original close");
    let storage_id = lease
        .storage_identity(&dvr.root)
        .await
        .expect("fixture operation succeeds: recording with pending original close");
    let capture = Capture {
        recording_id: row.id.clone(),
        epoch: 0,
        worker: manager.resource_worker(),
        ingest_id: String::new(),
        ingest_epoch: 0,
        storage_id,
        base_path: base.to_string_lossy().into_owned(),
        generation: live.generation,
        expires_at_ms: 0,
        stopped: false,
        deleted: false,
        finalizer: None,
        finalizer_epoch: 0,
        published_path: None,
    };
    let outcome = manager
        .store
        .live_tv_resource_command(
            Command::ClaimCapture {
                capture,
                ingest_id: "original-expired-ingest".into(),
                limit: 1,
                reserve: 0,
                device_id: "fixture-device".into(),
                channel_id: row.channel_id.clone(),
            },
            row.capture_start * 1000 + 1,
        )
        .await
        .expect("fixture operation succeeds: recording with pending original close");
    assert!(matches!(outcome, Outcome::Record(Record::Capture(ref c)) if c.epoch == 1));
    // Real committed Capture expiry0; no elapsed-time inference or sleep.
    manager
        .store
        .live_tv_resource_command(
            Command::DetachCapture {
                recording_id: row.id.clone(),
                epoch: 1,
                worker: manager.resource_worker(),
            },
            0,
        )
        .await
        .expect("fixture operation succeeds: recording with pending original close");
    let expired = manager
        .resource_capture(&row.id)
        .await
        .expect("fixture operation succeeds: recording with pending original close")
        .expect("fixture operation succeeds: recording with pending original close");
    assert_eq!(expired.expires_at_ms, 0);
    assert!(expired.finalizer.is_none());
    let row = manager
        .store
        .get_dvr_recording(&row.id)
        .await
        .expect("fixture operation succeeds: recording with pending original close")
        .expect("fixture operation succeeds: recording with pending original close");
    let mut writer = lease
        .open(super::super::attempt_path(&base, 1))
        .await
        .expect("fixture operation succeeds: recording with pending original close");
    writer
        .write_all(b"original part cannot be assembled yet")
        .await
        .expect("fixture operation succeeds: recording with pending original close");
    let gate = Arc::new(BlockingGate::default());
    *lease
        .entry
        .native
        .hooks
        .close
        .lock()
        .expect("fixture operation succeeds: recording with pending original close") =
        Some(Arc::clone(&gate));
    drop(writer);
    drop(lease);
    gate.entered().await;
    (manager, live, dvr, row, gate, dir)
}

#[tokio::test]
async fn disabled_recording_finalization_waits_for_original_native_close() {
    use plurx_core::dvr::DvrState;
    let (manager, live, _dvr, row, gate, _dir) = recording_with_pending_original_close().await;
    let (events, _queue) = super::super::super::webhook::channel();
    manager.finish_disabled_recordings(&live, &events).await;
    let current = manager
        .store
        .get_dvr_recording(&row.id)
        .await
        .expect("fixture operation succeeds: disabled recording finalization waits for original native close")
        .expect("fixture operation succeeds: disabled recording finalization waits for original native close");
    assert_eq!(current.state, DvrState::Recording);
    assert!(
        manager
            .resource_capture(&row.id)
            .await
            .expect("fixture operation succeeds: disabled recording finalization waits for original native close")
            .expect("fixture operation succeeds: disabled recording finalization waits for original native close")
            .finalizer
            .is_none(),
        "disabled branch did not claim a finalizer"
    );
    assert!(
        matches!(
            manager
                .finish_row(
                    &row,
                    &events,
                    row.capture_end,
                    0,
                    None,
                    "disabled",
                    live.generation
                )
                .await,
            Err(super::super::super::LiveTvError::OwnerUnavailable(_))
        ),
        "common finalizer fence independently refuses"
    );
    gate.release();
    drained(&manager.capture_owners).await;
    manager.finish_disabled_recordings(&live, &events).await;
    let published = manager
        .store
        .get_dvr_recording(&row.id)
        .await
        .expect("fixture operation succeeds: disabled recording finalization waits for original native close")
        .expect("fixture operation succeeds: disabled recording finalization waits for original native close");
    assert!(matches!(
        published.state,
        DvrState::Done | DvrState::Partial
    ));
    assert!(std::path::Path::new(published.path.as_ref().expect("fixture operation succeeds: disabled recording finalization waits for original native close")).exists());
    manager
        .capture_owners
        .shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("fixture operation succeeds: disabled recording finalization waits for original native close");
}

#[tokio::test]
async fn expired_actual_capture_lease_cannot_bypass_original_native_close() {
    use plurx_core::dvr::DvrState;
    let (manager, live, dvr, row, gate, _dir) = recording_with_pending_original_close().await;
    assert!(
        manager.live_recording_ids().is_empty(),
        "registry/live filter supplies no closure proof"
    );
    assert_eq!(
        manager
            .resource_capture(&row.id)
            .await
            .expect("fixture operation succeeds: expired actual capture lease cannot bypass original native close")
            .expect("fixture operation succeeds: expired actual capture lease cannot bypass original native close")
            .expires_at_ms,
        0,
        "actual committed Capture lease is expired"
    );
    let before = manager
        .store
        .list_dvr_events(&row.id, None, None, 100)
        .await
        .expect("fixture operation succeeds: expired actual capture lease cannot bypass original native close")
        .rows
        .len();
    let (events, _queue) = super::super::super::webhook::channel();
    manager
        .dvr_recover(&live, &dvr, &events, live.generation, row.capture_end + 1)
        .await
        .expect("fixture operation succeeds: expired actual capture lease cannot bypass original native close");
    let current = manager
        .store
        .get_dvr_recording(&row.id)
        .await
        .expect("fixture operation succeeds: expired actual capture lease cannot bypass original native close")
        .expect("fixture operation succeeds: expired actual capture lease cannot bypass original native close");
    assert_eq!((current.state, current.attempt), (DvrState::Recording, 1));
    assert_eq!(
        manager
            .store
            .list_dvr_events(&row.id, None, None, 100)
            .await
            .expect("fixture operation succeeds: expired actual capture lease cannot bypass original native close")
            .rows
            .len(),
        before,
        "no worker-lost/retry/finalization mutation before same original close"
    );
    gate.release();
    drained(&manager.capture_owners).await;
    manager
        .dvr_recover(&live, &dvr, &events, live.generation, row.capture_end + 1)
        .await
        .expect("fixture operation succeeds: expired actual capture lease cannot bypass original native close");
    let published = manager
        .store
        .get_dvr_recording(&row.id)
        .await
        .expect("fixture operation succeeds: expired actual capture lease cannot bypass original native close")
        .expect("fixture operation succeeds: expired actual capture lease cannot bypass original native close");
    assert!(matches!(
        published.state,
        DvrState::Done | DvrState::Partial
    ));
    assert_eq!(published.attempt, 1);
    assert!(std::path::Path::new(published.path.as_ref().expect("fixture operation succeeds: expired actual capture lease cannot bypass original native close")).exists());
    manager
        .capture_owners
        .shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("fixture operation succeeds: expired actual capture lease cannot bypass original native close");
}

#[tokio::test]
async fn closed_outer_entry_alias_keeps_slot_admission_and_shutdown_unsettled() {
    let (root, lease, fence, dir) = fixture("outer-alias").await;
    let writer = lease.open(dir.path().join("attempt.part")).await.expect("fixture operation succeeds: closed outer entry alias keeps slot admission and shutdown unsettled");
    lease.install_writer(tokio::spawn(async move {
        drop(writer);
        SinkWriterOutcome::Closed { bytes: 0 }
    }));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !lease.settled() {
            root.drive();
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixture operation succeeds: closed outer entry alias keeps slot admission and shutdown unsettled");
    assert_eq!(root.entries.lock().expect("fixture operation succeeds: closed outer entry alias keeps slot admission and shutdown unsettled").len(), 1);
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
    let mut others = Vec::new();
    for n in 1..CAPTURE_SLOTS {
        others.push(
            root.reserve(
                &format!("alias-capacity-{n}"),
                1,
                fence.try_restart_admission().await.expect("fixture operation succeeds: closed outer entry alias keeps slot admission and shutdown unsettled"),
            )
            .expect("fixture operation succeeds: closed outer entry alias keeps slot admission and shutdown unsettled"),
        );
    }
    assert!(root
        .reserve("no-refund", 1, fence.try_restart_admission().await.expect("fixture operation succeeds: closed outer entry alias keeps slot admission and shutdown unsettled"))
        .is_err());
    assert!(root.shutdown(tokio::time::Instant::now()).await.is_err());
    assert_eq!(root.entries.lock().expect("fixture operation succeeds: closed outer entry alias keeps slot admission and shutdown unsettled").len(), CAPTURE_SLOTS);
    drop(others);
    drop(lease);
    drained(&root).await;
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 0);
    let _ = root.shutdown(tokio::time::Instant::now()).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while root.driver.observe() != JoinState::Joined {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixture operation succeeds: closed outer entry alias keeps slot admission and shutdown unsettled");
    root.shutdown(tokio::time::Instant::now()).await.expect("fixture operation succeeds: closed outer entry alias keeps slot admission and shutdown unsettled");
}

#[tokio::test]
async fn writer_panic_keeps_the_original_inflight_native_job_and_file() {
    let (root, lease, fence, dir) = fixture("writer-panic").await;
    let mut writer = lease.open(dir.path().join("attempt.part")).await.expect(
        "fixture operation succeeds: writer panic keeps the original inflight native job and file",
    );
    let gate = Arc::new(BlockingGate::default());
    *lease.entry.native.hooks.write.lock().expect(
        "fixture operation succeeds: writer panic keeps the original inflight native job and file",
    ) = Some(Arc::clone(&gate));
    let (enter, registered) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        registered.await.expect("fixture operation succeeds: writer panic keeps the original inflight native job and file");
        let write = writer.write_all(b"original pending bytes");
        tokio::pin!(write);
        assert!(write.as_mut().now_or_never().is_none());
        panic!("injected outer writer panic with native job still owned");
        #[allow(unreachable_code)]
        SinkWriterOutcome::Closed { bytes: 0 }
    });
    lease.install_writer(task);
    enter.send(()).expect(
        "fixture operation succeeds: writer panic keeps the original inflight native job and file",
    );
    gate.entered().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while lease.entry.writer.observe() != JoinState::Unknown {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect(
        "fixture operation succeeds: writer panic keeps the original inflight native job and file",
    );
    let original = lease
        .entry
        .native
        .state
        .lock()
        .expect("fixture operation succeeds: writer panic keeps the original inflight native job and file")
        .original
        .as_ref()
        .expect("fixture operation succeeds: writer panic keeps the original inflight native job and file")
        .id();
    assert_eq!(
        lease.entry.native.state.lock().expect("fixture operation succeeds: writer panic keeps the original inflight native job and file").inflight_bytes,
        b"original pending bytes".len() as u64
    );
    drop(lease);
    for _ in 0..3 {
        root.drive();
    }
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
    {
        let entries = root.entries.lock().expect("fixture operation succeeds: writer panic keeps the original inflight native job and file");
        assert_eq!(
            entries[0]
                .native
                .state
                .lock()
                .expect("fixture operation succeeds: writer panic keeps the original inflight native job and file")
                .original
                .as_ref()
                .expect("fixture operation succeeds: writer panic keeps the original inflight native job and file")
                .id(),
            original
        );
        assert!(entries[0]
            .native
            .state
            .lock()
            .expect("fixture operation succeeds: writer panic keeps the original inflight native job and file")
            .file
            .lock()
            .expect("fixture operation succeeds: writer panic keeps the original inflight native job and file")
            .is_some());
    }
    gate.release();
    let native = Arc::clone(&root.entries.lock().expect("fixture operation succeeds: writer panic keeps the original inflight native job and file")[0].native);
    assert!(tokio::time::timeout(
        Duration::from_secs(5),
        std::future::poll_fn(|cx| native.poll_close(cx))
    )
    .await
    .expect(
        "fixture operation succeeds: writer panic keeps the original inflight native job and file"
    ));
    assert_eq!(native.state.lock().expect("fixture operation succeeds: writer panic keeps the original inflight native job and file").inflight_bytes, 0);
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn actual_full_pipe_write_retains_original_payload_debit_after_observer_drop() {
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd};
    let (root, lease, fence, _dir) = fixture("full-pipe-write").await;
    let mut descriptors = [-1; 2];
    // SAFETY: successful pipe2 returns two distinct original owned descriptors.
    assert_eq!(
        unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    let mut reader = unsafe { std::fs::File::from_raw_fd(descriptors[0]) };
    let mut writer = unsafe { std::fs::File::from_raw_fd(descriptors[1]) };
    let capacity = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETPIPE_SZ) };
    assert!(capacity > 0);
    writer.write_all(&vec![1; capacity as usize]).expect("fixture operation succeeds: actual full pipe write retains original payload debit after observer drop");
    {
        let mut s = lease.entry.native.state.lock().expect("fixture operation succeeds: actual full pipe write retains original payload debit after observer drop");
        *s.file.lock().expect("fixture operation succeeds: actual full pipe write retains original payload debit after observer drop") = Some(writer);
        s.opened = true;
    }
    let native = Arc::clone(&lease.entry.native);
    let mut wrapper = NativeWriter {
        file: Arc::clone(&native),
    };
    let payload = [2_u8; 4096];
    {
        let write = wrapper.write_all(&payload);
        tokio::pin!(write);
        assert!(
            write.as_mut().now_or_never().is_none(),
            "full kernel pipe cannot complete without a reader"
        );
    }
    let original = native.state.lock().expect("fixture operation succeeds: actual full pipe write retains original payload debit after observer drop").original.as_ref().expect("fixture operation succeeds: actual full pipe write retains original payload debit after observer drop").id();
    drop(wrapper);
    drop(lease);
    root.drive();
    assert_eq!(
        native.state.lock().expect("fixture operation succeeds: actual full pipe write retains original payload debit after observer drop").original.as_ref().expect("fixture operation succeeds: actual full pipe write retains original payload debit after observer drop").id(),
        original
    );
    assert_eq!(
        native.state.lock().expect("fixture operation succeeds: actual full pipe write retains original payload debit after observer drop").inflight_bytes,
        payload.len() as u64
    );
    assert_eq!(native.hooks.close_calls.load(Ordering::Acquire), 0);
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
    let read = tokio::task::spawn_blocking(move || {
        let mut value = vec![0; capacity as usize + payload.len()];
        reader.read_exact(&mut value).expect("fixture operation succeeds: actual full pipe write retains original payload debit after observer drop");
        value
    });
    let bytes = read.await.expect("fixture operation succeeds: actual full pipe write retains original payload debit after observer drop");
    assert!(bytes[..capacity as usize].iter().all(|b| *b == 1));
    assert_eq!(&bytes[capacity as usize..], &payload);
    assert!(tokio::time::timeout(
        Duration::from_secs(5),
        std::future::poll_fn(|cx| native.poll_close(cx))
    )
    .await
    .expect("fixture operation succeeds: actual full pipe write retains original payload debit after observer drop"));
    assert_eq!(native.state.lock().expect("fixture operation succeeds: actual full pipe write retains original payload debit after observer drop").inflight_bytes, 0);
    assert_eq!(native.hooks.close_calls.load(Ordering::Acquire), 1);
    drop(native);
    drained(&root).await;
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 0);
    root.shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("fixture operation succeeds: actual full pipe write retains original payload debit after observer drop");
}

#[tokio::test]
async fn old_raw_sink_cannot_detach_a_new_unattached_original_acquisition() {
    use plurx_core::live_tv_resource::{Command, Outcome, Record};
    let (manager, live, _dvr, row, close_gate, dir) = recording_with_pending_original_close().await;
    close_gate.release();
    drained(&manager.capture_owners).await;
    let previous = manager.resource_capture(&row.id).await.expect("fixture operation succeeds: old raw sink cannot detach a new unattached original acquisition").expect("fixture operation succeeds: old raw sink cannot detach a new unattached original acquisition");
    let now = row.capture_start * 1000 + 2;
    manager
        .store
        .live_tv_resource_command(
            Command::Release {
                ingest_id: previous.ingest_id.clone(),
                epoch: previous.ingest_epoch,
                worker: manager.resource_worker(),
            },
            now,
        )
        .await
        .expect("fixture operation succeeds: old raw sink cannot detach a new unattached original acquisition");
    let lease = manager
        .capture_owners
        .reserve(
            &row.id,
            2,
            manager
                .serving
                .serving
                .try_restart_admission()
                .await
                .expect("fixture operation succeeds: old raw sink cannot detach a new unattached original acquisition"),
        )
        .expect("fixture operation succeeds: old raw sink cannot detach a new unattached original acquisition");
    let outcome = manager
        .store
        .live_tv_resource_command(
            Command::ClaimCapture {
                capture: previous,
                ingest_id: "new-unattached-ingest".into(),
                limit: 1,
                reserve: 0,
                device_id: "fixture-device".into(),
                channel_id: row.channel_id.clone(),
            },
            now + 1,
        )
        .await
        .expect("fixture operation succeeds: old raw sink cannot detach a new unattached original acquisition");
    assert!(
        matches!(outcome, Outcome::Record(Record::Capture(ref c)) if c.epoch == 2 && c.generation == live.generation)
    );
    let before = manager.resource_capture(&row.id).await.expect("fixture operation succeeds: old raw sink cannot detach a new unattached original acquisition").expect("fixture operation succeeds: old raw sink cannot detach a new unattached original acquisition");
    let old = super::super::tests::test_sink_attempt_at(
        &row.id,
        1,
        dir.path().join("pending-original"),
        Box::new(super::super::tests::MemoryWriter::default()),
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
        Arc::clone(&manager.metrics),
    );
    assert_eq!(
        super::super::settle_sink(&old).await,
        super::super::SinkStopResult::Settled
    );
    let transport = super::super::tests::test_transport(
        vec![Arc::clone(&old)],
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
        0,
    );
    manager
        .registry
        .lock()
        .expect("fixture operation succeeds: old raw sink cannot detach a new unattached original acquisition")
        .transports
        .insert("7.1".into(), transport);
    let gate = Arc::new(BlockingGate::default());
    *lease.entry.native.hooks.after_open.lock().expect("fixture operation succeeds: old raw sink cannot detach a new unattached original acquisition") = Some(Arc::clone(&gate));
    lease
        .entry
        .native
        .start_open(dir.path().join("new-original.a2.part"), false)
        .expect("fixture operation succeeds: old raw sink cannot detach a new unattached original acquisition");
    gate.entered().await;
    assert_eq!(
        manager.stop_sink(&row.id, 2).await,
        super::super::SinkStopResult::Settling
    );
    let after = manager.resource_capture(&row.id).await.expect("fixture operation succeeds: old raw sink cannot detach a new unattached original acquisition").expect("fixture operation succeeds: old raw sink cannot detach a new unattached original acquisition");
    assert_eq!(
        after.expires_at_ms, before.expires_at_ms,
        "raw old sink cannot expire the new actual Capture lease"
    );
    assert!(
        !lease.demand_alive(),
        "original acquisition receives same-root stop"
    );
    let late = dir.path().join("must-not-open-after-stop.part");
    assert!(lease.open(late.clone()).await.is_err());
    assert!(!late.exists());
    gate.release();
    drop(lease);
    drop(old);
    drained(&manager.capture_owners).await;
    manager.close_all_transports().await;
    manager
        .capture_owners
        .shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("fixture operation succeeds: old raw sink cannot detach a new unattached original acquisition");
}

#[tokio::test]
async fn stop_during_identity_read_cannot_race_the_live_acquisition_poller() {
    let (root, lease, fence, dir) = fixture("active-identity-read").await;
    let gate = Arc::new(BlockingGate::default());
    *lease.entry.identity.hooks.read.lock().expect("fixture operation succeeds: stop during identity read cannot race the live acquisition poller") = Some(Arc::clone(&gate));
    let path = dir.path().to_string_lossy().into_owned();
    let caller = tokio::spawn(async move {
        let result = lease.storage_identity(&path).await;
        (lease, result)
    });
    gate.entered().await;
    let original = root.entries.lock().expect("fixture operation succeeds: stop during identity read cannot race the live acquisition poller")[0]
        .identity
        .state
        .lock()
        .expect("fixture operation succeeds: stop during identity read cannot race the live acquisition poller")
        .original
        .as_ref()
        .expect("fixture operation succeeds: stop during identity read cannot race the live acquisition poller")
        .id();
    let stop = root.begin_stop("active-identity-read", 1).expect("fixture operation succeeds: stop during identity read cannot race the live acquisition poller");
    for _ in 0..3 {
        root.drive();
    }
    {
        let entries = root.entries.lock().expect("fixture operation succeeds: stop during identity read cannot race the live acquisition poller");
        let s = entries[0].identity.state.lock().expect("fixture operation succeeds: stop during identity read cannot race the live acquisition poller");
        assert_eq!(s.kind, Some(OpKind::Read));
        assert_eq!(s.original.as_ref().expect("fixture operation succeeds: stop during identity read cannot race the live acquisition poller").id(), original);
        assert_eq!(
            entries[0]
                .identity
                .hooks
                .close_calls
                .load(Ordering::Acquire),
            0,
            "cleanup cannot poll/consume a live acquisition's original output"
        );
        assert!(!entries[0].acquisition_done.load(Ordering::Acquire));
    }
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
    gate.release();
    let (lease, result) = tokio::time::timeout(Duration::from_secs(5), caller)
        .await
        .expect("fixture operation succeeds: stop during identity read cannot race the live acquisition poller")
        .expect("fixture operation succeeds: stop during identity read cannot race the live acquisition poller");
    assert_eq!(result.expect_err("expected refusal: stop during identity read cannot race the live acquisition poller").kind(), io::ErrorKind::Interrupted);
    assert!(!lease.demand_alive());
    drop(lease);
    drop(stop);
    drained(&root).await;
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 0);
    root.shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("fixture operation succeeds: stop during identity read cannot race the live acquisition poller");
}

#[tokio::test]
async fn unresolved_capture_shutdown_still_seals_and_drains_the_session_registry() {
    let (manager, _live, _dvr, _row, gate, _dir) = recording_with_pending_original_close().await;
    let original_deadline = tokio::time::Instant::now();
    manager.capture_owners.seal(original_deadline);
    assert!(manager.shutdown().await.is_err());
    assert!(
        manager.registry.lock().expect("fixture operation succeeds: unresolved capture shutdown still seals and drains the session registry").closing,
        "capture uncertainty cannot skip existing session shutdown"
    );
    assert_eq!(
        *manager.capture_owners.shutdown_deadline.lock().expect("fixture operation succeeds: unresolved capture shutdown still seals and drains the session registry"),
        Some(original_deadline),
        "later shutdown observer cannot renew the original deadline"
    );
    gate.release();
    drained(&manager.capture_owners).await;
    let _ = manager
        .capture_owners
        .shutdown(tokio::time::Instant::now())
        .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while manager.capture_owners.driver.observe() != JoinState::Joined {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixture operation succeeds: unresolved capture shutdown still seals and drains the session registry");
    manager
        .capture_owners
        .shutdown(tokio::time::Instant::now())
        .await
        .expect("fixture operation succeeds: unresolved capture shutdown still seals and drains the session registry");
}

#[tokio::test]
async fn deletion_waits_for_original_native_close_even_after_actual_lease_expiry() {
    let (manager, _live, _dvr, row, gate, _dir) = recording_with_pending_original_close().await;
    let claim = manager.resource_capture(&row.id).await.expect("fixture operation succeeds: deletion waits for original native close even after actual lease expiry").expect("fixture operation succeeds: deletion waits for original native close even after actual lease expiry");
    assert_eq!(claim.expires_at_ms, 0);
    let part = super::super::attempt_path(std::path::Path::new(&claim.base_path), claim.epoch);
    assert!(part.exists());
    assert!(!manager.delete_recording_files(&row).await);
    assert!(
        part.exists(),
        "an expired SQL lease cannot authorize unlink before actual close"
    );
    gate.release();
    drained(&manager.capture_owners).await;
    assert!(manager.delete_recording_files(&row).await);
    assert!(!part.exists());
    manager
        .capture_owners
        .shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("fixture operation succeeds: deletion waits for original native close even after actual lease expiry");
}

struct SqlFixture {
    manager: Arc<super::super::super::LiveTvManager>,
    root: Arc<CaptureOwners>,
    lease: CaptureLease,
    fence: ServingFence,
    authority: super::super::super::resource::CaptureAuthority,
    command: plurx_core::live_tv_resource::Command,
    row: plurx_core::dvr::DvrRecording,
    _dir: tempfile::TempDir,
}
async fn sql_fixture() -> SqlFixture {
    let (manager, live, dvr, now, target, dir) =
        super::super::tests::oversized_scheduler_fixture().await;
    manager
        .store
        .put_settings(&[("live_tv.enabled", "1"), ("live_tv.config_generation", "7")])
        .await
        .expect("enable actual SQL capture authority");
    manager
        .dvr_expand(&live, &dvr, now)
        .await
        .expect("create actual scheduled recording");
    let row = manager
        .store
        .get_dvr_recording_for_airing("63.1", target)
        .await
        .expect("read scheduled recording")
        .expect("actual scheduled row");
    let (root, lease, fence, _unused) = fixture(&row.id).await;
    let (mut authority, command) = manager.prepare_capture_claim(
        &live,
        &dvr,
        &row,
        "original-device",
        &dir.path().join("original"),
        "original-storage".into(),
    );
    authority.at_ms = Some(row.capture_start * 1000 + 1);
    SqlFixture {
        manager,
        root,
        lease,
        fence,
        authority,
        command,
        row,
        _dir: dir,
    }
}
fn sql_hooks(lease: &CaptureLease) -> Arc<SqlHooks> {
    Arc::clone(&lease.entry.sql.lock().expect("original SQL cell").hooks)
}
async fn sql_held(fence: &ServingFence, root: &CaptureOwners) {
    root.drive();
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
}

#[tokio::test]
async fn cancelled_original_sql_claim_commits_then_detaches_before_admission_release() {
    let SqlFixture {
        manager,
        root,
        lease,
        fence,
        authority,
        command,
        row,
        _dir,
    } = sql_fixture().await;
    let hooks = sql_hooks(&lease);
    hooks.before_claim.enabled.store(true, Ordering::Release);
    hooks.before_detach.enabled.store(true, Ordering::Release);
    {
        let observer = lease.claim(authority, command);
        tokio::pin!(observer);
        assert!(observer.as_mut().now_or_never().is_none());
    }
    hooks.before_claim.entered().await;
    drop(lease);
    sql_held(&fence, &root).await;
    assert!(manager
        .resource_capture(&row.id)
        .await
        .expect("pre-claim read")
        .is_none());
    hooks.before_claim.release.notify_one();
    hooks.before_detach.entered().await;
    sql_held(&fence, &root).await;
    let claimed = manager
        .resource_capture(&row.id)
        .await
        .expect("late actual claim")
        .expect("claim committed");
    assert_eq!(claimed.epoch, 1);
    assert!(!root.permits_finalization(&row.id, 1));
    hooks.before_detach.release.notify_one();
    drained(&root).await;
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 0);
    let capture = manager
        .resource_capture(&row.id)
        .await
        .expect("actual detach")
        .expect("history remains");
    assert!(capture.expires_at_ms < claimed.expires_at_ms);
    let ingest = manager
        .store
        .live_tv_resource_lookup(
            &format!("ingest:{}", claimed.ingest_id),
            super::super::super::resource::now_ms(),
        )
        .await
        .expect("remaining shared ingest authority")
        .expect("draining ingest is not fabricated absent");
    assert!(
        matches!(ingest, plurx_core::live_tv_resource::Record::Ingest(ref i)
        if i.draining && !i.consumers.contains_key(&format!("capture:{}", row.id)))
    );
    root.shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("original SQL drained");
}

#[tokio::test]
async fn cancelled_committed_sql_result_keeps_same_original_handle_until_positive_join() {
    let SqlFixture {
        manager,
        root,
        lease,
        fence,
        authority,
        command,
        row,
        _dir,
    } = sql_fixture().await;
    let hooks = sql_hooks(&lease);
    hooks.after_claim.enabled.store(true, Ordering::Release);
    {
        let observer = lease.claim(authority, command);
        tokio::pin!(observer);
        assert!(observer.as_mut().now_or_never().is_none());
    }
    hooks.after_claim.entered().await;
    assert!(manager
        .resource_capture(&row.id)
        .await
        .expect("committed original claim")
        .is_some());
    let original = lease.entry.sql.lock().expect("SQL cell").claim.identity();
    drop(lease);
    for _ in 0..32 {
        root.drive();
    }
    sql_held(&fence, &root).await;
    assert_eq!(
        root.entries.lock().expect("resident records")[0]
            .sql
            .lock()
            .expect("same SQL cell")
            .claim
            .identity(),
        original
    );
    assert!(!root.permits_finalization(&row.id, 1));
    hooks.after_claim.release.notify_one();
    drained(&root).await;
    root.shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("original result and detach joined");
}

#[tokio::test]
async fn active_sql_claim_is_not_detached_before_original_acquisition_and_native_close() {
    let SqlFixture {
        manager,
        root,
        lease,
        fence,
        authority,
        command,
        row,
        _dir,
    } = sql_fixture().await;
    let claim = lease
        .claim(authority, command)
        .await
        .expect("original claim");
    for _ in 0..32 {
        root.drive();
    }
    assert!(!lease.entry.sql.lock().expect("SQL cell").detach_started);
    let mut writer = lease
        .open(_dir.path().join("active.part"))
        .await
        .expect("actual original file");
    writer
        .write_all(b"original live acquisition")
        .await
        .expect("actual bytes");
    sql_held(&fence, &root).await;
    assert_eq!(
        manager
            .resource_capture(&row.id)
            .await
            .expect("live claim")
            .expect("capture")
            .expires_at_ms,
        claim.expires_at_ms
    );
    drop(writer);
    drop(claim);
    drop(lease);
    drained(&root).await;
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 0);
    root.shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("native file and exact detach closed");
}

#[tokio::test]
async fn concurrent_sql_cleanup_observers_keep_the_same_original_detach() {
    let SqlFixture {
        root,
        lease,
        fence,
        authority,
        command,
        ..
    } = sql_fixture().await;
    let hooks = sql_hooks(&lease);
    hooks.before_detach.enabled.store(true, Ordering::Release);
    drop(
        lease
            .claim(authority, command)
            .await
            .expect("original claim"),
    );
    drop(lease);
    hooks.before_detach.entered().await;
    let original = root.entries.lock().expect("records")[0]
        .sql
        .lock()
        .expect("SQL cell")
        .detach
        .identity();
    let mut observers = Vec::new();
    for _ in 0..8 {
        let root = Arc::clone(&root);
        observers.push(tokio::spawn(async move {
            for _ in 0..32 {
                root.drive();
                tokio::task::yield_now().await;
            }
        }));
    }
    for observer in observers {
        observer.await.expect("cleanup observer joined");
    }
    sql_held(&fence, &root).await;
    assert_eq!(
        root.entries.lock().expect("same records")[0]
            .sql
            .lock()
            .expect("same SQL cell")
            .detach
            .identity(),
        original
    );
    hooks.before_detach.release.notify_one();
    drained(&root).await;
    root.shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("same detach drained");
}

async fn failed_original_sql_retains_admission(claim_failure: bool, panic: bool) {
    let SqlFixture {
        manager,
        root,
        lease,
        fence,
        authority,
        command,
        row,
        _dir,
    } = sql_fixture().await;
    let hooks = sql_hooks(&lease);
    let flag = match (claim_failure, panic) {
        (true, true) => &hooks.panic_claim,
        (true, false) => &hooks.fail_claim,
        (false, true) => &hooks.panic_detach,
        (false, false) => &hooks.fail_detach,
    };
    flag.store(true, Ordering::Release);
    let result = lease.claim(authority, command).await;
    assert_eq!(result.is_err(), claim_failure);
    drop(result);
    drop(lease);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            root.drive();
            let finished = {
                let records = root.entries.lock().expect("retained original record");
                let sql = records[0].sql.lock().expect("retained SQL operation");
                if claim_failure {
                    sql.failed
                } else {
                    sql.detach_started && sql.detach.observe() != JoinState::Pending
                }
            };
            if finished {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("original failed operation completed");
    // Positive reads and a completed SQL mutation are not the missing original
    // acknowledgement. The same record and admission must remain Unknown.
    assert!(manager
        .resource_capture(&row.id)
        .await
        .expect("real committed history")
        .is_some());
    for _ in 0..64 {
        root.drive();
    }
    sql_held(&fence, &root).await;
    assert!(!root.permits_finalization(&row.id, i64::MAX));
    assert!(root.shutdown(tokio::time::Instant::now()).await.is_err());
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 1);
}
#[tokio::test]
async fn original_claim_panic_after_real_commit_keeps_restart_admission() {
    failed_original_sql_retains_admission(true, true).await;
}
#[tokio::test]
async fn ambiguous_original_claim_error_after_real_commit_keeps_restart_admission() {
    failed_original_sql_retains_admission(true, false).await;
}
#[tokio::test]
async fn original_detach_panic_after_real_commit_keeps_restart_admission() {
    failed_original_sql_retains_admission(false, true).await;
}
#[tokio::test]
async fn ambiguous_original_detach_error_after_real_commit_keeps_restart_admission() {
    failed_original_sql_retains_admission(false, false).await;
}

#[tokio::test]
async fn original_cleanup_detaches_the_claimed_epoch_instead_of_the_requested_attempt() {
    use plurx_core::live_tv_resource::{Command, Outcome, Record};
    let SqlFixture {
        manager,
        root,
        lease,
        fence,
        authority,
        command,
        row,
        _dir,
    } = sql_fixture().await;
    let predecessor = match authority
        .execute(command.clone())
        .await
        .expect("first committed epoch")
    {
        Outcome::Record(Record::Capture(c)) => c,
        other => panic!("actual predecessor capture required: {other:?}"),
    };
    assert_eq!(predecessor.epoch, 1);
    manager
        .store
        .live_tv_resource_command(
            Command::DetachCapture {
                recording_id: row.id.clone(),
                epoch: 1,
                worker: authority.worker.clone(),
            },
            0,
        )
        .await
        .expect("expire predecessor without changing its epoch");
    manager
        .store
        .live_tv_resource_command(
            Command::Release {
                ingest_id: predecessor.ingest_id,
                epoch: predecessor.ingest_epoch,
                worker: predecessor.worker,
            },
            0,
        )
        .await
        .expect("positively release the original now-draining ingest");
    let claim = lease
        .claim(authority, command)
        .await
        .expect("exact successor claim");
    assert_eq!(claim.epoch, 2);
    assert_eq!(lease.entry.attempt, 1);
    let expiry = claim.expires_at_ms;
    drop(claim);
    drop(lease);
    drained(&root).await;
    let detached = manager
        .resource_capture(&row.id)
        .await
        .expect("exact detached successor")
        .expect("capture history");
    assert_eq!(detached.epoch, 2);
    assert!(detached.expires_at_ms < expiry);
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 0);
    root.shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("claimed epoch drained");
}

#[tokio::test]
async fn restart_seal_during_original_sql_claim_does_not_renew_or_release_its_admission() {
    let SqlFixture {
        root,
        lease,
        fence,
        authority,
        command,
        ..
    } = sql_fixture().await;
    let hooks = sql_hooks(&lease);
    hooks.before_claim.enabled.store(true, Ordering::Release);
    {
        let observer = lease.claim(authority, command);
        tokio::pin!(observer);
        assert!(observer.as_mut().now_or_never().is_none());
    }
    hooks.before_claim.entered().await;
    let original = lease
        .entry
        .sql
        .lock()
        .expect("original SQL cell")
        .claim
        .identity();
    let cutoff = tokio::time::Instant::now();
    root.seal(cutoff);
    assert!(!lease.demand_alive());
    drop(lease);
    assert!(root
        .shutdown(cutoff + Duration::from_secs(20))
        .await
        .is_err());
    sql_held(&fence, &root).await;
    assert_eq!(
        *root.shutdown_deadline.lock().expect("original deadline"),
        Some(cutoff)
    );
    assert_eq!(
        root.entries.lock().expect("same original record")[0]
            .sql
            .lock()
            .expect("same original claim")
            .claim
            .identity(),
        original
    );
    hooks.before_claim.release.notify_one();
    drained(&root).await;
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 0);
    // A later observer may see actual completion; it may not extend the cutoff.
    assert_eq!(
        *root.shutdown_deadline.lock().expect("unchanged deadline"),
        Some(cutoff)
    );
}

#[tokio::test]
async fn sealed_capture_refuses_its_first_sql_claim_before_store_effects() {
    let SqlFixture {
        manager,
        root,
        lease,
        fence,
        authority,
        command,
        row,
        _dir,
    } = sql_fixture().await;
    let cutoff = tokio::time::Instant::now() + Duration::from_secs(5);
    root.seal(cutoff);
    assert!(lease.claim(authority, command).await.is_err());
    assert!(lease
        .entry
        .sql
        .lock()
        .expect("unentered original SQL cell")
        .authority
        .is_none());
    assert!(manager
        .resource_capture(&row.id)
        .await
        .expect("no SQL effect after seal")
        .is_none());
    sql_held(&fence, &root).await;
    drop(lease);
    root.shutdown(cutoff)
        .await
        .expect("unentered original admission drained");
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 0);
}

#[tokio::test]
async fn changed_incarnation_refuses_entry_but_detaches_the_original_sql_worker() {
    let SqlFixture {
        manager,
        root,
        lease,
        fence,
        authority,
        command,
        row,
        _dir,
    } = sql_fixture().await;
    let original_worker = authority.worker.clone();
    let claim = lease
        .claim(authority, command)
        .await
        .expect("positive original claim");
    let expiry = claim.expires_at_ms;
    manager
        .incarnation
        .current
        .lock()
        .expect("actual manager incarnation")
        .incarnation = "successor-incarnation".into();
    assert!(manager.validate_capture_claim(&claim).is_err());
    assert!(lease
        .entry
        .native
        .state
        .lock()
        .expect("unentered original file")
        .original
        .is_none());
    drop(claim);
    drop(lease);
    drained(&root).await;
    let detached = manager
        .resource_capture(&row.id)
        .await
        .expect("actual original detach")
        .expect("original history");
    assert_eq!(detached.worker, original_worker);
    assert_ne!(detached.worker, manager.resource_worker());
    assert!(detached.expires_at_ms < expiry);
    assert_eq!(fence.restart_drain_status(0).await.admissions_in_flight, 0);
    root.shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .expect("original worker drain acknowledged");
}
