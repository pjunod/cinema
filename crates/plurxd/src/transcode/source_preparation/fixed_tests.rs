//! Real Source assignment, opened P inputs, actual installed FFprobe/FFmpeg,
//! same-operation replay and original cleanup. Authored/compiled only: running
//! this fixture remains gated on the complete-candidate independent review.
use super::*;
use plurx_core::adaptive_resources::{runtime::NodeResourceRuntime, ResourceVector};
use std::future::Future;

pub(in crate::transcode) async fn source_fixed_planning_fixture(
    runtime: Arc<NodeResourceRuntime>,
    manager: &crate::transcode::TranscodeManager,
    prepared: &crate::http::hls::PreparedSourcePlayback,
    assignment: &SourceDispatchAssignment,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    runtime
        .refresh_admission_capacity(deadline)
        .await
        .expect("actual Linux admission observation");
    let snapshot = Arc::new(
        manager
            .store
            .playback_planning_snapshot(
                prepared.file().id,
                &crate::transcode::QUALITY_PLANNING_KEYS,
            )
            .await
            .expect("actual atomic Source snapshot")
            .expect("Source snapshot"),
    );
    let bytes =
        crate::transcode::adaptive_attempts::planning_snapshot_requested_heap_bytes(&snapshot)
            .expect("snapshot capacities")
            .checked_add(crate::transcode::adaptive_attempts::PLANNING_TO_R_HANDOFF_BYTES)
            .expect("application allowance");
    let request = crate::transcode::adaptive_attempts::fixed_planning_request(
        manager,
        &snapshot,
        prepared.fingerprint(),
        bytes,
    )
    .expect("actual Source P request");
    let operation =
        start_source_fixed_probe(&runtime, request, snapshot, assignment.clone(), deadline)
            .expect("synchronously register the original Source P/M");
    let before = runtime
        .accounting()
        .expect("original admission accounting")
        .used;
    assert_eq!(
        runtime
            .planning_ownership_diagnostics()
            .expect("complete P inventory")
            .native_roles_held,
        1
    );
    assert_eq!(
        runtime
            .planning_ownership_diagnostics()
            .expect("complete M inventory")
            .metadata_owners,
        1
    );
    tokio::time::timeout_at(deadline.into(), operation.prepare_fixed_inputs(deadline))
        .await
        .expect("FAILURE watchdog: original source acquisition hung")
        .expect("open actual original Source inputs under P");

    // Deterministic cancellation of a borrowed daemon waiter after actual
    // source acquisition, before entering the original native collector. The
    // mutex is a fixture delay only; this does NOT claim mid-native coverage.
    let SourceProbeState::Fixed(state) = &operation.state else {
        panic!("fixed original state")
    };
    let original = state.work.lock().await;
    let owner_address = std::ptr::from_ref(&original.owner) as usize;
    let mut waiting = Box::pin(operation.fixed_report(deadline));
    std::future::poll_fn(|cx| {
        assert!(
            waiting.as_mut().poll(cx).is_pending(),
            "the actual owner lock must delay this borrowed waiter"
        );
        std::task::Poll::Ready(())
    })
    .await;
    drop(waiting);
    assert_eq!(
        runtime
            .accounting()
            .expect("retained P/M after waiter drop")
            .used,
        before
    );
    assert_eq!(
        std::ptr::from_ref(&original.owner) as usize,
        owner_address,
        "waiter cancellation cannot replace the original P"
    );
    drop(original);

    let report = tokio::time::timeout_at(deadline.into(), operation.fixed_report(deadline))
        .await
        .expect("FAILURE watchdog: same native collector hung")
        .expect("run and join actual original FFprobe/FFmpeg tasks");
    assert!(report.document().contains("streams"));
    let replay = operation
        .fixed_report(deadline)
        .await
        .expect("cached same-M replay");
    assert!(
        std::ptr::eq(report.document(), replay.document()),
        "replay shares the original report payload"
    );
    assert_eq!(
        runtime
            .planning_ownership_diagnostics()
            .expect("one original owner")
            .metadata_owners,
        1
    );
    assert_eq!(
        runtime
            .planning_ownership_diagnostics()
            .expect("P still held through refinement")
            .native_roles_held,
        1
    );
    let encoding = manager
        .resolve_adaptive_encoding_from_probe(
            prepared.request(),
            prepared.file(),
            operation.fixed_snapshot().expect("same snapshot"),
            report.clone(),
        )
        .await
        .expect("pure Source recipe from the same admitted M");
    assert!(std::ptr::eq(
        report.document(),
        encoding
            .planning_metadata
            .as_ref()
            .expect("Encoding retains original M")
            .document()
    ));
    assert!(
        encoding
            .requested_heap_bytes()
            .expect("actual recipe capacities")
            <= crate::transcode::adaptive_attempts::PLANNING_TO_R_HANDOFF_BYTES
    );
    tokio::time::timeout_at(deadline.into(), operation.finish_fixed_native(deadline))
        .await
        .expect("FAILURE watchdog: original native closer hung")
        .expect("only original seven-member native closure settles P");
    let metadata = runtime.accounting().expect("M after P closes").used;
    assert_ne!(metadata, before);
    assert_ne!(metadata, ResourceVector::default());
    assert_eq!(
        runtime
            .planning_ownership_diagnostics()
            .expect("P close inventory")
            .native_roles_held,
        0
    );
    operation
        .finish_fixed_native(deadline)
        .await
        .expect("idempotent same P closure");
    assert_eq!(
        runtime.accounting().expect("no duplicate refund").used,
        metadata
    );
    assert!(tokio::time::timeout_at(deadline.into(), operation.settle())
        .await
        .expect("FAILURE watchdog: original Source settlement hung")
        .matches(assignment));
    drop(report);
    drop(replay);
    drop(operation);

    let alias = Arc::clone(&encoding);
    drop(encoding);
    // Drive the same bounded pass used by the production manager reaper.
    // The last real Encoding/M alias must still block that actual closer.
    manager.reconcile_adaptive_attempt_tasks().await;
    assert_eq!(
        runtime
            .planning_ownership_diagnostics()
            .expect("actual Encoding alias keeps M")
            .metadata_owners,
        1
    );
    assert_eq!(
        runtime.accounting().expect("no early M refund").used,
        metadata
    );
    drop(alias);
    tokio::time::timeout_at(deadline.into(), async {
        loop {
            // No daemon reap_loop is running in this direct fixture. Retry
            // only the original core book through its real manager cadence.
            manager.reconcile_adaptive_attempt_tasks().await;
            if runtime
                .planning_ownership_diagnostics()
                .is_some_and(|counts| counts.metadata_owners == 0)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("FAILURE watchdog: same original M cleanup did not finish after final alias drop");
    assert_eq!(
        runtime
            .accounting()
            .expect("original cleanup accounting")
            .used,
        ResourceVector::default()
    );
}

/// Dropping an actual native collector waiter must hand the SAME retained
/// selector to original cleanup. A finish-only loop cannot join its pending
/// original task and must fail this fixture's outer FAILURE watchdog.
pub(in crate::transcode) async fn source_native_cancelled_collector_cleanup_fixture(
    runtime: Arc<NodeResourceRuntime>,
    manager: &crate::transcode::TranscodeManager,
    prepared: &crate::http::hls::PreparedSourcePlayback,
    assignment: &SourceDispatchAssignment,
) {
    let deadline = Instant::now() + Duration::from_secs(90);
    runtime
        .refresh_admission_capacity(deadline)
        .await
        .expect("actual admission observation");
    let snapshot = Arc::new(
        manager
            .store
            .playback_planning_snapshot(
                prepared.file().id,
                &crate::transcode::QUALITY_PLANNING_KEYS,
            )
            .await
            .expect("actual Source snapshot")
            .expect("Source snapshot"),
    );
    let bytes =
        crate::transcode::adaptive_attempts::planning_snapshot_requested_heap_bytes(&snapshot)
            .expect("snapshot capacities")
            .checked_add(fixed_source_requested_bytes())
            .expect("original carrier storage")
            .checked_add(crate::transcode::adaptive_attempts::PLANNING_TO_R_HANDOFF_BYTES)
            .expect("handoff allowance");
    let request = crate::transcode::adaptive_attempts::fixed_planning_request(
        manager,
        &snapshot,
        prepared.fingerprint(),
        bytes,
    )
    .expect("original P request");
    let operation =
        start_source_fixed_probe(&runtime, request, snapshot, assignment.clone(), deadline)
            .expect("original P registered");
    operation
        .prepare_fixed_inputs(deadline)
        .await
        .expect("actual original source opened");
    let SourceProbeState::Fixed(state) = &operation.state else {
        panic!("original fixed state")
    };
    let gate = state
        .work
        .lock()
        .await
        .owner
        .as_ref()
        .expect("same original owner")
        .fixture_native_gate(false)
        .await
        .expect("gate only this original engine task");
    let mut collecting = Box::pin(operation.fixed_report(deadline));
    let started = tokio::time::timeout_at(deadline.into(), async {
        tokio::select! {
            started = gate.started() => started,
            early = &mut collecting => panic!("original collector completed before its actual native gate: {}", early.is_ok()),
        }
    }).await.expect("FAILURE watchdog: original native custody not reached");
    assert!(started.pid > 0 && !started.member_id.is_nil() && !started.close_operation_id.is_nil());
    // This says STARTED/CUSTODY only. No PID observation proves continued
    // execution, native absence, completion, or a closed root.
    let held = runtime.accounting().expect("original P/M charge").used;
    drop(collecting);
    operation.cancel();
    let mut closing = Box::pin(operation.settle());
    std::future::poll_fn(|cx| {
        assert!(
            closing.as_mut().poll(cx).is_pending(),
            "the original native task has not returned a close summary"
        );
        std::task::Poll::Ready(())
    })
    .await;
    assert_eq!(
        runtime.accounting().expect("no early partial refund").used,
        held
    );
    gate.release();
    let settled = tokio::time::timeout_at(deadline.into(), closing)
        .await
        .expect(
        "FAILURE watchdog: original Source cleanup did not join cancelled collector's native tasks",
    );
    assert!(settled.matches(assignment));
    drop(settled);
    let counts = runtime
        .planning_ownership_diagnostics()
        .expect("complete original closure inventory");
    assert_eq!(counts.native_roles_held, 0);
    assert_eq!(
        counts.metadata_owners, 1,
        "original application lease keeps M until Source frame disposal"
    );
    assert_ne!(
        runtime.accounting().expect("M still charged").used,
        ResourceVector::default()
    );
    drop(operation);
    drop(gate);
    tokio::time::timeout_at(deadline.into(), async {
        loop {
            runtime.maintain_planning_cleanup(deadline).await;
            if runtime
                .planning_ownership_diagnostics()
                .is_some_and(|counts| counts.metadata_owners == 0)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("FAILURE watchdog: same original M did not close after application drop");
    assert_eq!(
        runtime
            .accounting()
            .expect("original P/M finally retired")
            .used,
        ResourceVector::default()
    );
}

async fn original_source_request(
    runtime: &Arc<NodeResourceRuntime>,
    manager: &crate::transcode::TranscodeManager,
    prepared: &crate::http::hls::PreparedSourcePlayback,
) -> (
    Arc<plurx_core::store::PlaybackPlanningSnapshot>,
    plurx_core::adaptive_resources::runtime::planning_probe::FixedPlanningProbeRequest,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    runtime
        .refresh_admission_capacity(deadline)
        .await
        .expect("actual capacity observation");
    let snapshot = Arc::new(
        manager
            .store
            .playback_planning_snapshot(
                prepared.file().id,
                &crate::transcode::QUALITY_PLANNING_KEYS,
            )
            .await
            .expect("actual atomic snapshot")
            .expect("actual Source snapshot"),
    );
    let bytes =
        crate::transcode::adaptive_attempts::planning_snapshot_requested_heap_bytes(&snapshot)
            .expect("snapshot capacities")
            .checked_add(crate::transcode::adaptive_attempts::PLANNING_TO_R_HANDOFF_BYTES)
            .expect("application allowance");
    let request = crate::transcode::adaptive_attempts::fixed_planning_request(
        manager,
        &snapshot,
        prepared.fingerprint(),
        bytes,
    )
    .expect("actual original P request");
    (snapshot, request)
}

/// Expiry and an actually aged original membership observation refuse before
/// reservation. Waiting here changes no observation or physical owner.
pub(in crate::transcode) async fn source_before_reservation_refusal_fixture(
    runtime: Arc<NodeResourceRuntime>,
    manager: &crate::transcode::TranscodeManager,
    prepared: &crate::http::hls::PreparedSourcePlayback,
    assignment: &SourceDispatchAssignment,
) {
    use plurx_core::adaptive_resources::runtime::direct_source::{
        DirectSourceBinding, DirectSourceRequest,
    };
    use plurx_core::adaptive_resources::WorkloadDemand;
    use plurx_core::process::observation::Metric;
    let fresh_now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("clock range");
    assignment
        .validate_observation_freshness(fresh_now_ms)
        .expect("original assignment observation starts genuinely fresh");
    let (snapshot, request) = original_source_request(&runtime, manager, prepared).await;
    let before = runtime.accounting().expect("baseline").used;
    let failure = start_source_fixed_probe(
        &runtime,
        request,
        snapshot,
        assignment.clone(),
        Instant::now(),
    )
    .err()
    .expect("actual expired constructor");
    assert!(
        failure.into_parts().1.is_none(),
        "exact pre-P tag returns no held operation"
    );
    assert_eq!(runtime.accounting().expect("no expired P/M").used, before);
    // The current production observation maximum age is five seconds. Wait
    // for THIS retained observation to become genuinely stale, then verify
    // the original validator before asking the direct constructor to reject it.
    tokio::time::sleep(Duration::from_secs(6)).await;
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("clock range");
    assert!(
        assignment.validate_observation_freshness(now_ms).is_err(),
        "original observation must actually be stale"
    );
    let file = prepared.file();
    let media_file_id = assignment.binding().file_id().as_str().to_owned();
    let memory = DirectSourceRequest::minimum_memory_bytes(
        file.path.as_os_str().len(),
        media_file_id.len(),
        1,
        4096,
        1,
        0,
    )
    .expect("actual direct requested layout");
    let request = DirectSourceRequest {
        binding: DirectSourceBinding::Sharing(assignment.clone()),
        path: file.path.clone(),
        media_file_id,
        scanner_size: file.size,
        scanner_mtime: file.mtime,
        demand: WorkloadDemand {
            resources: Metric::Known(ResourceVector {
                peak_memory_bytes: memory,
                handles: 1,
                ..ResourceVector::default()
            }),
            limit_epoch: runtime.accounting().expect("same actual epoch").limit_epoch,
            trial: false,
        },
        max_bodies: 1,
        chunk_bytes: 4096,
        outstanding_chunks: 1,
        application_owner_bytes: 0,
    };
    let error = runtime
        .prepare_direct_source(request, Instant::now() + Duration::from_secs(30))
        .err()
        .expect("actual stale observation refusal");
    assert!(error.definitely_before_reservation());
    assert_eq!(
        runtime
            .accounting()
            .expect("no direct R/read-domain reservation")
            .used,
        before
    );
    let counts = runtime
        .planning_ownership_diagnostics()
        .expect("original inventory");
    assert_eq!(counts.native_roles_held, 0);
    assert_eq!(counts.metadata_owners, 0);
}

/// A real one-shot application lease refusal retains its original P selector;
/// dropping a borrowed settlement waiter cannot replace or refund that owner.
pub(in crate::transcode) async fn source_application_lease_refusal_cleanup_fixture(
    runtime: Arc<NodeResourceRuntime>,
    manager: &crate::transcode::TranscodeManager,
    prepared: &crate::http::hls::PreparedSourcePlayback,
    assignment: &SourceDispatchAssignment,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let (snapshot, mut request) = original_source_request(&runtime, manager, prepared).await;
    let recipe_binding = request.recipe_source_binding.clone();
    request.application_metadata_bytes = request
        .application_metadata_bytes
        .checked_add(
            assignment
                .requested_heap_bytes()
                .expect("original assignment capacities"),
        )
        .and_then(|value| value.checked_add(recipe_binding.capacity()))
        .expect("original application layout");
    let requested = request.application_metadata_bytes;
    let owner = runtime
        .prepare_fixed_planning_probe(request, deadline)
        .expect("ONE original P/M");
    let charged = runtime.accounting().expect("original P/M charge").used;
    let application = owner
        .take_application_lease(requested)
        .expect("consume actual original one-shot application partition");
    // Reuse the production application boundary; its second lease attempt is
    // rejected by the actual original state, not a boolean or injected error.
    let failure = retain_source_fixed_application(
        &runtime,
        owner,
        snapshot,
        assignment.clone(),
        recipe_binding,
        requested,
    )
    .err()
    .expect("actual application already-taken refusal");
    let (_, operation) = failure.into_parts();
    let operation = operation.expect("same original P must remain selected");
    let SourceProbeState::PlanningUncertain(state) = &operation.state else {
        panic!("held original state")
    };
    let work_slot = state
        .work
        .as_ref()
        .expect("Some original owner has resumable cleanup");
    let mut original = work_slot.lock().await;
    assert!(original.owner.is_some());
    let counts = runtime
        .planning_ownership_diagnostics()
        .expect("ONE original P/M");
    assert_eq!(counts.native_roles_held, 1);
    assert_eq!(counts.metadata_owners, 1);
    // Invoke the exact synchronous production transfer, while keeping its
    // actual mutex as a deterministic delay before native retirement awaits.
    // This creates no native receipt, task, preparation or replacement owner.
    original
        .select_cleanup()
        .expect("select SAME original cleanup");
    assert!(original.owner.is_none());
    assert!(original.cleanup.is_some());
    let original_slot = std::ptr::from_ref(&original.cleanup) as usize;
    let mut waiting = Box::pin(operation.settle());
    std::future::poll_fn(|cx| {
        assert!(
            waiting.as_mut().poll(cx).is_pending(),
            "actual original lock delays settlement"
        );
        std::task::Poll::Ready(())
    })
    .await;
    drop(waiting);
    assert_eq!(
        runtime
            .accounting()
            .expect("no refund after cancellation")
            .used,
        charged
    );
    assert_eq!(
        std::ptr::from_ref(&original.cleanup) as usize,
        original_slot
    );
    assert!(original.cleanup.is_some());
    drop(original);
    assert!(tokio::time::timeout_at(deadline.into(), operation.settle())
        .await
        .expect("FAILURE watchdog: original no-task cleanup hung")
        .matches(assignment));
    {
        let work = work_slot.lock().await;
        assert!(work.owner.is_none(), "same owner moved into cleanup");
        assert!(work.cleanup.is_some());
        assert!(work.native_closed);
    }
    let counts = runtime
        .planning_ownership_diagnostics()
        .expect("original native cleanup inventory");
    assert_eq!(counts.native_roles_held, 0);
    assert_eq!(
        counts.metadata_owners, 1,
        "actual application alias still holds M"
    );
    let metadata = runtime.accounting().expect("P closed/M held").used;
    assert_ne!(metadata, charged);
    assert_ne!(metadata, ResourceVector::default());
    assert!(tokio::time::timeout_at(deadline.into(), operation.settle())
        .await
        .expect("FAILURE watchdog: same cleanup replay hung")
        .matches(assignment));
    assert_eq!(
        runtime.accounting().expect("no duplicate refund").used,
        metadata
    );
    drop(operation);
    assert_eq!(
        runtime
            .accounting()
            .expect("M held by actual application")
            .used,
        metadata
    );
    drop(application);
    tokio::time::timeout_at(deadline.into(), async {
        loop {
            runtime.maintain_planning_cleanup(deadline).await;
            if runtime
                .planning_ownership_diagnostics()
                .is_some_and(|counts| counts.metadata_owners == 0)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("FAILURE watchdog: final original M cleanup hung");
    assert_eq!(
        runtime.accounting().expect("actual final accounting").used,
        ResourceVector::default()
    );
}
