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
