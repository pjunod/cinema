//! Factory P/R/producer admission only: pre-factory availability IO is outside
//! these assertions. All actual native fixtures are authored, not executed.
use super::*;
use crate::serving_fence::PlannedOutageFenceToken;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_factory_drain_refuses_before_original_p_reservation() {
    Box::pin(source_copy_preadmission_fixture_with_restart(
        13,
        None,
        Some(0),
    ))
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_original_native_cancellation_retains_restart_admission_until_settlement() {
    Box::pin(source_copy_preadmission_fixture_with_restart(
        13,
        None,
        Some(1),
    ))
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_genuine_lost_native_summary_keeps_restart_admission_and_p_charge() {
    Box::pin(source_copy_preadmission_fixture_with_restart(
        15,
        None,
        Some(2),
    ))
    .await;
}

pub(super) async fn before_factory(state: &Arc<crate::state::AppState>) -> PlannedOutageFenceToken {
    state
        .serving
        .begin_restart_preparation_until(crate::state::clock_ms() as u64 + 60_000)
        .await
        .expect("actual local planned-outage fence")
}

pub(super) async fn refused_factory(
    state: &Arc<crate::state::AppState>,
    manager: &Arc<TranscodeManager>,
    assignment: &SourceDispatchAssignment,
    admitted: Result<SourceViewerActor, SourceWorkerNoAdmission>,
    ingress: SourceFactoryIngressFixture,
    fence: PlannedOutageFenceToken,
) {
    let refusal = admitted
        .err()
        .expect("actual factory refused while drain was active");
    assert_eq!(refusal.reason(), SourceWorkerError::Unavailable);
    assert!(refusal.assignment().same_identity(assignment));
    assert!(manager.lookup_source_worker(assignment).is_none());
    let runtime = manager
        .resource_runtime
        .as_ref()
        .expect("actual selected runtime");
    assert_eq!(
        runtime.accounting().expect("no factory P/R").used,
        plurx_core::adaptive_resources::ResourceVector::default()
    );
    assert_eq!(
        state
            .serving
            .restart_drain_status(0)
            .await
            .admissions_in_flight,
        0
    );
    ingress.close().await;
    assert_eq!(
        state
            .store
            .settle_source_assigned_without_activation(assignment)
            .await
            .expect("actual uninvoked assignment settlement"),
        plurx_core::sharing_source_sessions::SourceReleaseOutcome::Released
    );
    state
        .serving
        .cancel_planned_outage_preparation(fence, 0)
        .await;
}

pub(super) async fn after_factory(
    state: &Arc<crate::state::AppState>,
    manager: &Arc<TranscodeManager>,
    actor: &SourceViewerActor,
    assignment: &SourceDispatchAssignment,
    mode: u8,
    case: u8,
    ingress_monitor: SourceFactoryIngressMonitor,
) {
    let gate = tokio::time::timeout(
        Duration::from_secs(30),
        manager.source_workers.probe_hooks.fixed_native_gate(),
    )
    .await
    .expect("FAILURE watchdog: same original native gate missing");
    let started = tokio::time::timeout(Duration::from_secs(30), gate.started())
        .await
        .expect("FAILURE watchdog: actual native custody missing");
    assert!(started.pid > 0); // Custody diagnostic, never liveness/closure proof.
    let charge = manager
        .resource_runtime
        .as_ref()
        .expect("actual selected runtime")
        .accounting()
        .expect("same original charge")
        .used;
    assert!(tokio::time::timeout(
        Duration::from_millis(30),
        actor.wait_ready(Instant::now() + Duration::from_secs(30))
    )
    .await
    .is_err());
    let fence = before_factory(state).await;
    let blocked = state.serving.restart_drain_status(0).await;
    assert_eq!(blocked.admissions_in_flight, 1);
    assert!(!blocked.drained);
    assert!(state.serving.try_restart_admission().await.is_none());
    assert_eq!(
        manager
            .resource_runtime
            .as_ref()
            .expect("same selected Source runtime after waiter cancellation")
            .accounting()
            .expect("same original P/M accounting after waiter cancellation")
            .used,
        charge
    );
    // Reuse the authentic original-owner native closure / real JoinError body.
    // It releases this exact gate and never substitutes IDs for native proof.
    fixed_native::exercise(state, manager, actor, assignment, mode, ingress_monitor).await;
    let status = state.serving.restart_drain_status(0).await;
    if case == 1 {
        assert_eq!(
            status.admissions_in_flight, 0,
            "exact successful Source settlement disarms"
        );
        assert!(status.drained);
    } else {
        assert_eq!(case, 2);
        assert_eq!(
            status.admissions_in_flight, 1,
            "genuine lost summary retains original custody"
        );
        assert!(!status.drained);
        assert_eq!(actor.settlement_status(), None);
        assert!(manager.lookup_source_worker(assignment).is_some());
    }
    state
        .serving
        .cancel_planned_outage_preparation(fence, 0)
        .await;
}
